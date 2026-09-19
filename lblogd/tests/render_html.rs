//! Tests for the Markdown-to-HTML renderer and the HTML page templates.

use lblogd::post::{parse_post, PostDefaults};
use lblogd::render::{
    markdown_to_html, render_index_html, render_post_html, BlogMeta, DEFAULT_STYLE,
};

#[test]
fn headings_are_demoted_one_level() {
    // The page template owns <h1> for the post title, so a body heading
    // starts at <h2>; otherwise a post would have two top-level headings.
    assert!(markdown_to_html("# One").contains("<h2>One</h2>"));
    assert!(markdown_to_html("## Two").contains("<h3>Two</h3>"));
    assert!(markdown_to_html("### Three").contains("<h4>Three</h4>"));
    // h6 has nowhere left to go.
    assert!(markdown_to_html("###### Six").contains("<h6>Six</h6>"));
}

#[test]
fn a_post_has_exactly_one_h1() {
    let mut post = sample_post("The Title");
    post.body_md = "# Section\n\nText.\n".to_string();
    let html = render_post_html(&fixture_meta(), DEFAULT_STYLE, &post);
    assert_eq!(html.matches("<h1>").count(), 1, "{html}");
    assert!(html.contains("<h1>The Title</h1>"), "{html}");
    assert!(html.contains("<h2>Section</h2>"), "{html}");
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

#[test]
fn an_image_in_the_file_area_points_at_the_web_route() {
    // The author writes the name of a file that sits beside the posts; the
    // web side serves it from /files/, the mesh side as :/file/.
    for md in [
        "![Antenne](antenne.jpg)",
        "![Antenne](./antenne.jpg)",
        "![Antenne](files/antenne.jpg)",
        "![Antenne](/files/antenne.jpg)",
    ] {
        let html = markdown_to_html(md);
        assert!(
            html.contains("<img src=\"/files/antenne.jpg\" alt=\"Antenne\""),
            "input {md} produced {html}"
        );
    }
}

#[test]
fn an_external_image_is_left_exactly_as_written() {
    let html = markdown_to_html("![Antenne](https://example.com/antenne.jpg)");
    assert!(
        html.contains("<img src=\"https://example.com/antenne.jpg\""),
        "{html}"
    );
}

fn sample_post(title: &str) -> lblogd::post::Post {
    let src = format!("+++\ntitle = \"{title}\"\ndate = \"2026-07-12\"\n+++\n\nSome **body**.\n");
    parse_post(&src, &fixture_defaults()).unwrap()
}

#[test]
fn post_html_is_a_complete_document() {
    let html = render_post_html(&fixture_meta(), DEFAULT_STYLE, &sample_post("A Post"));
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
    let html = render_post_html(
        &fixture_meta(),
        DEFAULT_STYLE,
        &sample_post("Tags <b> and &"),
    );
    assert!(html.contains("Tags &lt;b&gt; and &amp;"));
    assert!(!html.contains("<title>Tags <b>"));
}

#[test]
fn index_html_lists_posts_with_links() {
    let posts = vec![sample_post("First Post"), sample_post("Second Post")];
    let html = render_index_html(&fixture_meta(), DEFAULT_STYLE, &posts);
    assert!(html.starts_with("<!doctype html>"));
    // Headed by the blog, not by the word "Posts".
    assert!(html.contains("<h1>Test Blog</h1>"), "{html}");
    assert!(html.contains("<a href=\"/posts/first-post\">First Post</a>"));
    assert!(html.contains("<a href=\"/posts/second-post\">Second Post</a>"));
    assert!(html.contains("2026-07-12"));
}

#[test]
fn a_footnote_keeps_its_definition() {
    // The regression #197 opens with: without the extension, `[^1]: Kurz`
    // parses as a link reference definition, which drops the definition line
    // from the output and turns the reference into a link labelled `^1`.
    let html = markdown_to_html("Satz.[^1]\n\n[^1]: Kurz\n");
    assert!(html.contains("Kurz"), "definition lost: {html}");
    assert!(html.contains("footnote-reference"), "{html}");
    assert!(html.contains("footnote-definition"), "{html}");
    assert!(
        !html.contains("href=\"Kurz\""),
        "definition read as a link: {html}"
    );
}

#[test]
fn a_footnote_whose_body_is_a_bare_url_keeps_it() {
    // The common shape of a source citation, and the one that vanished: the
    // URL used to survive only as the target of the mangled reference, which
    // is why asserting on the URL alone is not enough.
    let html = markdown_to_html("Satz.[^q]\n\n[^q]: https://example.com/a\n");
    assert!(
        html.contains("footnote-definition"),
        "definition lost: {html}"
    );
    assert!(html.contains(">https://example.com/a</a>"), "{html}");
    assert!(
        !html.contains(">^q<"),
        "reference read as a link label: {html}"
    );
}

#[test]
fn strikethrough() {
    assert!(markdown_to_html("~~gone~~ here").contains("<del>gone</del>"));
}

#[test]
fn task_list_items_become_checkboxes() {
    let html = markdown_to_html("- [x] done\n- [ ] open\n");
    assert!(html.contains("type=\"checkbox\" checked"), "{html}");
    assert!(html.contains("type=\"checkbox\"/>"), "{html}");
    assert!(!html.contains("[x]"), "marker left as text: {html}");
}

#[test]
fn heading_identifiers_become_the_id_attribute() {
    // Demoted like every other body heading, but the identifier survives it.
    let html = markdown_to_html("## Text {#custom-id}\n");
    assert!(html.contains("<h3 id=\"custom-id\">Text</h3>"), "{html}");
}

#[test]
fn definition_list() {
    let html = markdown_to_html("Term\n: first\n: second\n");
    assert!(html.contains("<dt>Term</dt>"), "{html}");
    assert!(html.contains("<dd>first</dd>"), "{html}");
    assert!(html.contains("<dd>second</dd>"), "{html}");
}

#[test]
fn highlight() {
    assert!(markdown_to_html("==look== here").contains("<mark>look</mark>"));
    // An unclosed marker is text, not a half-open element.
    let html = markdown_to_html("2 == 2 is true");
    assert!(!html.contains("<mark>"), "{html}");
    assert!(html.contains("2 == 2 is true"), "{html}");
}

#[test]
fn sub_and_superscript_in_both_cheat_sheet_and_flanked_form() {
    // The intra-word forms are the cheat sheet's; pulldown-cmark's own
    // extension rejects them and only takes the flanked one.
    let html = markdown_to_html("H~2~O X^2^ E ~n~ x ^y^");
    assert!(html.contains("H<sub>2</sub>O"), "{html}");
    assert!(html.contains("X<sup>2</sup>"), "{html}");
    assert!(html.contains("<sub>n</sub>"), "{html}");
    assert!(html.contains("<sup>y</sup>"), "{html}");
}

#[test]
fn an_approximation_is_not_a_subscript() {
    // `~` is common prose. Only the intra-word pair is ours, and the parser
    // leaves the flanked-but-unpaired form alone.
    let html = markdown_to_html("about ~5 or ~10 kg in ~/tmp");
    assert!(!html.contains("<sub>"), "{html}");
    assert!(html.contains("about ~5 or ~10 kg in ~/tmp"), "{html}");
}

#[test]
fn bare_urls_and_addresses_become_links() {
    let html = markdown_to_html("see https://example.com/x, now");
    assert!(
        html.contains("<a href=\"https://example.com/x\">https://example.com/x</a>,"),
        "the comma ends the sentence, not the URL: {html}"
    );
    let html = markdown_to_html("(and https://b.de/y) end");
    assert!(
        html.contains("<a href=\"https://b.de/y\">https://b.de/y</a>)"),
        "the bracket the URL did not open is not part of it: {html}"
    );
    let html = markdown_to_html("write to lp@lew-palm.de please");
    assert!(
        html.contains("<a href=\"mailto:lp@lew-palm.de\">lp@lew-palm.de</a>"),
        "{html}"
    );
}

#[test]
fn a_bare_url_is_not_found_where_it_is_not_prose() {
    // Code is quoted so that nothing rewrites it, and a URL in a link label
    // would otherwise become a second link inside the first.
    let html = markdown_to_html("`see https://example.com/x`");
    assert!(!html.contains("<a href"), "{html}");
    let html = markdown_to_html("```\nhttps://example.com/x\n```\n");
    assert!(!html.contains("<a href"), "{html}");
    let html = markdown_to_html("[https://label.de](https://real.de)");
    assert_eq!(html.matches("<a href").count(), 1, "{html}");
    assert!(html.contains("href=\"https://real.de\""), "{html}");
}

#[test]
fn emoji_shortcodes_stay_as_written() {
    // Declined, not forgotten: see the module docs for why a partial table
    // would read worse than none.
    assert!(markdown_to_html("joy :joy: here").contains(":joy:"));
}

/// Defaults for fixtures that always set title and date themselves.
fn fixture_defaults() -> PostDefaults {
    PostDefaults {
        title: "fixture".to_string(),
        date: "2000-01-01".parse().unwrap(),
    }
}

/// Blog metadata for fixtures that are about rendering, not about identity.
fn fixture_meta() -> BlogMeta {
    BlogMeta {
        title: "Test Blog".to_string(),
        author: None,
        description: None,
        language: "en".to_string(),
        web_url: None,
        nomadnet_address: None,
        email: None,
        lxmf: None,
        has_about: false,
        has_landing: false,
        nav: Vec::new(),
    }
}
