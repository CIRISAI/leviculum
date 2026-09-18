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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::watch;

use crate::files::{load_files_dir, FileArea, FileEntry, FilesError};
use crate::post::{load_posts_dir, parse_post, Post, PostDefaults, PostError};
use crate::render::{
    default_about_title, render_about_micron, render_index_micron, render_link_micron,
    render_page_micron, render_post_micron, BlogMeta, NavEntry, DEFAULT_STYLE, INDEX_MICRON_TARGET,
};
use crate::site::{load_site, LinkSpec, Site, SiteError};

/// The request path of the site's root page: the post index, or the landing
/// page once `pages_dir` holds an `index.md`.
pub const INDEX_PATH: &str = "/page/index.mu";

/// The request path the post index moves to once a landing page has taken
/// [`INDEX_PATH`].
pub const BLOG_PATH: &str = "/page/blog.mu";

/// The request path of the blog's about page.
pub const ABOUT_PATH: &str = "/page/about.mu";

/// The request path a static page or a link is served under on the mesh.
pub fn page_path(name: &str) -> String {
    format!("/page/{name}.mu")
}

/// The micron request target a static page or a link is linked to.
fn page_target(name: &str) -> String {
    format!(":/page/{name}.mu")
}

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
    /// The file area could not be read.
    #[error("{0}")]
    Files(#[from] FilesError),
    /// The static pages or the links are not usable: a bad name, a bad URL,
    /// or a name something else already answers under.
    #[error("{0}")]
    Site(#[from] SiteError),
}

/// Where a snapshot's content comes from.
///
/// A struct rather than a growing list of positional arguments: three of the
/// four members are optional, and `load(&meta, dir, None, None, None)` says
/// nothing about which `None` is which.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    /// Directory of Markdown posts.
    pub posts_dir: PathBuf,
    /// The operator's stylesheet, or `None` for the built-in one.
    pub css_path: Option<PathBuf>,
    /// The about page's text file, if there is one.
    pub about_path: Option<PathBuf>,
    /// The file area, if the blog has one.
    pub files: Option<FileArea>,
    /// The directory of static pages, if the blog has one.
    pub pages_dir: Option<PathBuf>,
    /// The `[links]` entries, in the order the config lists them.
    pub links: Vec<LinkSpec>,
}

impl Sources {
    /// Posts only: no stylesheet, no about text, no file area.
    pub fn new(posts_dir: impl Into<PathBuf>) -> Self {
        Sources {
            posts_dir: posts_dir.into(),
            ..Sources::default()
        }
    }

    /// Serve the operator's stylesheet instead of the built-in one.
    pub fn with_css(mut self, css_path: Option<impl Into<PathBuf>>) -> Self {
        self.css_path = css_path.map(Into::into);
        self
    }

    /// Render the about page from this text file.
    pub fn with_about(mut self, about_path: Option<impl Into<PathBuf>>) -> Self {
        self.about_path = about_path.map(Into::into);
        self
    }

    /// Serve this file area alongside the pages.
    pub fn with_files(mut self, files: Option<FileArea>) -> Self {
        self.files = files;
        self
    }

    /// Serve the static pages in this directory.
    pub fn with_pages(mut self, pages_dir: Option<impl Into<PathBuf>>) -> Self {
        self.pages_dir = pages_dir.map(Into::into);
        self
    }

    /// Serve these links, in this order.
    pub fn with_links(mut self, links: Vec<LinkSpec>) -> Self {
        self.links = links;
        self
    }

    /// Every path a change should trigger a reload for: the post directory,
    /// the file area, the pages directory, and the two single files.
    pub fn watch_paths(&self) -> Vec<&Path> {
        let mut paths = vec![self.posts_dir.as_path()];
        if let Some(files) = &self.files {
            paths.push(files.dir.as_path());
        }
        paths.extend(self.pages_dir.as_deref());
        paths.extend(self.css_path.as_deref());
        paths.extend(self.about_path.as_deref());
        paths
    }
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
    /// The servable files, keyed by served name. Both sides answer from this
    /// one map, so the mesh and the web can never disagree about what exists.
    ///
    /// Entries carry a path and a length, not the bytes: a file is read when
    /// a request asks for it (see [`crate::files`]).
    pub files: BTreeMap<String, FileEntry>,
    /// The file area the entries came from, carried so a handler can re-check
    /// the size ceiling when it reads.
    pub file_area: Option<FileArea>,
    /// The static pages and links: what the site says besides the posts.
    pub site: Site,
}

