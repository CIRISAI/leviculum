//! The single source of served content, and the reload path that replaces it.
//!
//! Both the NomadNet node and the web server render from the same
//! [`Snapshot`], published over a [`tokio::sync::watch`] channel. That the
//! two sides share one load is not just convenience: loading the directory
//! twice lets them disagree when a post is written between the two reads.
//!
//! [`Reloader::reload`] swaps a whole new snapshot in atomically. A failed
//! load leaves the previous one in place, so a malformed post never takes a
//! running server down. That is the deliberate difference from startup, where
//! the same error is fatal: at startup there is no good state to fall back on,
//! and serving nothing is better than serving something the author did not
//! write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::watch;

use crate::post::{load_posts_dir, parse_post, Post, PostDefaults, PostError};
use crate::render::{
    default_about_title, render_about_micron, render_index_micron, render_post_micron, BlogMeta,
    DEFAULT_STYLE,
};

/// The request path of the blog's index page.
pub const INDEX_PATH: &str = "/page/index.mu";

/// The request path of the blog's about page.
pub const ABOUT_PATH: &str = "/page/about.mu";

/// Errors from building a snapshot.
#[derive(Debug, Error)]
pub enum ContentError {
    /// Loading the posts directory failed.
    #[error("loading posts: {0}")]
    Posts(#[from] PostError),
    /// Encoding a page as msgpack failed.
    #[error("page encoding: {0}")]
    Encode(String),
    /// The configured stylesheet could not be read.
    #[error("reading stylesheet {path}: {source}")]
    Css {
        /// The configured stylesheet path.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// One consistent view of the blog: the parsed posts and the Micron pages
/// rendered from exactly those posts.
///
/// The pages are pre-rendered and pre-encoded because the node answers
/// requests from them directly; the web server renders HTML per request from
/// [`posts`](Self::posts), which is cheap enough and keeps the HTML path free
/// of a second cache to invalidate.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// The blog's identity, rendered into every page. Constant across
    /// reloads, but carried here so a page can be rendered from a snapshot
    /// alone.
    pub meta: BlogMeta,
    /// Parsed posts, newest first.
    pub posts: Vec<Post>,
    /// Rendered pages by request path, each already encoded as the single
    /// msgpack bin value the response APIs expect.
    pub pages: HashMap<String, Vec<u8>>,
    /// The stylesheet inlined into every HTML page: the operator's file, or
    /// the built-in default. Part of the snapshot so it reloads with the
    /// posts.
    pub css: String,
    /// The about page's text, when a file is configured. Parsed exactly like
    /// a post, but never listed, never dated and never in the feed.
    pub about: Option<Post>,
}

impl Snapshot {
    /// The request paths this snapshot serves, sorted.
    pub fn served_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.pages.keys().cloned().collect();
        paths.sort();
        paths
    }
}

/// Read the posts and the stylesheet, and render every page.
pub fn load_snapshot(
    meta: &BlogMeta,
    posts_dir: &Path,
    css_path: Option<&Path>,
    about_path: Option<&Path>,
) -> Result<Snapshot, ContentError> {
    let posts = load_posts_dir(posts_dir)?;
    let about = about_path.map(|path| load_about(meta, path)).transpose()?;
    let pages = build_pages(meta, &posts, about.as_ref())?;
    let css = match css_path {
        Some(path) => std::fs::read_to_string(path).map_err(|source| ContentError::Css {
            path: path.display().to_string(),
            source,
        })?,
        None => DEFAULT_STYLE.to_string(),
    };
    Ok(Snapshot {
        meta: meta.clone(),
        posts,
        pages,
        css,
        about,
    })
}

/// Read the about text, which is a post file in every respect except that
/// nothing dates or lists it.
///
/// Its title defaults to the author's name rather than to the file name: an
/// about page headed "about" would tell a reader nothing they did not already
/// know from clicking a name.
fn load_about(meta: &BlogMeta, path: &Path) -> Result<Post, ContentError> {
    let source = std::fs::read_to_string(path).map_err(|source| PostError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let defaults = PostDefaults {
        title: default_about_title(meta.author.as_deref()),
        date: PostDefaults::for_file(path)?.date,
    };
    let post = parse_post(&source, &defaults).map_err(|e| PostError::File {
        path: path.to_path_buf(),
        source: Box::new(e),
    })?;
    Ok(post)
}

/// The receiving end of the content channel, held by the node and the web
/// server. Cloneable, so any number of consumers can read the current
/// snapshot.
pub type SnapshotRx = watch::Receiver<Arc<Snapshot>>;

/// Owns the publishing end of the content channel.
///
/// Kept alive for the process lifetime: dropping it closes the channel, which
/// consumers treat as "no more reloads", not as an error.
pub struct Reloader {
    tx: watch::Sender<Arc<Snapshot>>,
    meta: BlogMeta,
    posts_dir: PathBuf,
    css_path: Option<PathBuf>,
    about_path: Option<PathBuf>,
}

impl Reloader {
    /// Load `posts_dir` once and open the channel with the result. A failure
    /// here is fatal to startup by design; see the module docs.
    pub fn new(
        meta: BlogMeta,
        posts_dir: &Path,
        css_path: Option<&Path>,
        about_path: Option<&Path>,
    ) -> Result<(Reloader, SnapshotRx), ContentError> {
        let snapshot = Arc::new(load_snapshot(&meta, posts_dir, css_path, about_path)?);
        let (tx, rx) = watch::channel(snapshot);
        Ok((
            Reloader {
                tx,
                meta,
                posts_dir: posts_dir.to_path_buf(),
                css_path: css_path.map(Path::to_path_buf),
                about_path: about_path.map(Path::to_path_buf),
            },
            rx,
        ))
    }

    /// Re-read the posts directory and publish the result.
    ///
    /// On error nothing is published and the previous snapshot stays live, so
    /// a typo in a post cannot take the server offline. The error names the
    /// offending file and is the caller's to log.
    pub fn reload(&self) -> Result<usize, ContentError> {
        let snapshot = load_snapshot(
            &self.meta,
            &self.posts_dir,
            self.css_path.as_deref(),
            self.about_path.as_deref(),
        )?;
        let count = snapshot.posts.len();
        // send() only fails when every receiver is gone, which means both
        // servers have stopped; there is nothing useful to do about it here.
        let _ = self.tx.send(Arc::new(snapshot));
        Ok(count)
    }
}

/// Render every page and encode each as the single msgpack bin value the
/// response APIs expect (the `[request_id, response]` wrapper is added by
/// `send_response`/`send_response_resource` internally).
fn build_pages(
    meta: &BlogMeta,
    posts: &[Post],
    about: Option<&Post>,
) -> Result<HashMap<String, Vec<u8>>, ContentError> {
    let mut pages = HashMap::new();
    pages.insert(
        INDEX_PATH.to_string(),
        msgpack_bin(render_index_micron(meta, posts).as_bytes())?,
    );
    // The about page exists whenever there is anything to put on it, which
    // may be contact details alone with no text file.
    if meta.has_about {
        pages.insert(
            ABOUT_PATH.to_string(),
            msgpack_bin(render_about_micron(meta, about).as_bytes())?,
        );
    }
    for post in posts {
        pages.insert(
            post_page_path(post),
            msgpack_bin(render_post_micron(meta, post).as_bytes())?,
        );
    }
    Ok(pages)
}

/// The request path a post's page is served under.
pub fn post_page_path(post: &Post) -> String {
    format!("/page/{}.mu", post.slug)
}

/// Encode bytes as one msgpack bin value, the page response payload contract
/// NomadNet clients decode.
fn msgpack_bin(data: &[u8]) -> Result<Vec<u8>, ContentError> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Binary(data.to_vec()))
        .map_err(|e| ContentError::Encode(e.to_string()))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal metadata: these tests are about loading, not rendering.
    fn meta() -> BlogMeta {
        BlogMeta {
            title: "Test Blog".to_string(),
            language: "en".to_string(),
            ..BlogMeta::default()
        }
    }

