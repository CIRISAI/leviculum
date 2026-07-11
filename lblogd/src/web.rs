//! The clearnet web server: serves the blog as HTML over HTTP and HTTPS.
//!
//! HTTPS certificates come from Let's Encrypt automatically via `rustls-acme`
//! using the TLS-ALPN-01 challenge, which rides the HTTPS listener itself:
//! the ACME validation connection arrives on port 443 with a special ALPN
//! value and is answered inside the TLS acceptor, so no challenge plumbing
//! exists at the HTTP layer. Certificates and the account key are cached in a
//! persistent directory so restarts and renewals do not re-register.
//!
//! The plain-HTTP listener does exactly one thing: 301-redirect every request
//! to the `https://` equivalent.
//!
//! Posts are loaded once at startup and rendered per request from the cached
//! [`Post`]s; a reload-on-change mechanism is deliberately out of scope here
//! (batch D wires the daemon and can add it).

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::StreamExt;
use rustls_acme::caches::DirCache;
use rustls_acme::AcmeConfig;
use thiserror::Error;

use crate::post::{load_posts_dir, Post, PostError};
use crate::render::{render_index_html, render_post_html};

/// Errors from starting or running the web server.
#[derive(Debug, Error)]
pub enum WebError {
    /// Loading the posts directory failed.
    #[error("loading posts: {0}")]
    Posts(#[from] PostError),
    /// The config lists no domains to obtain a certificate for.
    #[error("no domains configured")]
    NoDomains,
    /// Binding a listen address failed.
    #[error("binding {addr}: {source}")]
    Bind {
        /// The address that failed to bind.
        addr: SocketAddr,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The HTTP redirect server failed.
    #[error("http server: {0}")]
    Http(std::io::Error),
    /// The HTTPS server failed.
    #[error("https server: {0}")]
    Https(std::io::Error),
    /// The ACME certificate state stream ended (it is designed to run
    /// forever, so this indicates a bug in the ACME layer).
    #[error("acme certificate state stream ended unexpectedly")]
    AcmeEnded,
}

/// Configuration for [`run_web`].
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// Domains the certificate covers; the first one doubles as the redirect
    /// target when a request carries no Host header.
    pub domains: Vec<String>,
    /// Persistent directory caching the ACME account key and certificates.
    /// Losing it forces re-issuance on every start, which burns Let's
    /// Encrypt rate limits.
    pub acme_cache_dir: PathBuf,
    /// Contact email for the ACME account (expiry warnings and the like).
    pub acme_contact_email: String,
    /// Use the Let's Encrypt STAGING directory instead of production.
    /// Staging issues untrusted certificates but has generous rate limits;
    /// production is a deliberate config choice for real deployments.
    pub acme_staging: bool,
    /// Plain-HTTP listen address (normally port 80), redirect-only.
    pub http_bind: SocketAddr,
    /// HTTPS listen address (normally port 443). TLS-ALPN-01 validation
    /// requires the ACME server to reach the certificate domains on port
    /// 443, so in real deployments this must be reachable there.
    pub https_bind: SocketAddr,
    /// Directory of Markdown posts to serve.
    pub posts_dir: PathBuf,
}

/// Build the blog router: `/` is the post index, `/posts/{slug}` one post,
/// everything else a small HTML 404.
pub fn build_router(posts: Arc<Vec<Post>>) -> Router {
    Router::new()
        .route("/", get(index_page))
        .route("/posts/{slug}", get(post_page))
        .fallback(fallback_page)
        .with_state(posts)
}

async fn index_page(State(posts): State<Arc<Vec<Post>>>) -> Html<String> {
    Html(render_index_html(&posts))
}

async fn post_page(
    State(posts): State<Arc<Vec<Post>>>,
    UrlPath(slug): UrlPath<String>,
) -> Response {
    match posts.iter().find(|p| p.slug == slug) {
        Some(post) => Html(render_post_html(post)).into_response(),
        None => not_found(),
    }
}

async fn fallback_page() -> Response {
    not_found()
}

fn not_found() -> Response {
    const BODY: &str = "<!doctype html>\n<html lang=\"en\">\n<head><meta charset=\"utf-8\">\
                        <title>404 Not Found</title></head>\n\
                        <body><h1>404 Not Found</h1></body>\n</html>\n";
    (StatusCode::NOT_FOUND, Html(BODY)).into_response()
}

/// Serve the blog over HTTPS with automatic Let's Encrypt certificates, plus
/// a plain-HTTP listener that 301-redirects to HTTPS. Runs until one of the
/// servers fails.
///
/// The certificate acquisition path (rustls-acme against Let's Encrypt) is
/// compile-verified only: it needs a publicly reachable domain, so it is
/// exercised in real deployment, not in CI.
pub async fn run_web(config: WebConfig) -> Result<(), WebError> {
    if config.domains.is_empty() {
        return Err(WebError::NoDomains);
    }
    let posts = Arc::new(load_posts_dir(&config.posts_dir)?);
    let router = build_router(posts);

    let mut acme_state = AcmeConfig::new(&config.domains)
        .contact_push(format!("mailto:{}", config.acme_contact_email))
        .cache(DirCache::new(config.acme_cache_dir.clone()))
        .directory_lets_encrypt(!config.acme_staging)
        .state();
    let acceptor = acme_state.axum_acceptor(acme_state.default_rustls_config());
    // The state stream drives ACME ordering and renewal; it must be polled
    // for the acceptor to ever have a certificate. It never terminates.
    let acme_driver = async move {
        while let Some(event) = acme_state.next().await {
            match event {
                Ok(ok) => eprintln!("lblogd: acme: {ok:?}"),
                Err(err) => eprintln!("lblogd: acme error: {err:?}"),
            }
        }
    };

    let https_server = axum_server::bind(config.https_bind)
        .acceptor(acceptor)
        .serve(router.into_make_service());

    let redirect = redirect_router(config.https_bind.port(), config.domains[0].clone());
    let http_listener = tokio::net::TcpListener::bind(config.http_bind)
        .await
        .map_err(|source| WebError::Bind {
            addr: config.http_bind,
            source,
        })?;
    let http_server = axum::serve(http_listener, redirect.into_make_service());

    tokio::select! {
        result = https_server => result.map_err(WebError::Https),
        result = http_server.into_future() => result.map_err(WebError::Http),
        () = acme_driver => Err(WebError::AcmeEnded),
    }
}

/// The redirect-only router served on the plain-HTTP listener: every request
/// gets a 301 to its `https://` equivalent.
fn redirect_router(https_port: u16, fallback_host: String) -> Router {
    Router::new().fallback(move |headers: HeaderMap, uri: Uri| {
        let target = redirect_target(&headers, &uri, https_port, &fallback_host);
        async move { (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, target)]) }
    })
}