impl Snapshot {
    /// The page request paths this snapshot serves, sorted.
    pub fn served_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.pages.keys().cloned().collect();
        paths.sort();
        paths
    }

    /// The file request paths this snapshot serves, sorted. Separate from
    /// [`served_paths`](Self::served_paths) because the two are answered
    /// differently on the wire: a page is a msgpack response, a file is a raw
    /// Resource with metadata.
    pub fn served_file_paths(&self) -> Vec<String> {
        self.files
            .keys()
            .map(|name| crate::files::node_path(name))
            .collect()
    }

    /// The entry a `/file/<name>` request names, if it exists.
    pub fn file_for_node_path(&self, path: &str) -> Option<&FileEntry> {
        let name = path.strip_prefix(crate::files::NODE_PREFIX)?;
        self.files.get(&crate::files::sanitize_name(name)?)
    }
}

/// Read the posts, the stylesheet and the file area, and render every page.
pub fn load_snapshot(meta: &BlogMeta, sources: &Sources) -> Result<Snapshot, ContentError> {
    let posts = load_posts_dir(&sources.posts_dir)?;
    let about = sources
        .about_path
        .as_deref()
        .map(|path| load_about(meta, path))
        .transpose()?;
    // The collision check needs the posts, so the site is loaded after them
    // and before anything is rendered: a name clash must be an error, not a
    // page that quietly shadows a post.
    let site = load_site(
        &meta.title,
        sources.pages_dir.as_deref(),
        &sources.links,
        &posts,
        meta.has_about,
    )?;
    // The nav and the moved post index are facts about the loaded site, not
    // about the config, so the meta the pages are rendered from is derived
    // here rather than passed in. Both sides then render from one list and
    // cannot disagree about where a page lives.
    let meta = &with_site(meta, &site);
    let pages = build_pages(meta, &posts, about.as_ref(), &site)?;
    let css = match sources.css_path.as_deref() {
        Some(path) => std::fs::read_to_string(path).map_err(|source| ContentError::Css {
            path: path.display().to_string(),
            source,
        })?,
        None => DEFAULT_STYLE.to_string(),
    };
    let files = match &sources.files {
        Some(area) => load_files_dir(area)?,
        None => BTreeMap::new(),
    };
    Ok(Snapshot {
        meta: meta.clone(),
        posts,
        pages,
        css,
        about,
        files,
        file_area: sources.files.clone(),
        site,
    })
}

/// The blog's metadata as the loaded site makes it: where the post index
/// lives, and what the nav line offers.
///
/// The nav stays empty when the only thing to point at is the post index
/// itself, which is the state of every blog that configures neither
/// `pages_dir` nor `[links]`. An empty nav renders to nothing, so those blogs
/// serve byte-for-byte what they served before any of this existed.
fn with_site(meta: &BlogMeta, site: &Site) -> BlogMeta {
    let mut meta = meta.clone();
    meta.has_landing = site.has_landing();
    meta.nav = match site.is_empty() {
        true => Vec::new(),
        false => build_nav(&meta, site),
    };
    meta
}

/// The nav line: the landing page, the blog, then the static pages by name
/// and the links in config order.
///
/// Pages before links, because a directory has no order to honour and the
/// config file does: sorting the pages and then following the file keeps the
/// one order the operator actually wrote.
fn build_nav(meta: &BlogMeta, site: &Site) -> Vec<NavEntry> {
    let mut nav = Vec::new();
    if let Some(landing) = &site.landing {
        nav.push(NavEntry {
            label: landing.title.clone(),
            web: "/".to_string(),
            micron: INDEX_MICRON_TARGET.to_string(),
        });
    }
    nav.push(NavEntry {
        label: "Blog".to_string(),
        web: meta.index_html_path().to_string(),
        micron: meta.index_micron_target().to_string(),
    });
    for page in &site.pages {
        nav.push(NavEntry {
            label: page.page.title.clone(),
            web: format!("/{}", page.name),
            micron: page_target(&page.name),
        });
    }
    for link in &site.links {
        nav.push(NavEntry {
            label: link.label(),
            web: format!("/{}", link.name),
            micron: page_target(&link.name),
        });
    }
    nav
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
    sources: Sources,
}

