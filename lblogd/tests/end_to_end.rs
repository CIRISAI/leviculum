//! End-to-end: parse a full sample post (frontmatter plus mixed Markdown) and
//! render it through both pipelines without error.

use lblogd::post::{parse_post, PostDefaults};
use lblogd::render::{
    render_index_html, render_index_micron, render_post_html, render_post_micron, BlogMeta,
    DEFAULT_STYLE,
};
use leviculum_micron::{parse, Block};

const SAMPLE: &str = r#"+++
title = "Bringing Up the LoRa Rig"
date = "2026-07-12"
+++

The rig is **finally** stable. Setup was *mostly* painless:

# Hardware

- two LNodes
- three RNodes

## Flashing

Run `just flash` and wait:

```
$ just flash
flashing T114 on /dev/ttyACM0
```

See [the docs](https://example.com/rig) for details.

![rig photo](rig.png)

| board | count |
|-------|-------|
| LNode | 2     |
"#;

#[test]
fn sample_post_renders_to_both_formats() {
    let post = parse_post(SAMPLE, &fixture_defaults()).unwrap();
    assert_eq!(post.title, "Bringing Up the LoRa Rig");
    assert_eq!(post.slug, "bringing-up-the-lora-rig");
    assert_eq!(post.date.to_string(), "2026-07-12");

    let html = render_post_html(&fixture_meta(), DEFAULT_STYLE, &post);
    assert!(html.contains("<title>Bringing Up the LoRa Rig</title>"));
    assert!(html.contains("2026-07-12"));
    assert!(html.contains("<strong>finally</strong>"));
    assert!(html.contains("<pre><code>"));

    let micron = render_post_micron(&fixture_meta(), &post);
    let doc = parse(&micron);
    assert!(matches!(doc.blocks[0], Block::Heading { depth: 1, .. }));
    assert!(doc
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Heading { depth: 2, .. })));
    assert!(doc
        .blocks
        .iter()
        .any(|b| matches!(b, Block::LiteralBlock { .. })));
    assert!(micron.contains("2026-07-12"));

    let index_html = render_index_html(&fixture_meta(), DEFAULT_STYLE, std::slice::from_ref(&post));
    assert!(index_html.contains("/posts/bringing-up-the-lora-rig"));

    let index_micron = render_index_micron(&fixture_meta(), std::slice::from_ref(&post));
    assert!(index_micron.contains(":/page/bringing-up-the-lora-rig.mu"));
    assert!(index_micron.contains("2026-07-12"));
}

/// A post using the extended syntax of the Markdown cheat sheet, the part
/// that #197 added. Kept separate from SAMPLE so the basic-syntax assertions
/// above stay about basic syntax.
const EXTENDED: &str = r#"+++
title = "Extended Syntax"
date = "2026-09-19"
+++

Water is H~2~O and the answer is X^2^.[^src]

- [x] measured
- [ ] written up

~~Wrong~~ ==right==, see https://example.com/x or write to lp@example.com.

Term
: what it means

[^src]: https://example.com/source
"#;

#[test]
fn extended_syntax_survives_both_page_pipelines() {
    let post = parse_post(EXTENDED, &fixture_defaults()).unwrap();

    let html = render_post_html(&fixture_meta(), DEFAULT_STYLE, &post);
    for expected in [
        "H<sub>2</sub>O",
        "X<sup>2</sup>",
        "footnote-reference",
        "type=\"checkbox\" checked",
        "<del>Wrong</del>",
        "<mark>right</mark>",
        "<a href=\"https://example.com/x\">",
        "<a href=\"mailto:lp@example.com\">",
        "<dt>Term</dt>",
        "<dd>what it means</dd>",
        // The definition itself, which is what used to vanish.
        ">https://example.com/source</a>",
    ] {
        assert!(html.contains(expected), "missing {expected}: {html}");
    }

    let micron = render_post_micron(&fixture_meta(), &post);
    let doc = parse(&micron);
    let text: String = doc
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph { line, .. } => Some(line_text(line)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "H\u{2082}O",
        "X\u{b2}",
        "[src]",
        "\u{2611} measured",
        "\u{2610} written up",
        "  what it means",
        "[src] https://example.com/source",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    // The page still ends with the way back to the index, after the
    // footnote block rather than in front of it.
    let back = micron.find(":/page/index.mu").expect("back link");
    // The marker followed by a space is the definition; in the body the same
    // marker is followed by the sentence's full stop.
    let footnote = micron.find("[src] ").expect("footnote block");
    assert!(footnote < back, "footnotes after the back link:\n{micron}");
}

/// The concatenated span text of a line.
fn line_text(line: &leviculum_micron::Line) -> String {
    line.spans.iter().map(|s| s.text.as_str()).collect()
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
