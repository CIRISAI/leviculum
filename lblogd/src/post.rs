//! The post/content model: frontmatter parsing, slugs, and directory loading.
//!
//! A post is a Markdown file with a leading TOML frontmatter block delimited
//! by `+++` lines:
//!
//! ```text
//! +++
//! title = "Hello"
//! date = "2026-07-12"
//! slug = "hello"        # optional, defaults to slugify(title)
//! +++
//!
//! Markdown body...
//! ```
//!
//! `title` and `date` are required; a missing or invalid field is a clear
//! [`PostError`]. Dates are plain `YYYY-MM-DD` values ordered by
//! (year, month, day); no calendar library is involved.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;
use thiserror::Error;

/// Errors from parsing a post or loading a posts directory.
#[derive(Debug, Error)]
pub enum PostError {
    /// The source does not start with a `+++` frontmatter delimiter line.
    #[error("missing frontmatter: post must start with a +++ line")]
    MissingFrontmatter,
    /// The opening `+++` has no matching closing `+++` line.
    #[error("unterminated frontmatter: no closing +++ line")]
    UnterminatedFrontmatter,
    /// The frontmatter block is not valid TOML.
    #[error("invalid frontmatter TOML: {0}")]
    Toml(#[from] toml::de::Error),
    /// The required `title` field is missing or empty.
    #[error("frontmatter is missing required field: title")]
    MissingTitle,
    /// The required `date` field is missing.
    #[error("frontmatter is missing required field: date")]
    MissingDate,
    /// The `date` field is not a valid `YYYY-MM-DD` calendar date.
    #[error("invalid date {0:?}: expected YYYY-MM-DD")]
    InvalidDate(String),
    /// Neither an explicit slug nor the title yields a non-empty slug.
    #[error("empty slug: title {0:?} slugifies to nothing and no explicit slug is set")]
    EmptySlug(String),
    /// A file in the posts directory failed to parse.
    #[error("{}: {source}", path.display())]
    File {
        /// The offending file.
        path: PathBuf,
        /// The underlying parse error.
        source: Box<PostError>,
    },
    /// A filesystem read failed.
    #[error("reading {}: {source}", path.display())]
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

/// A calendar date ordered by (year, month, day).
///
/// Parsed from `YYYY-MM-DD` with real month/day range checks (including leap
/// years); displayed back in the same form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    /// Four-digit year.
    pub year: i32,
    /// Month, 1-12.
    pub month: u8,
    /// Day of month, 1-31 (validated against the month).
    pub day: u8,
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for Date {
    type Err = PostError;

    fn from_str(s: &str) -> Result<Date, PostError> {
        let invalid = || PostError::InvalidDate(s.to_string());
        let parts: Vec<&str> = s.split('-').collect();
        let [y, m, d] = parts.as_slice() else {
            return Err(invalid());
        };
        if y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return Err(invalid());
        }
        let year: i32 = y.parse().map_err(|_| invalid())?;
        let month: u8 = m.parse().map_err(|_| invalid())?;
        let day: u8 = d.parse().map_err(|_| invalid())?;
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return Err(invalid());
        }
        Ok(Date { year, month, day })
    }
}

/// The number of days in `month` of `year` (Gregorian, leap-year aware).
fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// A parsed blog post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Post {
    /// The post title from the frontmatter.
    pub title: String,
    /// The publication date from the frontmatter.
    pub date: Date,
    /// The URL slug: the explicit frontmatter `slug`, else `slugify(title)`.
    pub slug: String,
    /// The Markdown body (everything after the closing `+++` line).
    pub body_md: String,
}

/// The raw TOML frontmatter shape. All fields optional so that missing
/// required fields surface as specific [`PostError`]s instead of a generic
/// TOML message; unknown fields are tolerated.
#[derive(Deserialize)]
struct RawFrontmatter {
    title: Option<String>,
    date: Option<String>,
    slug: Option<String>,
}

/// Parse a post source (frontmatter plus Markdown body) into a [`Post`].
pub fn parse_post(source: &str) -> Result<Post, PostError> {
    let mut lines = source.split('\n');
    let first = lines.next().unwrap_or("");
    if first.trim_end_matches('\r') != "+++" {
        return Err(PostError::MissingFrontmatter);
    }

    let mut frontmatter = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end_matches('\r') == "+++" {
            closed = true;
            break;
        }
        frontmatter.push_str(line);
        frontmatter.push('\n');
    }
    if !closed {
        return Err(PostError::UnterminatedFrontmatter);
    }
    let body_md: String = lines.collect::<Vec<&str>>().join("\n");

    let raw: RawFrontmatter = toml::from_str(&frontmatter)?;
    let title = match raw.title {
        Some(t) if !t.trim().is_empty() => t,
        _ => return Err(PostError::MissingTitle),
    };
    let date: Date = raw.date.ok_or(PostError::MissingDate)?.parse()?;
    let slug = match raw.slug {
        Some(s) if !s.is_empty() => s,
        _ => {
            let s = slugify(&title);
            if s.is_empty() {
                return Err(PostError::EmptySlug(title));
            }
            s
        }
    };

    Ok(Post {
        title,
        date,
        slug,
        body_md,
    })
}

/// Slugify a string for use in URLs and page paths.
///
/// Keeps ASCII alphanumerics (lowercased); every other run of characters
/// becomes a single hyphen; leading/trailing hyphens are trimmed. Non-ASCII
/// characters are treated as separators, matching the micron heading-anchor
/// slug rules, so slugs are always plain lowercase ASCII.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !out.is_empty() && !prev_hyphen {
            out.push('-');
            prev_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Load every `*.md` file in `dir` as a post, sorted by date descending
/// (newest first) with a stable title tie-break. A malformed file surfaces as
/// [`PostError::File`] naming the file.
pub fn load_posts_dir(dir: &Path) -> Result<Vec<Post>, PostError> {
    let read_err = |source| PostError::Io {
        path: dir.to_path_buf(),
        source,
    };
    let mut posts = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(read_err)? {
        let path = entry.map_err(read_err)?.path();
        if !path.is_file() || path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|source| PostError::Io {
            path: path.clone(),
            source,
        })?;
        let post = parse_post(&source).map_err(|e| PostError::File {
            path: path.clone(),
            source: Box::new(e),
        })?;
        posts.push(post);
    }
    posts.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.title.cmp(&b.title)));
    Ok(posts)
}