/// The `https://` URL a plain-HTTP request redirects to: the request's Host
/// (sans port, falling back to the first configured domain), the HTTPS port
/// unless it is the default 443, and the original path and query.
fn redirect_target(headers: &HeaderMap, uri: &Uri, https_port: u16, fallback_host: &str) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(host_without_port)
        .filter(|h| !h.is_empty())
        .unwrap_or(fallback_host);
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    if https_port == 443 {
        format!("https://{host}{path}")
    } else {
        format!("https://{host}:{https_port}{path}")
    }
}

/// Strip a `:port` suffix from a Host header value, keeping IPv6 literals
/// (`[::1]:80` becomes `[::1]`) intact.
fn host_without_port(host: &str) -> &str {
    if let Some(end) = host.rfind(']') {
        return &host[..=end];
    }
    match host.rfind(':') {
        Some(idx) => &host[..idx],
        None => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn sample_posts() -> Arc<Vec<Post>> {
        Arc::new(vec![
            Post {
                title: "Hello World".to_string(),
                date: "2026-07-02".parse().unwrap(),
                slug: "hello-world".to_string(),
                body_md: "First **post** body.".to_string(),
            },
            Post {
                title: "Older Post".to_string(),
                date: "2026-06-30".parse().unwrap(),
                slug: "older-post".to_string(),
                body_md: "Nothing to see.".to_string(),
            },
        ])
    }

    async fn get(router: Router, path: &str) -> (StatusCode, HeaderMap, String) {
        let response = router
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn index_lists_posts_with_links() {
        let (status, headers, body) = get(build_router(sample_posts()), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(body.contains("Hello World"));
        assert!(body.contains("/posts/hello-world"));
        assert!(body.contains("/posts/older-post"));
    }

    #[tokio::test]
    async fn post_page_renders_body() {
        let (status, headers, body) = get(build_router(sample_posts()), "/posts/hello-world").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert!(body.contains("Hello World"));
        assert!(body.contains("<strong>post</strong>"));
    }

    #[tokio::test]
    async fn unknown_slug_is_404() {
        let (status, _, body) = get(build_router(sample_posts()), "/posts/does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("404"));
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let (status, _, body) = get(build_router(sample_posts()), "/random").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("404"));
    }

    #[tokio::test]
    async fn redirect_router_301s_to_https() {
        let response = redirect_router(443, "blog.example".to_string())
            .oneshot(
                Request::builder()
                    .uri("/posts/hello-world?x=1")
                    .header(header::HOST, "blog.example:80")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
        assert_eq!(
            response.headers()[header::LOCATION],
            "https://blog.example/posts/hello-world?x=1"
        );
    }

    #[test]
    fn redirect_target_shapes() {
        let uri: Uri = "/a/b?q=1".parse().unwrap();
        let mut headers = HeaderMap::new();

        // No Host header: fall back to the configured domain.
        assert_eq!(
            redirect_target(&headers, &uri, 443, "blog.example"),
            "https://blog.example/a/b?q=1"
        );

        // Host with port: port stripped, default HTTPS port omitted.
        headers.insert(header::HOST, "other.example:8080".parse().unwrap());
        assert_eq!(
            redirect_target(&headers, &uri, 443, "blog.example"),
            "https://other.example/a/b?q=1"
        );

        // Non-default HTTPS port is appended.
        assert_eq!(
            redirect_target(&headers, &uri, 8443, "blog.example"),
            "https://other.example:8443/a/b?q=1"
        );

        // IPv6 literal keeps its brackets, loses its port.
        headers.insert(header::HOST, "[::1]:8080".parse().unwrap());
        assert_eq!(
            redirect_target(&headers, &uri, 443, "blog.example"),
            "https://[::1]/a/b?q=1"
        );
    }

    #[test]
    fn host_without_port_shapes() {
        assert_eq!(host_without_port("example.com"), "example.com");
        assert_eq!(host_without_port("example.com:80"), "example.com");
        assert_eq!(host_without_port("[::1]"), "[::1]");
        assert_eq!(host_without_port("[::1]:8080"), "[::1]");
    }
}
