//! The site beyond the posts: static pages, links that point elsewhere, and
//! the one table that says which names are already taken.
//!
//! A blog is not the whole of what a domain has to say. `pages_dir` holds
//! Markdown pages that are not entries — a landing page, a page about the
//! code — rendered like the about page: no date, no byline, never listed and
//! never in the feed. `[links]` holds the other half: names that do not carry
//! content but point at something off this server, and therefore have to look
//! different on each side. A browser can be redirected; a NomadNet client
//! cannot follow a web link at all, so the mesh side shows the URL as text.
//!
//! Both halves compete for the same two namespaces — the web's top-level path
//! and the mesh's `/page/<name>.mu` — with the posts and with the routes the
//! server already answers. [`RESERVED`] is the single table that decides it,
//! checked once in [`load_site`] for pages and links alike, so a name can
//! never be free on one side and taken on the other.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::post::{parse_post, slugify, Post, PostDefaults, PostError};

/// The page name that becomes the site's landing page: `index.md` in
/// `pages_dir` takes web `/` and mesh `/page/index.mu`, and the post index
/// moves down to [`BLOG_NAME`].
pub const LANDING_NAME: &str = "index";

/// The name the post index moves to once a landing page has taken the root.
pub const BLOG_NAME: &str = "blog";

/// The about page's name, reserved only when an about page is configured.
pub const ABOUT_NAME: &str = "about";

/// One name the server answers under no matter what the operator configures,
/// and what answers it on each side.
///
/// Carrying both paths is the point: the reason `files` is unusable is a web
/// route and the reason a post slug is unusable is a mesh path, and an error
/// message that names only one of them sends the operator looking in the
/// wrong place.
pub struct Reserved {
    /// The name that cannot be used for a page or a link.
    pub name: &'static str,
    /// What already answers there, for the error message.
    pub what: &'static str,
}

/// Every unconditionally reserved name, in one place.
///
/// Two more are reserved conditionally and are checked alongside these in
/// [`taken_by`]: [`ABOUT_NAME`] when an about page is configured, and every
/// post's slug, which owns `/page/<slug>.mu` on the mesh. [`LANDING_NAME`] is
/// reserved for links but not for pages, where it *is* the landing page.
pub const RESERVED: &[Reserved] = &[
    Reserved {
        name: BLOG_NAME,
        what: "the post index (web /blog, mesh /page/blog.mu)",
    },
    Reserved {
        name: "posts",
        what: "the post route (web /posts/<slug>)",
    },
    Reserved {
        name: "files",
        what: "the file area (web /files/<name>)",
    },
    Reserved {
        name: "feed.xml",
        what: "the Atom feed (web /feed.xml)",
    },
];

/// Errors from loading the static pages and the links.
#[derive(Debug, Error)]
pub enum SiteError {
    /// The configured pages directory could not be read.
    #[error("reading pages_dir {path}: {source}")]
    Read {
        /// The directory that could not be read.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A page file's name is not a usable page name.
    #[error(
        "page {path}: {name:?} is not a usable page name: \
         plain lowercase ASCII letters, digits and hyphens only"
    )]
    PageName {
        /// The offending file.
        path: String,
        /// Its name, the file stem.
        name: String,
    },
    /// A link's key is not a usable name.
    #[error(
        "links.{name}: {name:?} is not a usable name: \
         plain lowercase ASCII letters, digits and hyphens only"
    )]
    LinkName {
        /// The offending key.
        name: String,
    },
    /// A page or a link claims a name something else already answers under.
    #[error("{kind} {name:?} collides with {what}")]
    Collision {
        /// Whether a page or a link claimed it.
        kind: &'static str,
        /// The claimed name.
        name: String,
        /// What already answers there.
        what: String,
    },
    /// A link's target is not an absolute HTTP(S) URL.
    #[error("links.{name}: {url:?} must be an absolute http:// or https:// URL")]
    LinkUrl {
        /// The offending key.
        name: String,
        /// What it was set to.
        url: String,
    },
    /// A page file failed to parse. Carries the file name from [`PostError`].
    #[error("{0}")]
    Page(#[from] PostError),
}

