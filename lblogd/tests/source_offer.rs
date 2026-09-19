//! The source offer AGPL section 13 requires, on every page of both sides.
//!
//! lblogd is AGPL software whose whole purpose is to be reachable over a
//! network, so every reader of a served page is a user "interacting with it
//! remotely through a computer network" in the sense of section 13. The offer
//! is therefore not a feature an operator switches on: it is on by default,
//! and the only thing configurable about it is *where* the source is, because
//! an operator who modified lblogd has to offer their own tree rather than
//! ours.
//!
//! The licence name and the default URL are spelled out here rather than read
//! from `env!("CARGO_PKG_LICENSE")` and `env!("CARGO_PKG_REPOSITORY")`: what
//! these tests pin is what a reader is told, and a test that derives it from
//! the same metadata the renderer does would follow any change silently.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use lblogd::config::Config;
use lblogd::content::{load_snapshot, Reloader, INDEX_PATH};
use lblogd::post::{Date, Post};
use lblogd::render::{
    render_about_html, render_about_micron, render_index_html, render_index_micron,
    render_page_html, render_page_micron, render_post_html, render_post_micron, BlogMeta,
    DEFAULT_STYLE,
};
use lblogd::web::build_router;
use leviculum_micron::parse;

/// The licence lblogd is under, as a reader must be told it.
const LICENSE: &str = "AGPL-3.0-or-later";

/// Where the Corresponding Source is when the operator names no other place.
const UPSTREAM: &str = "https://codeberg.org/Lew_Palm/leviculum";

/// A blog configured with nothing but a name — the case that decides whether
/// the offer is default-on or merely available.
fn bare_meta() -> BlogMeta {
    BlogMeta {
        title: "Bare Blog".to_string(),
        language: "en".to_string(),
        ..BlogMeta::default()
    }
}

fn post() -> Post {
    Post {
        title: "Hallo Mesh".to_string(),
        date: "2026-07-01".parse::<Date>().expect("fixture date"),
        author: None,
        slug: "hallo".to_string(),
        body_md: "Erster Text.\n".to_string(),
    }
}

fn page() -> Post {
    Post {
        title: "Impressum".to_string(),
        date: "2026-07-01".parse::<Date>().expect("fixture date"),
        author: None,
        slug: "impressum".to_string(),
        body_md: "Wer hier schreibt.\n".to_string(),
    }
}

/// Every HTML page a reader can reach names the licence and links the source.
#[test]
fn the_web_side_offers_the_source_on_every_page() {
    let meta = bare_meta();
    let pages = [
        ("index", render_index_html(&meta, DEFAULT_STYLE, &[post()])),
        ("post", render_post_html(&meta, DEFAULT_STYLE, &post())),
        ("about", render_about_html(&meta, DEFAULT_STYLE, None)),
        ("page", render_page_html(&meta, DEFAULT_STYLE, &page())),
    ];
    for (name, html) in pages {
        assert!(
            html.contains(LICENSE),
            "{name} must name the licence: {html}"
        );
        assert!(
            html.contains(&format!("href=\"{UPSTREAM}\"")),
            "{name} must link the source: {html}"
        );
        assert!(
            html.contains("<footer>"),
            "{name} must carry the footer that holds it: {html}"
        );
    }
}

/// And so does every micron page: a NomadNet reader interacts with the same
/// program over the same kind of network.
#[test]
fn the_mesh_side_offers_the_source_on_every_page() {
    let meta = bare_meta();
    let pages = [
        ("index", render_index_micron(&meta, &[post()])),
        ("post", render_post_micron(&meta, &post())),
        ("about", render_about_micron(&meta, None)),
        ("page", render_page_micron(&meta, &page())),
    ];
    for (name, micron) in pages {
        assert!(
            micron.contains(LICENSE),
            "{name} must name the licence: {micron}"
        );
        assert!(
            micron.contains(UPSTREAM),
            "{name} must name the source URL: {micron}"
        );
        // The URL is plain text, not a `[label`target] link: micron link
        // targets are Reticulum paths, and a NomadNet client cannot follow a
        // web address.
        assert!(
            !micron.contains(&format!("`{UPSTREAM}]")),
            "{name} must not pretend the URL is a micron link: {micron}"
        );
        assert!(
            !parse(&micron).blocks.is_empty(),
            "{name} must stay parseable micron: {micron}"
        );
    }
}

/// The version of the running binary, so the offer names the tree that
/// corresponds to it rather than "whatever is at the top of the repository".
#[test]
fn the_offer_names_the_running_version() {
    let html = render_index_html(&bare_meta(), DEFAULT_STYLE, &[post()]);
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        html.contains(&format!("lblogd {version}")),
        "the offer must name the build it corresponds to: {html}"
    );
}