impl Reloader {
    /// Load the sources once and open the channel with the result. A failure
    /// here is fatal to startup by design; see the module docs.
    pub fn new(meta: BlogMeta, sources: Sources) -> Result<(Reloader, SnapshotRx), ContentError> {
        let snapshot = Arc::new(load_snapshot(&meta, &sources)?);
        let (tx, rx) = watch::channel(snapshot);
        Ok((Reloader { tx, meta, sources }, rx))
    }

    /// The sources this reloader reads from, for the caller that has to watch
    /// them.
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    /// Re-read the posts directory and publish the result.
    ///
    /// On error nothing is published and the previous snapshot stays live, so
    /// a typo in a post cannot take the server offline. The error names the
    /// offending file and is the caller's to log.
    pub fn reload(&self) -> Result<usize, ContentError> {
        let snapshot = load_snapshot(&self.meta, &self.sources)?;
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
    site: &Site,
) -> Result<HashMap<String, Vec<u8>>, ContentError> {
    let mut pages = HashMap::new();
    // The post index keeps `/page/index.mu` unless a landing page has taken
    // it, in which case it moves to `/page/blog.mu` and the landing page
    // answers the root. Posts themselves never move: a feed entry is
    // identified by its URL.
    let index_path = match site.has_landing() {
        true => BLOG_PATH,
        false => INDEX_PATH,
    };
    pages.insert(
        index_path.to_string(),
        msgpack_bin(render_index_micron(meta, posts).as_bytes())?,
    );
    if let Some(landing) = &site.landing {
        pages.insert(
            INDEX_PATH.to_string(),
            msgpack_bin(render_page_micron(meta, landing).as_bytes())?,
        );
    }
    for page in &site.pages {
        pages.insert(
            page_path(&page.name),
            msgpack_bin(render_page_micron(meta, &page.page).as_bytes())?,
        );
    }
    for link in &site.links {
        pages.insert(
            page_path(&link.name),
            msgpack_bin(
                render_link_micron(meta, &link.label(), &link.url, link.page.as_ref()).as_bytes(),
            )?,
        );
    }
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

        let snapshot = load_snapshot(&meta(), &Sources::new(dir.path())).unwrap();
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
        let (reloader, mut rx) = Reloader::new(meta(), Sources::new(dir.path())).unwrap();
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
        let (reloader, mut rx) = Reloader::new(meta(), Sources::new(dir.path())).unwrap();
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
    fn snapshot_carries_the_file_area_and_reload_tracks_it() {
        let posts = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        write_post(posts.path(), "a.md", "First", "2026-07-01");
        std::fs::write(files.path().join("antenne.png"), b"\x89PNG stub").unwrap();

        let sources = Sources::new(posts.path()).with_files(Some(FileArea::new(files.path())));
        let (reloader, mut rx) = Reloader::new(meta(), sources).unwrap();
        {
            let snapshot = rx.borrow_and_update();
            assert_eq!(snapshot.served_file_paths(), vec!["/file/antenne.png"]);
            assert_eq!(
                snapshot
                    .file_for_node_path("/file/antenne.png")
                    .unwrap()
                    .len,
                9
            );
            // The traversal guard applies to the lookup, not just the load.
            assert!(snapshot
                .file_for_node_path("/file/../../etc/passwd")
                .is_none());
            assert!(snapshot.file_for_node_path("/page/index.mu").is_none());
        }

        std::fs::remove_file(files.path().join("antenne.png")).unwrap();
        std::fs::write(files.path().join("mast.jpg"), b"jpeg stub").unwrap();
        reloader.reload().unwrap();

        let snapshot = rx.borrow_and_update();
        assert_eq!(snapshot.served_file_paths(), vec!["/file/mast.jpg"]);
    }

    #[test]
    fn a_missing_file_area_is_not_an_error() {
        let posts = tempfile::tempdir().unwrap();
        write_post(posts.path(), "a.md", "First", "2026-07-01");
        let sources =
            Sources::new(posts.path()).with_files(Some(FileArea::new(posts.path().join("gone"))));

        let snapshot = load_snapshot(&meta(), &sources).unwrap();
        assert!(snapshot.files.is_empty());
        assert!(snapshot.served_file_paths().is_empty());
    }

    #[test]
    fn reload_drops_pages_of_deleted_posts() {
        let dir = tempfile::tempdir().unwrap();
        write_post(dir.path(), "a.md", "First", "2026-07-01");
        write_post(dir.path(), "b.md", "Second", "2026-07-02");
        let (reloader, mut rx) = Reloader::new(meta(), Sources::new(dir.path())).unwrap();
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
