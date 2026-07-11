//! Tests for the Markdown-to-HTML renderer and the HTML page templates.

use lblogd::post::parse_post;
use lblogd::render::{markdown_to_html, render_index_html, render_post_html};

#[test]
fn headings() {
    assert!(markdown_to_html("# One").contains("<h1>One</h1>"));
    assert!(markdown_to_html("## Two").contains("<h2>Two</h2>"));
    assert!(markdown_to_html("### Three").contains("<h3>Three</h3>"));
}

#[test]
fn bold_and_italic() {
    assert!(markdown_to_html("**bold**").contains("<strong>bold</strong>"));
    assert!(markdown_to_html("*italic*").contains("<em>italic</em>"));
}

#[test]
fn inline_code_and_code_block() {
    assert!(markdown_to_html("`code`").contains("<code>code</code>"));
    let html = markdown_to_html("```\nlet x = 1;\n```");
    assert!(html.contains("<pre><code>"));
    assert!(html.contains("let x = 1;"));
}

#[test]
fn link() {
    let html = markdown_to_html("[text](https://example.com)");
    assert!(html.contains("<a href=\"https://example.com\">text</a>"));
}

#[test]
fn bullet_list() {
    let html = markdown_to_html("- one\n- two\n");
    assert!(html.contains("<ul>"));
    assert!(html.contains("<li>one</li>"));
    assert!(html.contains("<li>two</li>"));
}

#[test]
fn numbered_list() {
    let html = markdown_to_html("1. first\n2. second\n");
    assert!(html.contains("<ol>"));
    assert!(html.contains("<li>first</li>"));
    assert!(html.contains("<li>second</li>"));
}

fn sample_post(title: &str) -> lblogd::post::Post {
    let src = format!("+++\ntitle = \"{title}\"\ndate = \"2026-07-12\"\n+++\n\nSome **body**.\n");
    parse_post(&src).unwrap()
}

#[test]
fn post_html_is_a_complete_document() {
    let html = render_post_html(&sample_post("A Post"));
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<title>A Post</title>"));
    assert!(html.contains("<style>"));
    assert!(html.contains("<h1>A Post</h1>"));
    assert!(html.contains("2026-07-12"));
    assert!(html.contains("<strong>body</strong>"));
    assert!(html.ends_with("</html>\n"));
}

#[test]
fn post_html_escapes_the_title() {
    let html = render_post_html(&sample_post("Tags <b> and &"));
    assert!(html.contains("Tags &lt;b&gt; and &amp;"));
    assert!(!html.contains("<title>Tags <b>"));
}

#[test]
fn index_html_lists_posts_with_links() {
    let posts = vec![sample_post("First Post"), sample_post("Second Post")];
    let html = render_index_html(&posts);
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<h1>Posts</h1>"));
    assert!(html.contains("<a href=\"/posts/first-post\">First Post</a>"));
    assert!(html.contains("<a href=\"/posts/second-post\">Second Post</a>"));
    assert!(html.contains("2026-07-12"));
}
