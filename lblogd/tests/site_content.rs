//! The pages leviculum.network ships (`site/pages/` in the repo, installed
//! by the lblogd package under `/usr/share/lblogd/pages/`) load, render on
//! both sides, and link only to things that answer.
//!
//! The pages are content, not fixtures, so these tests read the real files.
//! What they hold the content to is what lblogd will do with it on the
//! server: a landing page whose four links land, sibling pages that reach
//! each other on the web and on the mesh, no page that takes a name the
//! server or the deployed about page already answers under, and a board
//! page that names exactly the boards the firmware catalogue names.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

use lblogd::content::{load_snapshot, Reloader, Snapshot, Sources, INDEX_PATH};
use lblogd::render::BlogMeta;
use lblogd::site::{ABOUT_NAME, RESERVED};
use lblogd::web::build_router;

/// `site/pages/` in this checkout.
fn pages_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../site/pages")
}

/// The firmware board catalogue the board page summarises.
fn catalogue_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/src/firmware/boards.md")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The page names in `site/pages/`, by file stem, sorted.
fn page_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(pages_dir())
        .unwrap_or_else(|e| panic!("read {}: {e}", pages_dir().display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

/// The deployment's shape: a blog with an author and an about page, which
/// is what reserves `about` and what the landing page's byline links to.
fn meta() -> BlogMeta {
    BlogMeta {
        title: "Leviculum Dev Blog".to_string(),
        author: Some("Lew Palm".to_string()),
        language: "en".to_string(),
        email: Some("lew@leviculum.network".to_string()),
        has_about: true,
        ..BlogMeta::default()
    }
}

/// An empty posts directory and the real pages.
fn sources(posts: &Path) -> Sources {
    Sources::new(posts).with_pages(Some(pages_dir()))
}

fn snapshot() -> Snapshot {
    let posts = tempfile::tempdir().expect("posts dir");
    load_snapshot(&meta(), &sources(posts.path())).expect("site/pages loads")
}

/// The micron text the snapshot serves under `path`.
fn served_micron(snapshot: &Snapshot, path: &str) -> String {
    let bytes = snapshot
        .pages
        .get(path)
        .unwrap_or_else(|| panic!("no page at {path}; has {:?}", snapshot.served_paths()));
    let value = rmpv::decode::read_value(&mut std::io::Cursor::new(&bytes[..]))
        .unwrap_or_else(|e| panic!("{path} is not msgpack: {e}"));
    let rmpv::Value::Binary(data) = value else {
        panic!("{path} must be a msgpack bin value, got {value:?}");
    };
    String::from_utf8(data).unwrap_or_else(|e| panic!("{path} is not UTF-8: {e}"))
}

/// Every Markdown link destination in `md`, in order, with its label.
fn markdown_links(md: &str) -> Vec<(String, String)> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut links = Vec::new();
    let mut open: Option<(String, String)> = None;
    for event in Parser::new(md) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                open = Some((String::new(), dest_url.to_string()));
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((label, _)) = open.as_mut() {
                    label.push_str(&t);
                }
            }
            Event::End(TagEnd::Link) => links.extend(open.take()),
            _ => {}
        }
    }
    links
}

/// Every link target in a micron page: the part after the second backtick
/// of each `` `[label`target] ``.
fn micron_targets(micron: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = micron;
    while let Some(start) = rest.find("`[") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(']') else { break };
        let inner = &rest[..end];
        if let Some((_, target)) = inner.split_once('`') {
            targets.push(target.to_string());
        }
        rest = &rest[end + 1..];
    }
    targets
}

fn router() -> Router {
    let posts = tempfile::tempdir().expect("posts dir");
    let (reloader, content) = Reloader::new(meta(), sources(posts.path())).expect("initial load");
    // Both live for the router's lifetime, as in the daemon. The posts
    // directory is only read at load, but a reload must not find it gone.
    std::mem::forget(reloader);
    std::mem::forget(posts);
    build_router(content)
}

async fn status_of(router: &Router, path: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
        .status()
}

async fn body_of(router: &Router, path: &str) -> String {
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
    let body = to_bytes(response.into_body(), 1 << 20).await.expect("body");
    String::from_utf8_lossy(&body).into_owned()
}

#[test]
fn the_site_has_the_four_pages_and_a_landing_page() {
    assert_eq!(page_names(), ["boards", "download", "index", "source"]);
    let snapshot = snapshot();
    for path in [
        INDEX_PATH,
        "/page/blog.mu",
        "/page/boards.mu",
        "/page/download.mu",
        "/page/source.mu",
    ] {
        assert!(
            snapshot.pages.contains_key(path),
            "{path} missing; served: {:?}",
            snapshot.served_paths()
        );
    }
}

#[test]
fn no_page_takes_a_reserved_name() {
    // load_site already refuses a collision; this says which names the
    // deployment holds back, so a page added later fails here by name rather
    // than at the server's next reload.
    for name in page_names() {
        assert!(
            !RESERVED.iter().any(|r| r.name == name),
            "{name}.md takes a reserved name"
        );
        assert_ne!(name, ABOUT_NAME, "the deployment has an about page");
    }
}

#[test]
fn every_page_renders_on_the_mesh_and_parses_as_micron() {
    let snapshot = snapshot();
    for name in page_names() {
        let path = match name.as_str() {
            "index" => INDEX_PATH.to_string(),
            other => format!("/page/{other}.mu"),
        };
        let micron = served_micron(&snapshot, &path);
        let doc = leviculum_micron::parse(&micron);
        assert!(
            doc.blocks.len() > 3,
            "{path} renders to almost nothing: {micron}"
        );
        // Every same-node link must land on something this node serves.
        for target in micron_targets(&micron) {
            let Some(local) = target.strip_prefix(':') else {
                continue;
            };
            assert!(
                snapshot.pages.contains_key(local),
                "{path} links to {target}, which this node does not serve; served: {:?}",
                snapshot.served_paths()
            );
        }
        // No web path may leak into a mesh page as a link target.
        assert!(
            !micron_targets(&micron).iter().any(|t| t.starts_with('/')),
            "{path} carries a web path as a mesh link: {micron}"
        );
    }
}