/// One `[links]` entry as the config file spells it, before the page of the
/// same name (if any) is attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpec {
    /// The name, which is the path on both sides.
    pub name: String,
    /// Where it points: an absolute `http://` or `https://` URL.
    pub url: String,
}

/// A static page: a Markdown file in `pages_dir` served under its own name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SitePage {
    /// The file stem, which is the path on both sides.
    pub name: String,
    /// The parsed file. Only its title and body are used; a page is not a
    /// dated entry, so its date, slug and author are ignored exactly as the
    /// about page's are.
    pub page: Post,
}

/// A link, with the page of the same name attached when there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteLink {
    /// The name, which is the path on both sides.
    pub name: String,
    /// Where it points.
    pub url: String,
    /// `pages_dir/<name>.md`, rendered above the URL on the mesh side so a
    /// link can carry, say, clone instructions. The web side redirects and
    /// never shows it.
    pub page: Option<Post>,
}

impl SiteLink {
    /// What to call this in a nav line: the page's title when it has one,
    /// else the name with its first letter capitalised.
    pub fn label(&self) -> String {
        match &self.page {
            Some(page) => page.title.clone(),
            None => capitalize(&self.name),
        }
    }
}

/// Everything the site serves besides the posts and the about page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Site {
    /// The landing page, when `pages_dir` holds `index.md`. Its presence is
    /// what moves the post index to `/blog`.
    pub landing: Option<Post>,
    /// The other static pages, by name. A directory has no order of its own,
    /// so they are sorted by name and the nav follows that.
    pub pages: Vec<SitePage>,
    /// The links, in the order `[links]` lists them.
    pub links: Vec<SiteLink>,
}

impl Site {
    /// Whether a landing page has taken the root.
    pub fn has_landing(&self) -> bool {
        self.landing.is_some()
    }

    /// Nothing configured: no landing page, no pages, no links. This is the
    /// state every deployment written before any of this existed is in, and
    /// the one in which the served bytes must be exactly what they were.
    pub fn is_empty(&self) -> bool {
        self.landing.is_none() && self.pages.is_empty() && self.links.is_empty()
    }

    /// The static page served under `name`, if any. A name that is also a
    /// link is deliberately not found here: the redirect wins on the web, and
    /// the mesh renders the page through the link.
    pub fn page(&self, name: &str) -> Option<&Post> {
        self.pages.iter().find(|p| p.name == name).map(|p| &p.page)
    }

    /// The link named `name`, if any.
    pub fn link(&self, name: &str) -> Option<&SiteLink> {
        self.links.iter().find(|l| l.name == name)
    }
}

