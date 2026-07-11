//! Unit tests for the post/content model: frontmatter parsing, dates, slugs,
//! and directory loading.

use std::fs;

use lblogd::post::{load_posts_dir, parse_post, slugify, Date, PostError};

const VALID: &str = "+++\ntitle = \"Hello World\"\ndate = \"2026-07-12\"\n+++\n\nBody text.\n";

#[test]
fn valid_frontmatter_parses() {
    let post = parse_post(VALID).unwrap();
    assert_eq!(post.title, "Hello World");
    assert_eq!(post.date.to_string(), "2026-07-12");
    assert_eq!(post.slug, "hello-world");
    assert!(post.body_md.contains("Body text."));
}

#[test]
fn missing_title_is_an_error() {
    let src = "+++\ndate = \"2026-07-12\"\n+++\nBody";
    assert!(matches!(parse_post(src), Err(PostError::MissingTitle)));
}

#[test]
fn empty_title_is_an_error() {
    let src = "+++\ntitle = \"  \"\ndate = \"2026-07-12\"\n+++\nBody";
    assert!(matches!(parse_post(src), Err(PostError::MissingTitle)));
}

#[test]
fn missing_date_is_an_error() {
    let src = "+++\ntitle = \"T\"\n+++\nBody";
    assert!(matches!(parse_post(src), Err(PostError::MissingDate)));
}

#[test]
fn invalid_date_is_an_error() {
    for bad in [
        "2026-13-01",
        "2026-02-30",
        "2026-7-12",
        "12.07.2026",
        "soon",
    ] {
        let src = format!("+++\ntitle = \"T\"\ndate = \"{bad}\"\n+++\nBody");
        assert!(
            matches!(parse_post(&src), Err(PostError::InvalidDate(_))),
            "date {bad:?} should be rejected"
        );
    }
}

#[test]
fn leap_day_is_accepted() {
    let src = "+++\ntitle = \"T\"\ndate = \"2024-02-29\"\n+++\nBody";
    assert_eq!(parse_post(src).unwrap().date.to_string(), "2024-02-29");
}

#[test]
fn dates_order_chronologically() {
    let a: Date = "2025-12-31".parse().unwrap();
    let b: Date = "2026-01-02".parse().unwrap();
    let c: Date = "2026-01-10".parse().unwrap();
    assert!(a < b && b < c);
}

#[test]
fn explicit_slug_is_honored() {
    let src = "+++\ntitle = \"Hello World\"\ndate = \"2026-07-12\"\nslug = \"custom\"\n+++\nBody";
    assert_eq!(parse_post(src).unwrap().slug, "custom");
}

#[test]
fn slug_defaults_from_title() {
    let src = "+++\ntitle = \"A Post, With Punctuation!\"\ndate = \"2026-07-12\"\n+++\nBody";
    assert_eq!(parse_post(src).unwrap().slug, "a-post-with-punctuation");
}

#[test]
fn missing_frontmatter_is_an_error() {
    assert!(matches!(
        parse_post("# Just Markdown\n"),
        Err(PostError::MissingFrontmatter)
    ));
}

#[test]
fn unterminated_frontmatter_is_an_error() {
    let src = "+++\ntitle = \"T\"\ndate = \"2026-07-12\"\nBody without closing";
    assert!(matches!(
        parse_post(src),
        Err(PostError::UnterminatedFrontmatter)
    ));
}

#[test]
fn invalid_toml_is_an_error() {
    let src = "+++\ntitle = unquoted\n+++\nBody";
    assert!(matches!(parse_post(src), Err(PostError::Toml(_))));
}

#[test]
fn slugify_cases() {
    assert_eq!(slugify("Hello World"), "hello-world");
    assert_eq!(slugify("  Hello,   World!  "), "hello-world");
    assert_eq!(slugify("CamelCase and 123"), "camelcase-and-123");
    assert_eq!(slugify("--a--b--"), "a-b");
    assert_eq!(slugify("trailing---"), "trailing");
    // Non-ASCII characters are separators, keeping slugs plain ASCII.
    assert_eq!(slugify("Grün und Über"), "gr-n-und-ber");
    assert_eq!(slugify(""), "");
    assert_eq!(slugify("!!!"), "");
}

#[test]
fn all_punctuation_title_without_slug_is_an_error() {
    let src = "+++\ntitle = \"!!!\"\ndate = \"2026-07-12\"\n+++\nBody";
    assert!(matches!(parse_post(src), Err(PostError::EmptySlug(_))));
}

fn write_post(dir: &std::path::Path, name: &str, title: &str, date: &str) {
    let src = format!("+++\ntitle = \"{title}\"\ndate = \"{date}\"\n+++\nBody of {title}.\n");
    fs::write(dir.join(name), src).unwrap();
}

#[test]
fn load_posts_dir_sorts_newest_first_with_title_tiebreak() {
    let dir = tempfile::tempdir().unwrap();
    write_post(dir.path(), "old.md", "Oldest", "2025-01-01");
    write_post(dir.path(), "new.md", "Newest", "2026-07-01");
    write_post(dir.path(), "tie-b.md", "Beta", "2026-03-15");
    write_post(dir.path(), "tie-a.md", "Alpha", "2026-03-15");
    fs::write(dir.path().join("notes.txt"), "not a post").unwrap();

    let posts = load_posts_dir(dir.path()).unwrap();
    let titles: Vec<&str> = posts.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Newest", "Alpha", "Beta", "Oldest"]);
}

#[test]
fn load_posts_dir_surfaces_malformed_file_with_path() {
    let dir = tempfile::tempdir().unwrap();
    write_post(dir.path(), "good.md", "Good", "2026-01-01");
    fs::write(
        dir.path().join("broken.md"),
        "+++\ntitle = \"X\"\n+++\nBody",
    )
    .unwrap();

    let err = load_posts_dir(dir.path()).unwrap_err();
    let PostError::File { path, source } = err else {
        panic!("expected PostError::File, got {err:?}");
    };
    assert!(path.ends_with("broken.md"));
    assert!(matches!(*source, PostError::MissingDate));
}

#[test]
fn load_posts_dir_missing_dir_is_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    assert!(matches!(
        load_posts_dir(&missing),
        Err(PostError::Io { .. })
    ));
}