#[tokio::test]
async fn every_page_renders_on_the_web() {
    let router = router();
    for name in page_names() {
        let path = match name.as_str() {
            "index" => "/".to_string(),
            other => format!("/{other}"),
        };
        let body = body_of(&router, &path).await;
        assert!(body.contains("<h1>"), "{path}: {body}");
    }
}

#[tokio::test]
async fn the_landing_page_carries_its_four_links_and_they_land() {
    let index = read(&pages_dir().join("index.md"));
    let links = markdown_links(&index);
    let router = router();
    for label in ["Download", "Documentation", "Source", "Blog"] {
        let (_, url) = links
            .iter()
            .find(|(l, _)| l == label)
            .unwrap_or_else(|| panic!("index.md has no {label:?} link: {links:?}"));
        if url.starts_with('/') {
            assert_eq!(
                status_of(&router, url).await,
                StatusCode::OK,
                "{label} -> {url}"
            );
        } else {
            // External; `just site-check` fetches these when it is online.
            assert!(url.starts_with("https://"), "{label} -> {url}");
        }
    }
}

#[tokio::test]
async fn every_site_link_on_every_page_lands() {
    let router = router();
    for name in page_names() {
        let md = read(&pages_dir().join(format!("{name}.md")));
        for (label, url) in markdown_links(&md) {
            if url.starts_with('/') {
                assert_eq!(
                    status_of(&router, &url).await,
                    StatusCode::OK,
                    "{name}.md: [{label}]({url})"
                );
            } else {
                assert!(
                    url.starts_with("https://"),
                    "{name}.md: [{label}]({url}) is neither a site path nor https"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_landing_page_names_its_author_and_a_contact() {
    let body = body_of(&router(), "/").await;
    assert!(
        body.contains("by <a href=\"/about\">Lew Palm</a>"),
        "{body}"
    );
    assert!(body.contains("lew@leviculum.network"), "{body}");
}

/// The product rows of every `| Product | Level | Note |` table in the
/// catalogue, as (name, level) with the level's bold markers removed.
fn catalogue_boards(catalogue: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut in_table = false;
    for line in catalogue.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            in_table = false;
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.first() == Some(&"Product") && cells.get(1) == Some(&"Level") {
            in_table = true;
            continue;
        }
        if !in_table || cells.iter().all(|c| c.chars().all(|ch| ch == '-')) {
            continue;
        }
        if let [name, level, ..] = cells[..] {
            rows.push((name.to_string(), level.trim_matches('*').to_string()));
        }
    }
    rows
}

/// The boards the page lists on a line of their own, as `- **Name**: ...`.
fn page_board_lines(page: &str) -> BTreeSet<String> {
    page.lines()
        .filter_map(|l| l.strip_prefix("- **"))
        .filter_map(|l| l.split_once("**").map(|(name, _)| name.to_string()))
        .collect()
}

/// The levels that get a line of their own on the page; every other level
/// is named in a summary list.
const LISTED_LEVELS: [&str; 3] = ["Verified", "Bring-up", "Skeleton"];

#[test]
fn the_board_page_and_the_catalogue_name_the_same_boards() {
    let catalogue = catalogue_boards(&read(&catalogue_path()));
    assert!(
        catalogue.len() > 10,
        "the catalogue parser found almost nothing: {catalogue:?}"
    );
    let page = read(&pages_dir().join("boards.md"));

    let listed: BTreeSet<String> = catalogue
        .iter()
        .filter(|(_, level)| LISTED_LEVELS.contains(&level.as_str()))
        .map(|(name, _)| name.clone())
        .collect();
    assert_eq!(
        page_board_lines(&page),
        listed,
        "boards.md lists a board on its own line iff the catalogue rates it {LISTED_LEVELS:?}"
    );
    for (name, level) in &catalogue {
        assert!(
            page.contains(name.as_str()),
            "the catalogue names {name:?} ({level}), site/pages/boards.md does not"
        );
    }
    // And the level each listed line claims is the catalogue's.
    for (name, level) in catalogue
        .iter()
        .filter(|(_, l)| LISTED_LEVELS.contains(&l.as_str()))
    {
        let line = page
            .lines()
            .find(|l| l.starts_with(&format!("- **{name}**")))
            .unwrap_or_default();
        assert!(
            line.contains(&format!(" {level}:")),
            "{name} is {level} in the catalogue; the page says: {line}"
        );
    }
}

#[test]
fn the_package_installs_the_pages_where_the_sample_config_points() {
    // Three files name one directory: the package manifest installs the
    // pages there, the shipped config tells the operator to point pages_dir
    // there, and this test. A move in one without the others ships a config
    // whose suggested line names an empty or missing directory.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = read(&root.join("Cargo.toml"));
    assert!(
        manifest.contains(r#"["../site/pages/*.md", "usr/share/lblogd/pages/", "644"]"#),
        "lblogd/Cargo.toml no longer installs site/pages to /usr/share/lblogd/pages"
    );
    let config = read(&root.join("../packaging/lblogd/config.toml"));
    assert!(
        config
            .lines()
            .any(|l| l.trim() == r#"# pages_dir = "/usr/share/lblogd/pages""#),
        "packaging/lblogd/config.toml no longer points pages_dir at the shipped pages"
    );
}
