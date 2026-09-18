//! Static pages, the landing page and links: what each serves on both sides,
//! which names they may not take, and what stays exactly as it was without
//! them.
//!
//! The mesh assertions read the page out of the snapshot's msgpack bin value,
//! which is the byte-for-byte payload the node hands to the wire, so a page
//! that renders correctly but is filed under the wrong path still fails.

use std::path::Path;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use tower::ServiceExt;

use lblogd::content::{
    load_snapshot, page_path, Reloader, Snapshot, Sources, ABOUT_PATH, BLOG_PATH, INDEX_PATH,
};
use lblogd::render::BlogMeta;
use lblogd::site::LinkSpec;
use lblogd::web::build_router;
use leviculum_micron::{parse, Block};

/// A blog with a name and nothing else: these tests are about the site's
/// shape, not about its identity.
fn meta() -> BlogMeta {
    BlogMeta {
        title: "leviculum.network".to_string(),
        language: "en".to_string(),
        ..BlogMeta::default()
    }
}

/// The same blog with an about page, which reserves the name `about`.
fn meta_with_about() -> BlogMeta {
    BlogMeta {
        email: Some("lp@lew-palm.de".to_string()),
        has_about: true,
        ..meta()
    }
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap_or_else(|e| panic!("write {name}: {e}"));
}

fn link(name: &str, url: &str) -> LinkSpec {
    LinkSpec {
        name: name.to_string(),
        url: url.to_string(),
    }
}

/// A posts directory holding one post, and a pages directory.
fn fixture_dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    let posts = tempfile::tempdir().expect("posts dir");
    let pages = tempfile::tempdir().expect("pages dir");
    // The slug is pinned, because half of these tests are about a page
    // taking a post's name and the name has to be predictable.
    write(
        posts.path(),
        "hallo.md",
        "+++\ntitle = \"Hallo Mesh\"\ndate = \"2026-07-01\"\nslug = \"hallo\"\n+++\n\nErster Text.\n",
    );
    (posts, pages)
}

/// The micron text the snapshot serves under `path`, unwrapped from the
/// msgpack bin value the response API expects.
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

/// A router over a snapshot built from these sources.
fn router_over(meta: BlogMeta, sources: Sources) -> Router {
    let (reloader, content) = Reloader::new(meta, sources).expect("initial load");
    // The reloader owns the sending half; leaking it keeps the channel open
    // for the router's lifetime, which is exactly what the daemon does.
    std::mem::forget(reloader);
    build_router(content)
}

async fn get(router: &Router, path: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .expect("request");
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1 << 20).await.expect("body");
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

#[test]
fn a_landing_page_takes_the_root_and_the_post_index_moves_down() {
    let (posts, pages) = fixture_dirs();
    write(
        pages.path(),
        "index.md",
        "+++\ntitle = \"Leviculum\"\n+++\n\nEine Reticulum-Implementierung.\n",
    );

    let snapshot = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_pages(Some(pages.path())),
    )
    .expect("load");

    let root = served_micron(&snapshot, INDEX_PATH);
    assert!(
        root.contains(">Leviculum"),
        "the landing page heads /: {root}"
    );
    assert!(
        root.contains("Eine Reticulum-Implementierung"),
        "with its own text: {root}"
    );
    assert!(
        !root.contains(":/page/hallo.mu"),
        "the post list moved off the root: {root}"
    );

    let blog = served_micron(&snapshot, BLOG_PATH);
    assert!(
        blog.contains(":/page/hallo.mu"),
        "the post index moved to /page/blog.mu: {blog}"
    );
}

#[tokio::test]
async fn the_web_side_moves_with_it() {
    let (posts, pages) = fixture_dirs();
    write(pages.path(), "index.md", "Willkommen hier.\n");
    let router = router_over(
        meta(),
        Sources::new(posts.path()).with_pages(Some(pages.path())),
    );

    let (status, _, body) = get(&router, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Willkommen hier."), "{body}");
    assert!(!body.contains("/posts/hallo"), "not the post index: {body}");

    let (status, _, body) = get(&router, "/blog").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("/posts/hallo"), "the post index: {body}");

    // A post's way back goes to the list it was found in, which moved.
    let (status, _, body) = get(&router, "/posts/hallo").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<a href=\"/blog\">"), "{body}");
}