/// A config file that says nothing about source at all gets the upstream
/// offer, which is what makes it default-on rather than opt-in.
const SAMPLE: &str = "\
data_dir  = \"/var/lib/lblogd\"
posts_dir = \"/var/lib/lblogd/posts\"

[blog]
title = \"Bare Blog\"

[node]
instance_name = \"default\"

[web]
acme      = false
http_bind = \"127.0.0.1:8180\"
";

fn load(text: &str) -> Result<Config, lblogd::config::ConfigError> {
    let dir = tempfile::tempdir().expect("config dir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, text).expect("write config");
    Config::load(&path)
}

#[test]
fn an_unconfigured_config_offers_the_upstream_source() {
    assert!(!SAMPLE.contains("[source]"));
    let meta = load(SAMPLE).expect("load").blog_meta(None);
    assert_eq!(meta.source.url, UPSTREAM);
    assert_eq!(meta.source.license, LICENSE);
}

/// An operator who modified lblogd must be able to point at their own tree:
/// section 13 obliges them to offer *their* source, not ours.
#[test]
fn the_operator_can_point_the_offer_at_their_own_tree() {
    let text = format!("{SAMPLE}\n[source]\nurl = \"https://git.example.org/me/lblogd\"\n");
    let meta = load(&text).expect("load").blog_meta(None);
    assert_eq!(meta.source.url, "https://git.example.org/me/lblogd");

    let html = render_index_html(&meta, DEFAULT_STYLE, &[post()]);
    assert!(
        html.contains("https://git.example.org/me/lblogd"),
        "the page must offer the operator's tree: {html}"
    );
    assert!(
        !html.contains(UPSTREAM),
        "and must not still offer ours: {html}"
    );
}

/// An empty URL is refused at startup rather than served as a footer that
/// offers nothing: it is the one way to configure a page that claims an
/// offer and does not make one.
#[test]
fn an_empty_source_url_is_a_config_error() {
    let text = format!("{SAMPLE}\n[source]\nurl = \"\"\n");
    let err = load(&text).expect_err("empty url must be refused");
    assert!(err.to_string().contains("source.url"), "{err}");
}

/// The end-to-end path, from a config file nobody wrote a `[source]` section
/// into to the bytes each side hands the reader.
///
/// The renderer tests above pin the offer as a function of a `BlogMeta`; this
/// one pins that a deployed blog actually builds such a `BlogMeta` and serves
/// it. Both sides answer from the same snapshot, so both are checked here:
/// the web response body, and the msgpack payload the node puts on the wire.
#[tokio::test]
async fn a_configured_blog_serves_the_offer_on_both_sides() {
    let posts = tempfile::tempdir().expect("posts dir");
    std::fs::write(
        posts.path().join("hallo.md"),
        "+++\ntitle = \"Hallo Mesh\"\ndate = \"2026-07-01\"\nslug = \"hallo\"\n+++\n\nErster Text.\n",
    )
    .expect("write post");

    let text = SAMPLE.replace(
        "posts_dir = \"/var/lib/lblogd/posts\"",
        &format!("posts_dir = {:?}", posts.path()),
    );
    let config = load(&text).expect("load");
    let meta = config.blog_meta(None);
    let snapshot = load_snapshot(&meta, &config.content_sources()).expect("load snapshot");

    // The web side, through the real router.
    let (reloader, content) =
        Reloader::new(meta.clone(), config.content_sources()).expect("initial load");
    std::mem::forget(reloader);
    let router = build_router(content);
    for path in ["/", "/posts/hallo"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8(body.to_vec()).expect("utf-8");
        assert!(body.contains(LICENSE), "{path} served no licence: {body}");
        assert!(
            body.contains(&format!("href=\"{UPSTREAM}\"")),
            "{path} served no source link: {body}"
        );
    }

    // The mesh side, as the node hands it to the wire.
    for path in [INDEX_PATH, "/page/hallo.mu"] {
        let bytes = snapshot
            .pages
            .get(path)
            .unwrap_or_else(|| panic!("no page at {path}; has {:?}", snapshot.served_paths()));
        let value = rmpv::decode::read_value(&mut std::io::Cursor::new(&bytes[..]))
            .unwrap_or_else(|e| panic!("{path} is not msgpack: {e}"));
        let rmpv::Value::Binary(data) = value else {
            panic!("{path} must be a msgpack bin value, got {value:?}");
        };
        let page = String::from_utf8(data).expect("utf-8");
        assert!(page.contains(LICENSE), "{path} served no licence: {page}");
        assert!(
            page.contains(UPSTREAM),
            "{path} served no source URL: {page}"
        );
    }
}