/// Read `pages_dir` and validate the links against it, the posts and the
/// reserved names.
///
/// `blog_title` is what an `index.md` without a frontmatter title is headed
/// with — a landing page titled "index" would tell a reader nothing, exactly
/// as an about page titled "about" would not.
pub fn load_site(
    blog_title: &str,
    pages_dir: Option<&Path>,
    links: &[LinkSpec],
    posts: &[Post],
    has_about: bool,
) -> Result<Site, SiteError> {
    let taken = Reservations { posts, has_about };
    let mut landing = None;
    let mut pages: Vec<SitePage> = Vec::new();

    if let Some(dir) = pages_dir {
        for path in page_files(dir)? {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Reserved before well-formed: `feed.xml` is both, and "that name
            // is the Atom feed" is the more useful of the two answers.
            if name != LANDING_NAME {
                if let Some(what) = taken.taken_by(&name) {
                    return Err(SiteError::Collision {
                        kind: "page",
                        name,
                        what,
                    });
                }
            }
            if !is_usable_name(&name) {
                return Err(SiteError::PageName {
                    path: path.display().to_string(),
                    name,
                });
            }
            let page = load_page(&path, &name, blog_title)?;
            match name == LANDING_NAME {
                true => landing = Some(page),
                false => pages.push(SitePage { name, page }),
            }
        }
    }
    pages.sort_by(|a, b| a.name.cmp(&b.name));

    let mut site_links = Vec::with_capacity(links.len());
    for spec in links {
        // Unlike a page, a link may not be called `index`: the landing page
        // owns the root, and a redirect from it would take the site with it.
        if let Some(what) = taken
            .taken_by(&spec.name)
            .or_else(|| (spec.name == LANDING_NAME).then(|| "the landing page (web /)".to_string()))
        {
            return Err(SiteError::Collision {
                kind: "link",
                name: spec.name.clone(),
                what,
            });
        }
        if !is_usable_name(&spec.name) {
            return Err(SiteError::LinkName {
                name: spec.name.clone(),
            });
        }
        if !is_absolute_http_url(&spec.url) {
            return Err(SiteError::LinkUrl {
                name: spec.name.clone(),
                url: spec.url.clone(),
            });
        }
        // A page of the same name moves under the link: on the mesh its body
        // is rendered above the URL, on the web the redirect wins and it is
        // never shown.
        let page = pages
            .iter()
            .position(|p| p.name == spec.name)
            .map(|idx| pages.remove(idx).page);
        site_links.push(SiteLink {
            name: spec.name.clone(),
            url: spec.url.clone(),
            page,
        });
    }

    Ok(Site {
        landing,
        pages,
        links: site_links,
    })
}

/// The `*.md` files in `pages_dir`, sorted, so an error names the same file
/// on every run.
fn page_files(dir: &Path) -> Result<Vec<PathBuf>, SiteError> {
    let read_err = |source| SiteError::Read {
        path: dir.display().to_string(),
        source,
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(read_err)? {
        let path = entry.map_err(read_err)?.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "md") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Parse one page file. Its defaults are the post loader's, except that
/// `index.md` is headed with the blog's name rather than with "index".
fn load_page(path: &Path, name: &str, blog_title: &str) -> Result<Post, SiteError> {
    let source = std::fs::read_to_string(path).map_err(|source| PostError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut defaults = PostDefaults::for_file(path)?;
    if name == LANDING_NAME && !slugify(blog_title).is_empty() {
        defaults.title = blog_title.to_string();
    }
    let page = parse_post(&source, &defaults).map_err(|e| PostError::File {
        path: path.to_path_buf(),
        source: Box::new(e),
    })?;
    Ok(page)
}

/// The names that are taken for a given blog, beyond the constant table.
struct Reservations<'a> {
    posts: &'a [Post],
    has_about: bool,
}

impl Reservations<'_> {
    /// What already answers under `name`, or `None` when it is free.
    fn taken_by(&self, name: &str) -> Option<String> {
        if let Some(reserved) = RESERVED.iter().find(|r| r.name == name) {
            return Some(reserved.what.to_string());
        }
        if self.has_about && name == ABOUT_NAME {
            return Some("the about page (web /about, mesh /page/about.mu)".to_string());
        }
        self.posts
            .iter()
            .find(|p| p.slug == name)
            .map(|p| format!("the post {:?} (mesh /page/{}.mu)", p.title, p.slug))
    }
}

/// Whether a name may be a page or a link: exactly what the crate's slug
/// rules already produce, so a page path can never need escaping and can
/// never differ from what an author typed.
fn is_usable_name(name: &str) -> bool {
    !name.is_empty() && slugify(name) == name
}

/// Whether a link target is an absolute HTTP(S) URL.
///
/// Only those two schemes: a relative path would silently point back at this
/// server, and a `mailto:` or `lxmf:` target is not something a browser
/// redirect can carry.
fn is_absolute_http_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|rest| !rest.is_empty())
}