#[tokio::test]
async fn without_an_index_page_nothing_moves() {
    // The compatibility case: pages exist, but the root is still the posts.
    let (posts, pages) = fixture_dirs();
    write(pages.path(), "impressum.md", "Angaben nach TMG.\n");

    let snapshot = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_pages(Some(pages.path())),
    )
    .expect("load");
    assert_eq!(
        snapshot.served_paths(),
        vec!["/page/hallo.mu", "/page/impressum.mu", "/page/index.mu"],
        "the post index keeps /page/index.mu and nothing sits at /page/blog.mu"
    );
    assert!(
        served_micron(&snapshot, INDEX_PATH).contains(":/page/hallo.mu"),
        "the root is still the post index"
    );

    let router = router_over(
        meta(),
        Sources::new(posts.path()).with_pages(Some(pages.path())),
    );
    let (status, _, body) = get(&router, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("/posts/hallo"), "{body}");
    let (status, _, _) = get(&router, "/blog").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "/blog exists only once a landing page has taken /"
    );
    let (status, _, body) = get(&router, "/impressum").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Angaben nach TMG."), "{body}");
}

#[tokio::test]
async fn a_link_redirects_on_the_web_and_names_its_url_on_the_mesh() {
    const URL: &str = "https://codeberg.org/Lew_Palm/leviculum";
    let (posts, _pages) = fixture_dirs();
    let sources = Sources::new(posts.path()).with_links(vec![link("code", URL)]);

    let snapshot = load_snapshot(&meta(), &sources).expect("load");
    let page = served_micron(&snapshot, &page_path("code"));
    assert!(page.contains(URL), "the mesh page names the URL: {page}");
    assert!(
        !page.contains(&format!("`{URL}]")),
        "not as a micron link: a NomadNet client cannot follow one: {page}"
    );

    let router = router_over(meta(), sources);
    let (status, headers, _) = get(&router, "/code").await;
    assert_eq!(
        status,
        StatusCode::FOUND,
        "302, not 301: a forge moves and a cached 301 never expires"
    );
    assert_eq!(headers[header::LOCATION], URL);
}

#[tokio::test]
async fn a_page_of_the_same_name_rides_along_on_the_mesh_only() {
    const URL: &str = "https://codeberg.org/Lew_Palm/leviculum";
    let (posts, pages) = fixture_dirs();
    write(
        pages.path(),
        "code.md",
        "+++\ntitle = \"Code\"\n+++\n\nKlonen: `git clone`.\n",
    );
    let sources = Sources::new(posts.path())
        .with_pages(Some(pages.path()))
        .with_links(vec![link("code", URL)]);

    let snapshot = load_snapshot(&meta(), &sources).expect("load");
    let page = served_micron(&snapshot, &page_path("code"));
    let body_at = page.find("Klonen").expect("the page body is rendered");
    let url_at = page.find(URL).expect("the URL is named");
    assert!(body_at < url_at, "the body goes above the URL: {page}");

    let router = router_over(meta(), sources);
    let (status, headers, body) = get(&router, "/code").await;
    assert_eq!(status, StatusCode::FOUND, "the redirect still wins: {body}");
    assert_eq!(headers[header::LOCATION], URL);
}

#[test]
fn a_page_renders_to_micron_our_own_parser_accepts() {
    let (posts, pages) = fixture_dirs();
    write(
        pages.path(),
        "code.md",
        "+++\ntitle = \"Der Code\"\n+++\n\nEin **Absatz** und eine [Liste](https://example.org):\n\n- eins\n- zwei\n",
    );
    let snapshot = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_pages(Some(pages.path())),
    )
    .expect("load");

    let doc = parse(&served_micron(&snapshot, &page_path("code")));
    let blocks: Vec<&Block> = doc
        .blocks
        .iter()
        .filter(|b| !matches!(b, Block::Blank))
        .collect();
    // The nav line comes first, then the page's own title.
    assert!(
        matches!(blocks.get(1), Some(Block::Heading { depth: 1, .. })),
        "a page carries its title under the nav: {blocks:?}"
    );
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph { line, .. }
                if line.spans.iter().any(|s| s.text.contains("Absatz")))),
        "{blocks:?}"
    );
}

