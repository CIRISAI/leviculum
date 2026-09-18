//! The compatibility contract in one place: a blog that configures neither
//! `pages_dir` nor `[links]` serves exactly the bytes it served before either
//! existed.
//!
//! Every other test asserts on a substring, which is the right shape for a
//! feature but the wrong one for "nothing changed": a nav line, a moved back
//! link or a stray newline all slip past a `contains`. These pin the whole
//! document. When they fail, the question to answer first is whether the
//! change is visible to a deployment that asked for none of this.

use lblogd::content::{load_snapshot, Sources, INDEX_PATH};
use lblogd::post::{Date, Post};
use lblogd::render::{
    render_index_html, render_index_micron, render_post_html, render_post_micron, BlogMeta,
};

/// A deliberately tiny stylesheet: the built-in one is long, and what these
/// tests pin is the document around it, not its contents.
const CSS: &str = "body{}";

fn meta() -> BlogMeta {
    BlogMeta {
        title: "Test Blog".to_string(),
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

#[test]
fn the_html_index_is_unchanged() {
    assert_eq!(
        render_index_html(&meta(), CSS, &[post()]),
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Test Blog</title>\n\
         <style>body{}</style>\n\
         </head>\n\
         <body>\n\
         <h1>Test Blog</h1>\n\
         <ul class=\"posts\">\n\
         <li><span class=\"date\">2026-07-01</span> \
         <a href=\"/posts/hallo\">Hallo Mesh</a></li>\n\
         </ul>\n\
         </body>\n\
         </html>\n"
    );
}

#[test]
fn the_html_post_page_is_unchanged() {
    assert_eq!(
        render_post_html(&meta(), CSS, &post()),
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Hallo Mesh</title>\n\
         <style>body{}</style>\n\
         </head>\n\
         <body>\n\
         <article>\n\
         <h1>Hallo Mesh</h1>\n\
         <p class=\"date\">2026-07-01</p>\n\
         <p>Erster Text.</p>\n\
         </article>\n\
         <p><a href=\"/\">&larr; Test Blog</a></p>\n\
         </body>\n\
         </html>\n"
    );
}

#[test]
fn the_micron_index_is_unchanged() {
    assert_eq!(
        render_index_micron(&meta(), &[post()]),
        ">Test Blog\n\n`[2026-07-01 Hallo Mesh`:/page/hallo.mu]\n"
    );
}

#[test]
fn the_micron_post_page_is_unchanged() {
    assert_eq!(
        render_post_micron(&meta(), &post()),
        ">Hallo Mesh\n\n2026-07-01\n-\n\nErster Text.\n\n\n`[\u{2190} Test Blog`:/page/index.mu]\n"
    );
}

#[test]
fn a_blog_with_neither_pages_nor_links_serves_the_same_paths() {
    let posts = tempfile::tempdir().expect("posts dir");
    std::fs::write(
        posts.path().join("hallo.md"),
        "+++\ntitle = \"Hallo Mesh\"\ndate = \"2026-07-01\"\nslug = \"hallo\"\n+++\n\nErster Text.\n",
    )
    .expect("write post");

    let snapshot = load_snapshot(&meta(), &Sources::new(posts.path())).expect("load");
    assert_eq!(
        snapshot.served_paths(),
        vec!["/page/hallo.mu".to_string(), INDEX_PATH.to_string()],
        "no /page/blog.mu, no page of any other name"
    );
    assert!(
        snapshot.meta.nav.is_empty(),
        "an empty nav is what renders to nothing: {:?}",
        snapshot.meta.nav
    );
    assert!(!snapshot.meta.has_landing);
}