/// Capitalise the first character, for a link with no page to title it.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(title: &str, slug: &str) -> Post {
        Post {
            title: title.to_string(),
            date: "2026-07-01".parse().expect("fixture date"),
            author: None,
            slug: slug.to_string(),
            body_md: String::new(),
        }
    }

    fn link(name: &str, url: &str) -> LinkSpec {
        LinkSpec {
            name: name.to_string(),
            url: url.to_string(),
        }
    }

    #[test]
    fn nothing_configured_is_an_empty_site() {
        let site = load_site("Blog", None, &[], &[], false).expect("load");
        assert!(site.is_empty());
        assert!(!site.has_landing());
    }

    #[test]
    fn a_link_url_must_carry_a_scheme() {
        for bad in ["/code", "codeberg.org", "ftp://example.org", "https://"] {
            let err = load_site("Blog", None, &[link("code", bad)], &[], false)
                .expect_err("must be rejected");
            assert!(
                matches!(&err, SiteError::LinkUrl { name, .. } if name == "code"),
                "{err}"
            );
            assert!(err.to_string().contains("code"), "names the key: {err}");
        }
        for good in ["https://example.org", "http://example.org/x"] {
            load_site("Blog", None, &[link("code", good)], &[], false).expect("accepted");
        }
    }

    #[test]
    fn a_link_may_not_take_the_landing_page() {
        let err = load_site(
            "Blog",
            None,
            &[link("index", "https://example.org")],
            &[],
            false,
        )
        .expect_err("must be rejected");
        assert!(err.to_string().contains("landing page"), "{err}");
    }

    #[test]
    fn reserved_names_are_reserved_for_links_too() {
        for name in ["blog", "posts", "files", "feed.xml"] {
            let err = load_site(
                "Blog",
                None,
                &[link(name, "https://example.org")],
                &[],
                false,
            )
            .expect_err("must be rejected");
            assert!(
                matches!(&err, SiteError::Collision { kind: "link", .. }),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn about_is_reserved_only_once_an_about_page_exists() {
        let links = [link("about", "https://example.org")];
        load_site("Blog", None, &links, &[], false).expect("free without an about page");
        let err = load_site("Blog", None, &links, &[], true).expect_err("taken with one");
        assert!(err.to_string().contains("about page"), "{err}");
    }

    #[test]
    fn a_post_slug_is_reserved() {
        let posts = [post("Hello Mesh", "hello-mesh")];
        let err = load_site(
            "Blog",
            None,
            &[link("hello-mesh", "https://example.org")],
            &posts,
            false,
        )
        .expect_err("must be rejected");
        assert!(err.to_string().contains("Hello Mesh"), "{err}");
    }

    #[test]
    fn a_link_name_follows_the_slug_rules() {
        let err = load_site(
            "Blog",
            None,
            &[link("Code Hier", "https://example.org")],
            &[],
            false,
        )
        .expect_err("must be rejected");
        assert!(
            matches!(&err, SiteError::LinkName { name } if name == "Code Hier"),
            "{err}"
        );
    }

    #[test]
    fn links_keep_the_order_the_config_gave_them() {
        let site = load_site(
            "Blog",
            None,
            &[
                link("issues", "https://example.org/i"),
                link("code", "https://example.org/c"),
            ],
            &[],
            false,
        )
        .expect("load");
        let names: Vec<&str> = site.links.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["issues", "code"], "config order, not name order");
    }

    #[test]
    fn a_link_without_a_page_is_labelled_by_its_name() {
        let site = load_site(
            "Blog",
            None,
            &[link("code", "https://example.org")],
            &[],
            false,
        )
        .expect("load");
        assert_eq!(site.links[0].label(), "Code");
    }

    #[test]
    fn usable_names_are_exactly_the_slug_shapes() {
        assert!(is_usable_name("code"));
        assert!(is_usable_name("about-me"));
        assert!(is_usable_name("v2"));
        assert!(!is_usable_name(""));
        assert!(!is_usable_name("Code"));
        assert!(!is_usable_name("feed.xml"));
        assert!(!is_usable_name("über"));
        assert!(!is_usable_name("a b"));
    }
}