#[test]
fn a_link_page_renders_to_micron_our_own_parser_accepts() {
    let (posts, _pages) = fixture_dirs();
    let snapshot = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_links(vec![link("issues", "https://example.org/i")]),
    )
    .expect("load");

    let doc = parse(&served_micron(&snapshot, &page_path("issues")));
    let blocks: Vec<&Block> = doc
        .blocks
        .iter()
        .filter(|b| !matches!(b, Block::Blank))
        .collect();
    assert!(
        matches!(blocks.get(1), Some(Block::Heading { depth: 1, .. })),
        "{blocks:?}"
    );
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph { line, .. }
            if line.spans.iter().any(|s| s.text.contains("example.org/i")))),
        "the URL survives as plain text: {blocks:?}"
    );
}

#[tokio::test]
async fn the_nav_line_offers_every_destination_on_both_sides() {
    let (posts, pages) = fixture_dirs();
    write(pages.path(), "index.md", "Willkommen.\n");
    write(
        pages.path(),
        "impressum.md",
        "+++\ntitle = \"Impressum\"\n+++\n\nTMG.\n",
    );
    let sources = Sources::new(posts.path())
        .with_pages(Some(pages.path()))
        .with_links(vec![
            link("issues", "https://example.org/i"),
            link("code", "https://example.org/c"),
        ]);

    let snapshot = load_snapshot(&meta(), &sources).expect("load");
    let labels: Vec<&str> = snapshot
        .meta
        .nav
        .iter()
        .map(|entry| entry.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "leviculum.network", // the landing page, titled after the blog
            "Blog",
            "Impressum",
            "Issues", // links in config order, not alphabetically
            "Code",
        ]
    );

    // Every micron page carries it, posts included.
    for path in [INDEX_PATH, BLOG_PATH, "/page/hallo.mu", "/page/code.mu"] {
        let page = served_micron(&snapshot, path);
        assert!(
            page.contains("`[Impressum`:/page/impressum.mu]"),
            "{path} has no nav: {page}"
        );
        assert!(page.contains("`[Blog`:/page/blog.mu]"), "{path}: {page}");
    }

    let router = router_over(meta(), sources);
    for path in ["/", "/blog", "/posts/hallo", "/impressum"] {
        let (status, _, body) = get(&router, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains("<nav class=\"site-nav\">"), "{path}: {body}");
        assert!(
            body.contains("<a href=\"/impressum\">Impressum</a>"),
            "{path}"
        );
        assert!(body.contains("<a href=\"/code\">Code</a>"), "{path}");
    }
}

/// Each collision class, as a page name and as a link name: an error at
/// startup that names the offender.
#[test]
fn every_collision_class_is_a_startup_error() {
    let cases: [(&str, &str); 5] = [
        ("hallo", "the post slug"),
        ("blog", "the post index"),
        ("posts", "the post route"),
        ("files", "the file area"),
        ("feed.xml", "the Atom feed"),
    ];
    for (name, what) in cases {
        let (posts, pages) = fixture_dirs();
        write(pages.path(), &format!("{name}.md"), "Kollision.\n");
        let err = load_snapshot(
            &meta(),
            &Sources::new(posts.path()).with_pages(Some(pages.path())),
        )
        .expect_err("a page may not take a name something else answers under");
        assert!(err.to_string().contains(name), "{what}: {err}");

        let err = load_snapshot(
            &meta(),
            &Sources::new(posts.path()).with_links(vec![link(name, "https://example.org")]),
        )
        .expect_err("nor may a link");
        assert!(err.to_string().contains(name), "{what}: {err}");
    }

    // `index` is the landing page as a page name and forbidden as a link.
    let (posts, _pages) = fixture_dirs();
    let err = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_links(vec![link("index", "https://example.org")]),
    )
    .expect_err("a link may not take the root");
    assert!(err.to_string().contains("landing page"), "{err}");
}

#[test]
fn about_collides_only_once_an_about_page_is_configured() {
    let (posts, pages) = fixture_dirs();
    write(pages.path(), "about.md", "Über mich.\n");
    let sources = Sources::new(posts.path()).with_pages(Some(pages.path()));

    let snapshot = load_snapshot(&meta(), &sources).expect("free without an about page");
    assert!(snapshot.served_paths().contains(&page_path("about")));

    let err = load_snapshot(&meta_with_about(), &sources).expect_err("taken with one");
    assert!(err.to_string().contains("about"), "{err}");
    // And the about page itself is unharmed where no page claims the name.
    let snapshot = load_snapshot(&meta_with_about(), &Sources::new(posts.path())).expect("load");
    assert!(snapshot.served_paths().contains(&ABOUT_PATH.to_string()));
}