    fn write_post(dir: &Path, name: &str, title: &str, date: &str) {
        std::fs::write(
            dir.join(name),
            format!("+++\ntitle = \"{title}\"\ndate = \"{date}\"\n+++\n\nBody.\n"),
        )
        .unwrap();
    }

    #[test]
    fn snapshot_has_one_page_per_post_plus_the_index() {
        let dir = tempfile::tempdir().unwrap();
        write_post(dir.path(), "a.md", "First", "2026-07-01");
        write_post(dir.path(), "b.md", "Second", "2026-07-02");

        let snapshot = load_snapshot(&meta(), dir.path(), None, None).unwrap();
        assert_eq!(snapshot.posts.len(), 2);
        assert_eq!(
            snapshot.served_paths(),
            vec!["/page/first.mu", "/page/index.mu", "/page/second.mu"]
        );
    }

    #[test]
    fn reload_publishes_the_new_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        write_post(dir.path(), "a.md", "First", "2026-07-01");
        let (reloader, mut rx) = Reloader::new(meta(), dir.path(), None, None).unwrap();
        assert_eq!(rx.borrow_and_update().posts.len(), 1);

        write_post(dir.path(), "b.md", "Second", "2026-07-02");
        assert_eq!(reloader.reload().unwrap(), 2);

        assert!(rx.has_changed().unwrap(), "consumers must see the change");
        let snapshot = rx.borrow_and_update();
        assert_eq!(snapshot.posts.len(), 2);
        assert!(snapshot.pages.contains_key("/page/second.mu"));
    }

    #[test]
    fn failed_reload_keeps_the_previous_snapshot_live() {
        // The whole point of reloading rather than restarting: a typo must
        // not take the running server down.
        let dir = tempfile::tempdir().unwrap();
        write_post(dir.path(), "a.md", "First", "2026-07-01");
        let (reloader, mut rx) = Reloader::new(meta(), dir.path(), None, None).unwrap();
        rx.borrow_and_update();

        std::fs::write(
            dir.path().join("broken.md"),
            "+++\ntitle = \"Broken\"\ndate = \"2026-13-45\"\n+++\n\nBody.\n",
        )
        .unwrap();
        let err = reloader.reload().unwrap_err();
        assert!(err.to_string().contains("broken.md"), "{err}");

        assert!(
            !rx.has_changed().unwrap(),
            "a failed reload must publish nothing"
        );
        let snapshot = rx.borrow();
        assert_eq!(snapshot.posts.len(), 1);
        assert!(snapshot.pages.contains_key("/page/first.mu"));
    }

    #[test]
    fn reload_drops_pages_of_deleted_posts() {
        let dir = tempfile::tempdir().unwrap();
        write_post(dir.path(), "a.md", "First", "2026-07-01");
        write_post(dir.path(), "b.md", "Second", "2026-07-02");
        let (reloader, mut rx) = Reloader::new(meta(), dir.path(), None, None).unwrap();
        rx.borrow_and_update();

        std::fs::remove_file(dir.path().join("b.md")).unwrap();
        reloader.reload().unwrap();

        let snapshot = rx.borrow_and_update();
        assert!(!snapshot.pages.contains_key("/page/second.mu"));
        assert_eq!(
            snapshot.served_paths(),
            vec!["/page/first.mu", "/page/index.mu"]
        );
    }
}
