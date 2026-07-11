# lblogd

A dev-blog server. Posts are Markdown files with a TOML frontmatter block; the
server renders them to HTML for HTTP/HTTPS clients and to micron for NomadNet
clients over Reticulum.

Batch A ships the pure logic only: the post model (frontmatter, slugs,
directory loading) and the renderers (Markdown to HTML, Markdown to micron)
with unit tests. The HTTP server and the Reticulum node follow in later
batches; the binary is a stub until then.

## Post format

```
+++
title = "Hello"
date = "2026-07-12"
slug = "hello"        # optional, defaults to slugify(title)
+++

Markdown body...
```

Slugs are plain lowercase ASCII: alphanumerics kept, everything else collapsed
to single hyphens (matching micron heading-anchor slugs).

## Renderers

`markdown_to_html` uses pulldown-cmark; `markdown_to_micron` emits micron as
defined by the `leviculum-micron` parser. Constructs without a micron
equivalent degrade gracefully (images to `[image: alt]`, tables to plaintext
rows, blockquotes to indented text); see the mapping table in
`src/render.rs`. Round-trip tests parse the generated micron with
`leviculum-micron` and assert the document structure.