#[test]
fn a_page_name_that_is_not_a_slug_names_the_file() {
    let (posts, pages) = fixture_dirs();
    write(pages.path(), "Über Uns.md", "Text.\n");
    let err = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_pages(Some(pages.path())),
    )
    .expect_err("a page name must be plain lowercase ASCII");
    assert!(err.to_string().contains("Über Uns"), "{err}");
}

#[test]
fn a_link_url_must_be_absolute_and_the_error_names_the_key() {
    let (posts, _pages) = fixture_dirs();
    let err = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_links(vec![link("code", "/code")]),
    )
    .expect_err("a relative target would point back at this server");
    assert!(err.to_string().contains("code"), "{err}");
    assert!(err.to_string().contains("/code"), "{err}");
}

#[test]
fn a_collision_introduced_later_keeps_the_old_content_serving() {
    // The same guarantee a malformed post date has: startup is fatal, a
    // running server is not.
    let (posts, pages) = fixture_dirs();
    write(pages.path(), "impressum.md", "Angaben nach TMG.\n");
    let (reloader, mut rx) = Reloader::new(
        meta(),
        Sources::new(posts.path()).with_pages(Some(pages.path())),
    )
    .expect("initial load");
    rx.borrow_and_update();

    // A page that takes the post's name on the mesh.
    write(pages.path(), "hallo.md", "Kollision.\n");
    let err = reloader
        .reload()
        .expect_err("the collision must be refused");
    assert!(err.to_string().contains("hallo"), "{err}");

    assert!(
        !rx.has_changed().expect("channel open"),
        "a failed reload must publish nothing"
    );
    let snapshot = rx.borrow();
    assert!(
        snapshot.served_paths().contains(&page_path("impressum")),
        "the previous site is still being served: {:?}",
        snapshot.served_paths()
    );
    assert!(
        served_micron(&snapshot, &page_path("hallo")).contains("Erster Text."),
        "and the post still owns the name the page tried to take"
    );
}

#[test]
fn the_dry_run_names_every_page_and_link() {
    // `--print-hash` is the publishing dry run: what it lists is what the
    // node would register a request handler for, so a page or a link missing
    // here is a page or a link nobody on the mesh can reach.
    let data = tempfile::tempdir().expect("data dir");
    let (posts, pages) = fixture_dirs();
    write(pages.path(), "index.md", "Willkommen.\n");
    write(pages.path(), "impressum.md", "TMG.\n");
    let config = data.path().join("lblogd.toml");
    std::fs::write(
        &config,
        format!(
            "data_dir  = {:?}\nposts_dir = {:?}\npages_dir = {:?}\n\n\
             [links]\ncode = \"https://example.org/c\"\n\n\
             [blog]\ntitle = \"leviculum.network\"\n\n\
             [node]\ninstance_name = \"lblogd-test\"\n\n\
             [web]\nacme = false\n",
            data.path(),
            posts.path(),
            pages.path()
        ),
    )
    .expect("write config");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lblogd"))
        .arg("--config")
        .arg(&config)
        .arg("--print-hash")
        .output()
        .expect("run lblogd --print-hash");
    assert!(
        output.status.success(),
        "exit {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let listed = String::from_utf8(output.stdout).expect("utf-8 output");
    let paths: Vec<&str> = listed.lines().skip(1).collect();
    assert!(
        paths.contains(&"/page/impressum.mu"),
        "the page is missing: {listed}"
    );
    assert!(
        paths.contains(&"/page/code.mu"),
        "the link is missing: {listed}"
    );
    assert!(paths.contains(&INDEX_PATH), "the landing page: {listed}");
    assert!(paths.contains(&BLOG_PATH), "the moved post index: {listed}");
}

#[test]
fn a_pages_directory_that_is_not_there_is_a_startup_error() {
    // Unlike the file area, which is optional by existence, `pages_dir` was
    // named by the operator. A typo in it would otherwise cost the landing
    // page silently, and the root would quietly go back to being the posts.
    let (posts, pages) = fixture_dirs();
    let missing = pages.path().join("gone");
    let err = load_snapshot(
        &meta(),
        &Sources::new(posts.path()).with_pages(Some(&missing)),
    )
    .expect_err("a missing pages_dir must be refused");
    assert!(
        err.to_string().contains(&missing.display().to_string()),
        "the error names the directory: {err}"
    );
}
