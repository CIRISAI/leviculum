//! NomadNet URL parsing.
//!
//! The vocabulary here is the reference's own (NomadNet's built-in guide,
//! "Links and URLs"): the whole string is a **URL**, its first part is the
//! node's **destination address**, and everything after the `:` is the
//! **request path**. Leaving the destination address out makes it a URL to a
//! **local** page or file — local to the page in view.
//!
//! ```text
//! <address>                     -> address + /page/index.mu
//! <address>:/page/about.mu      -> address + /page/about.mu
//! <address>:                    -> address + /page/index.mu   (empty path)
//! :/page/about.mu               -> the open page's node + /page/about.mu
//! <address>:/page/x.mu`a=1|b=2  -> address + path + fields {var_a:1, var_b:2}
//! ```
//!
//! `<address>` is exactly [`TRUNCATED_HASH_HEX_LEN`] hex characters (the 16-byte
//! Reticulum truncated destination hash). Query fields follow a single backtick
//! and are `key=value` pairs joined by `|`; each key is stored with the NomadNet
//! `var_` prefix, matching how the reference browser passes URL query variables
//! to a page's request handler.
//!
//! Note what is NOT a form: a bare request path (`/page/about.mu`). The `:` is
//! what marks the destination-address boundary, so a URL that drops it names no
//! node at all and is rejected — by the reference exactly as here. See
//! `reference_rejects_the_same_urls_we_do` for the measured comparison.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// The default path when a URL names only a destination or an empty path,
/// matching `Browser.DEFAULT_PATH`.
pub const DEFAULT_PATH: &str = "/page/index.mu";

/// Length in hex characters of a Reticulum truncated destination hash
/// (`RNS.Reticulum.TRUNCATED_HASHLENGTH // 8 * 2` = 16 bytes = 32 chars).
pub use leviculum_ui::link::TRUNCATED_HASH_HEX_LEN;

use crate::render::RenderedLink;

/// A parsed page request target: where to link, what to ask for, and the query
/// fields to carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The 16-byte destination hash to link to.
    pub dest_hash: [u8; 16],
    /// The request path, e.g. `/page/index.mu`.
    pub path: String,
    /// Query fields as `(var_key, value)` pairs, in URL order. NomadNet passes
    /// these to the page handler as `var_*` request variables.
    pub fields: Vec<(String, String)>,
    /// Whether the path targets a `/file/` download rather than a `/page/`.
    /// A file target is downloaded to disk
    /// ([`Session::download_file`](crate::fetch::Session::download_file))
    /// instead of being rendered as a page.
    pub is_file: bool,
}

/// Errors from [`parse_url`].
///
/// Every variant means the same thing to the parser — the URL is rejected — and
/// exactly the same set of URLs is rejected as by the reference (see the module
/// docs). They differ only in what they can tell the reader, so the two mistakes
/// worth naming get their own message instead of a bare "malformed URL".
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UrlError {
    /// A request path written without the `:` that a local page or file keeps:
    /// `/page/about.mu` rather than `:/page/about.mu`. The commonest mistake in
    /// a hand-written page, and the one with a mechanical fix.
    #[error("malformed URL: a local page or file keeps the \":\"")]
    NoDestination,
    /// A local URL (a leading `:`) with no page open, so there is no
    /// destination address to take.
    #[error("a local URL needs an open page to take its destination address from")]
    NoOpenPage,
    /// The URL did not match any accepted form (bad hash length, non-hex
    /// destination, or too many `:`).
    #[error("malformed URL")]
    Malformed,
}

/// Parse a NomadNet URL into a [`Target`], mirroring `Browser.retrieve_url`.
///
/// `current_dest` is the destination of the page currently being viewed; it is
/// used only for the same-destination form (a leading `:`). Fields after a
/// single backtick are split on `|` into `key=value` pairs and stored with the
/// `var_` prefix; entries without exactly one `=` are ignored, matching the
/// reference parser.
pub fn parse_url(input: &str, current_dest: Option<[u8; 16]>) -> Result<Target, UrlError> {
    // Split off the query-fields component after a single backtick. The
    // reference only treats fields when there is exactly one backtick (two
    // components); otherwise the whole string stays the URL (and any stray
    // backtick makes the destination parse fail below).
    let backtick_parts: Vec<&str> = input.split('`').collect();
    let (url_part, fields) = if backtick_parts.len() == 2 {
        (backtick_parts[0], parse_fields(backtick_parts[1]))
    } else {
        (input, Vec::new())
    };

    // Split destination from path on the first `:` boundary.
    let colon_parts: Vec<&str> = url_part.split(':').collect();
    let (dest_hash, path) = match colon_parts.as_slice() {
        [only] => match parse_dest_hash(only) {
            // Bare destination address -> default path.
            Ok(dest) => (dest, DEFAULT_PATH.to_string()),
            // A request path that dropped the `:` along with the destination
            // address. Rejected either way (the reference rejects it too), but
            // it is worth saying so in the one case where the fix is obvious;
            // anything that does not even look like a path stays "malformed".
            Err(_) if only.starts_with('/') => return Err(UrlError::NoDestination),
            Err(err) => return Err(err),
        },
        [head, tail] => {
            if head.len() == TRUNCATED_HASH_HEX_LEN {
                let dest = parse_dest_hash(head)?;
                (dest, normalize_path(tail))
            } else if head.is_empty() {
                // Local form: the destination address is left out, so it comes
                // from the page in view.
                let dest = current_dest.ok_or(UrlError::NoOpenPage)?;
                (dest, normalize_path(tail))
            } else {
                return Err(UrlError::Malformed);
            }
        }
        _ => return Err(UrlError::Malformed),
    };

    let is_file = path.starts_with("/file/");
    Ok(Target {
        dest_hash,
        path,
        fields,
        is_file,
    })
}

/// The scheme of the `nururl:` form a typed address may take.
pub const NURURL_SCHEME: &str = "nururl:";

/// A parsed `nururl:` address: the page target and the query keys that were
/// not carried, so the address field can say what it dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NurUrl {
    /// The page to open.
    pub target: Target,
    /// Query entries that were not `nn.`-prefixed fields (the hint among
    /// them), in URL order. Only parsed and named, never acted on.
    pub ignored: Vec<String>,
}

/// Whether `input` is written in the `nururl:` form (scheme case folded).
pub fn is_nururl(input: &str) -> bool {
    input
        .get(..NURURL_SCHEME.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(NURURL_SCHEME))
}

/// Parse `nururl:<hash32>[/path][?query]` into a [`Target`].
///
/// The hash is the full 32-hex destination; the path, when present, starts
/// with `/` and an absent or bare `/` one is [`DEFAULT_PATH`]. Query entries
/// are split on `&`; an `nn.<k>=<v>` entry becomes the field `var_<k>` with
/// value `<v>`, the same `var_` request variable a NomadNet backtick field
/// becomes. Any other entry (the hint is one) is not carried: it is returned
/// in [`NurUrl::ignored`] so the caller can say so. Only the address is
/// parsed here; nothing is resolved and no gateway is asked.
pub fn parse_nururl(input: &str) -> Result<NurUrl, UrlError> {
    if !is_nururl(input) {
        return Err(UrlError::Malformed);
    }
    let rest = &input[NURURL_SCHEME.len()..];
    let (address, query) = match rest.split_once('?') {
        Some((address, query)) => (address, Some(query)),
        None => (rest, None),
    };
    let hex = address
        .get(..TRUNCATED_HASH_HEX_LEN)
        .ok_or(UrlError::Malformed)?;
    let dest_hash = parse_dest_hash(hex)?;
    let path = match &address[TRUNCATED_HASH_HEX_LEN..] {
        "" | "/" => DEFAULT_PATH.to_string(),
        path if path.starts_with('/') => path.to_string(),
        _ => return Err(UrlError::Malformed),
    };
    let mut fields = Vec::new();
    let mut ignored = Vec::new();
    for entry in query.unwrap_or("").split('&').filter(|e| !e.is_empty()) {
        match entry.split_once('=') {
            Some((key, value)) if key.len() > 3 && key.starts_with("nn.") => {
                fields.push((format!("var_{}", &key[3..]), value.to_string()));
            }
            _ => ignored.push(entry.to_string()),
        }
    }
    let is_file = path.starts_with("/file/");
    Ok(NurUrl {
        target: Target {
            dest_hash,
            path,
            fields,
            is_file,
        },
        ignored,
    })
}

/// How a followed link is handled, shared with loquax's links in message
/// bodies: in `leviculum-ui`, where both programs read it.
pub use leviculum_ui::link::{classify_link, LinkKind};

/// An empty path falls back to the default page, matching the reference.
fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        DEFAULT_PATH.to_string()
    } else {
        path.to_string()
    }
}

/// Parse the `key=value|key=value` fields blob, prefixing each key with `var_`.
/// Entries without exactly one `=` are dropped (reference behaviour).
fn parse_fields(blob: &str) -> Vec<(String, String)> {
    if blob.is_empty() {
        return Vec::new();
    }
    blob.split('|')
        .filter_map(|entry| {
            if !entry.contains('=') {
                return None;
            }
            let parts: Vec<&str> = entry.split('=').collect();
            if parts.len() == 2 {
                Some((format!("var_{}", parts[0]), parts[1].to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Decode exactly [`TRUNCATED_HASH_HEX_LEN`] hex characters into a 16-byte hash.
fn parse_dest_hash(hex: &str) -> Result<[u8; 16], UrlError> {
    if hex.len() != TRUNCATED_HASH_HEX_LEN {
        return Err(UrlError::Malformed);
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = hex_nibble(hex.as_bytes()[i * 2])?;
        let lo = hex_nibble(hex.as_bytes()[i * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Ok(out)
}

/// Convert a single ASCII hex digit into its nibble value.
fn hex_nibble(c: u8) -> Result<u8, UrlError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(UrlError::Malformed),
    }
}

/// Split a `#anchor` suffix off a link target, returning the base target and the
/// anchor name (if any, and non-empty).
fn split_anchor(target: &str) -> (&str, Option<String>) {
    match target.split_once('#') {
        Some((base, anchor)) if !anchor.is_empty() => (base, Some(anchor.to_string())),
        _ => (target, None),
    }
}

/// The preset (`key=value`) field components of a link, reconstructed into the
/// backtick blob [`parse_url`] understands. Valueless components are form-field
/// placeholders (interactive input, a v1 stub) and are dropped here.
fn preset_field_blob(link: &RenderedLink) -> String {
    link.fields
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Resolve a followed link into a fetch [`Target`] and any `#anchor`.
///
/// The link target is resolved against `current_dest` (for `:/page/x.mu`
/// same-destination links) and its preset fields are carried through
/// [`parse_url`], keeping URL grammar in one place.
pub fn resolve_link(
    link: &RenderedLink,
    current_dest: Option<[u8; 16]>,
) -> Result<(Target, Option<String>), UrlError> {
    let (base, anchor) = split_anchor(&link.target);
    let blob = preset_field_blob(link);
    let url = if blob.is_empty() {
        base.to_string()
    } else {
        format!("{base}`{blob}")
    };
    let target = parse_url(&url, current_dest)?;
    Ok((target, anchor))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_HEX: &str = "0123456789abcdef0123456789abcdef";
    const HASH_BYTES: [u8; 16] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ];
    const OTHER_HASH: [u8; 16] = [0xaa; 16];

    #[test]
    fn bare_hash_uses_default_path() {
        let t = parse_url(HASH_HEX, None).unwrap();
        assert_eq!(t.dest_hash, HASH_BYTES);
        assert_eq!(t.path, DEFAULT_PATH);
        assert!(t.fields.is_empty());
        assert!(!t.is_file);
    }

    #[test]
    fn hash_with_explicit_path() {
        let t = parse_url(&format!("{HASH_HEX}:/page/about.mu"), None).unwrap();
        assert_eq!(t.dest_hash, HASH_BYTES);
        assert_eq!(t.path, "/page/about.mu");
    }

    #[test]
    fn hash_with_empty_path_uses_default() {
        let t = parse_url(&format!("{HASH_HEX}:"), None).unwrap();
        assert_eq!(t.dest_hash, HASH_BYTES);
        assert_eq!(t.path, DEFAULT_PATH);
    }

    #[test]
    fn local_url_takes_the_open_page_s_node() {
        let t = parse_url(":/page/next.mu", Some(OTHER_HASH)).unwrap();
        assert_eq!(t.dest_hash, OTHER_HASH);
        assert_eq!(t.path, "/page/next.mu");
    }

    #[test]
    fn local_url_with_empty_path_uses_default() {
        let t = parse_url(":", Some(OTHER_HASH)).unwrap();
        assert_eq!(t.dest_hash, OTHER_HASH);
        assert_eq!(t.path, DEFAULT_PATH);
    }

    #[test]
    fn local_url_without_an_open_page_is_rejected() {
        assert_eq!(parse_url(":/page/x.mu", None), Err(UrlError::NoOpenPage));
    }

    /// A request path that dropped the `:` is rejected, and says which `:`.
    /// The rejection itself is the reference's (see
    /// `reference_rejects_the_same_urls_we_do`); only the wording is ours.
    #[test]
    fn a_path_without_the_colon_names_the_missing_colon() {
        for raw in ["/page/about.mu", "/page/about.md", "/file/doc.pdf", "/"] {
            assert_eq!(
                parse_url(raw, Some(OTHER_HASH)),
                Err(UrlError::NoDestination),
                "{raw} should report the missing \":\""
            );
        }
        // The advice only fits something shaped like a path: anything else is
        // just malformed, and telling its author about the `:` would mislead.
        for raw in ["page/about.mu", "nonsense", "0123456789abcdef"] {
            assert_eq!(
                parse_url(raw, Some(OTHER_HASH)),
                Err(UrlError::Malformed),
                "{raw} should not be blamed on the \":\""
            );
        }
    }

    #[test]
    fn fields_are_split_and_var_prefixed() {
        let t = parse_url(&format!("{HASH_HEX}:/page/x.mu`a=1|b=2"), None).unwrap();
        assert_eq!(t.path, "/page/x.mu");
        assert_eq!(
            t.fields,
            vec![
                ("var_a".to_string(), "1".to_string()),
                ("var_b".to_string(), "2".to_string()),
            ]
        );
    }

    #[test]
    fn field_entries_without_single_equals_are_dropped() {
        let t = parse_url(&format!("{HASH_HEX}:/page/x.mu`a=1|bogus|c=2=3|d=4"), None).unwrap();
        assert_eq!(
            t.fields,
            vec![
                ("var_a".to_string(), "1".to_string()),
                ("var_d".to_string(), "4".to_string()),
            ]
        );
    }

    #[test]
    fn empty_fields_component_yields_no_fields() {
        let t = parse_url(&format!("{HASH_HEX}:/page/x.mu`"), None).unwrap();
        assert!(t.fields.is_empty());
    }

    #[test]
    fn file_path_is_flagged() {
        let t = parse_url(&format!("{HASH_HEX}:/file/report.pdf"), None).unwrap();
        assert!(t.is_file);
    }

    #[test]
    fn short_hash_is_malformed() {
        assert_eq!(
            parse_url("0123456789abcdef", None),
            Err(UrlError::Malformed)
        );
    }

    #[test]
    fn non_hex_hash_is_malformed() {
        let bad = "z123456789abcdef0123456789abcdef";
        assert_eq!(parse_url(bad, None), Err(UrlError::Malformed));
    }

    #[test]
    fn too_many_colons_is_malformed() {
        assert_eq!(
            parse_url(&format!("{HASH_HEX}:/page:/x.mu"), None),
            Err(UrlError::Malformed)
        );
    }

    /// Accept exactly what the reference accepts, reject exactly what it
    /// rejects.
    ///
    /// This is not read off `Browser.py`, it is measured: NomadNet 1.2.8 was
    /// installed into a scratch venv and its real `Browser.retrieve_url` driven
    /// over this table (2026-07-30), with the object built by `__new__` and only
    /// the attributes that method touches set, so the reference code itself
    /// decided each row. Every row below is that run's outcome.
    ///
    /// The row that prompted the measurement is the first one: a page in the
    /// wild carried `/page/about.md` as a link to its own node, and the reader
    /// wanted to know whether rejecting it was our bug. It is not — NomadNet
    /// raises `ValueError: Malformed URL` on it too. Accepting it here would
    /// make a page work in lnomad that stays broken in NomadNet, which is
    /// exactly the divergence this test exists to prevent.
    #[test]
    fn reference_rejects_the_same_urls_we_do() {
        let open_page = Some(OTHER_HASH);

        // Rejected by NomadNet 1.2.8 (ValueError: Malformed URL).
        for raw in ["/page/about.md", "/page/about.mu", "page/about.md"] {
            assert!(
                parse_url(raw, open_page).is_err(),
                "{raw} is rejected by the reference and must be rejected here"
            );
        }

        // Accepted by NomadNet 1.2.8, resolving as asserted.
        let local = parse_url(":/page/about.md", open_page).expect("local URL");
        assert_eq!(local.dest_hash, OTHER_HASH);
        assert_eq!(local.path, "/page/about.md");

        let full = parse_url(&format!("{HASH_HEX}:/page/about.md"), open_page).expect("full URL");
        assert_eq!(full.dest_hash, HASH_BYTES);
        assert_eq!(full.path, "/page/about.md");

        let bare = parse_url(HASH_HEX, open_page).expect("bare address");
        assert_eq!(bare.dest_hash, HASH_BYTES);
        assert_eq!(bare.path, DEFAULT_PATH);

        // With no page open, NomadNet rejects both forms.
        for raw in ["/page/about.md", ":/page/about.md"] {
            assert!(
                parse_url(raw, None).is_err(),
                "{raw} with no open page is rejected by the reference"
            );
        }
    }

    #[test]
    fn nururl_parse_table() {
        // Valid forms and what they resolve to.
        // (address, path, carried fields, ignored entries)
        type Row = (
            String,
            &'static str,
            Vec<(&'static str, &'static str)>,
            Vec<&'static str>,
        );
        let valid: [Row; 6] = [
            (format!("nururl:{HASH_HEX}"), DEFAULT_PATH, vec![], vec![]),
            (format!("nururl:{HASH_HEX}/"), DEFAULT_PATH, vec![], vec![]),
            (
                format!("NURURL:{}/page/about.mu", HASH_HEX.to_uppercase()),
                "/page/about.mu",
                vec![],
                vec![],
            ),
            (
                format!("nururl:{HASH_HEX}/page/x.mu?nn.a=1&nn.b=two"),
                "/page/x.mu",
                vec![("var_a", "1"), ("var_b", "two")],
                vec![],
            ),
            (
                format!("nururl:{HASH_HEX}/page/x.mu?hint=marcel&nn.q=solar"),
                "/page/x.mu",
                vec![("var_q", "solar")],
                vec!["hint=marcel"],
            ),
            (
                format!("nururl:{HASH_HEX}?nn.=x&plain&nn.k="),
                DEFAULT_PATH,
                vec![("var_k", "")],
                vec!["nn.=x", "plain"],
            ),
        ];
        for (raw, path, fields, ignored) in valid {
            let parsed = parse_nururl(&raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(parsed.target.dest_hash, HASH_BYTES, "{raw}");
            assert_eq!(parsed.target.path, path, "{raw}");
            let want: Vec<(String, String)> = fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            assert_eq!(parsed.target.fields, want, "{raw}");
            assert_eq!(parsed.ignored, ignored, "{raw}");
            assert!(!parsed.target.is_file, "{raw}");
        }
        let file = parse_nururl(&format!("nururl:{HASH_HEX}/file/a.pdf")).unwrap();
        assert!(file.target.is_file);

        // Invalid forms.
        for raw in [
            "nururl:".to_string(),
            "nururl:0123456789abcdef".to_string(),
            "nururl:z123456789abcdef0123456789abcdef".to_string(),
            format!("nururl:{HASH_HEX}page/x.mu"),
            format!("nururl:{HASH_HEX}:/page/x.mu"),
            format!("nururl://{HASH_HEX}"),
            HASH_HEX.to_string(),
            "nururl:é123456789abcdef0123456789abcdef".to_string(),
        ] {
            assert_eq!(parse_nururl(&raw), Err(UrlError::Malformed), "{raw}");
        }
        assert!(is_nururl("NurUrl:x"));
        assert!(!is_nururl("nur"));
    }

    #[test]
    fn classify_https_and_http_are_external() {
        assert_eq!(
            classify_link("https://example.org/x"),
            LinkKind::External("https://example.org/x".to_string())
        );
        assert_eq!(
            classify_link("http://example.org"),
            LinkKind::External("http://example.org".to_string())
        );
    }

    #[test]
    fn classify_mailto_is_external() {
        assert_eq!(
            classify_link("mailto:a@b"),
            LinkKind::External("mailto:a@b".to_string())
        );
    }

    #[test]
    fn classify_scheme_case_is_insensitive() {
        assert_eq!(
            classify_link("HTTPS://example.org"),
            LinkKind::External("HTTPS://example.org".to_string())
        );
    }

    #[test]
    fn classify_file_scheme_is_unsafe() {
        assert_eq!(
            classify_link("file:///etc/passwd"),
            LinkKind::Unsafe("file".to_string())
        );
    }

    #[test]
    fn classify_javascript_scheme_is_unsafe() {
        assert_eq!(
            classify_link("javascript:alert(1)"),
            LinkKind::Unsafe("javascript".to_string())
        );
    }

    #[test]
    fn classify_rns_hash_path_and_same_dest_are_rns() {
        assert_eq!(
            classify_link(&format!("{HASH_HEX}:/page/x.mu")),
            LinkKind::Rns
        );
        assert_eq!(classify_link(":/page/x.mu"), LinkKind::Rns);
        assert_eq!(classify_link(HASH_HEX), LinkKind::Rns);
    }

    #[test]
    fn classify_letter_leading_hash_is_rns_not_scheme() {
        // A 32-hex destination that happens to start with a letter must not be
        // read as a URI scheme.
        let hash = "abcdef0123456789abcdef0123456789";
        assert_eq!(hash.len(), TRUNCATED_HASH_HEX_LEN);
        assert_eq!(classify_link(&format!("{hash}:/page/x.mu")), LinkKind::Rns);
    }

    #[test]
    fn classify_lxmf_target_is_an_address_not_a_page() {
        // NomadNet's shorthand and the aspect it expands to, both accepted.
        assert_eq!(
            classify_link(&format!("lxmf@{HASH_HEX}")),
            LinkKind::Lxmf(HASH_HEX.to_string())
        );
        assert_eq!(
            classify_link(&format!("lxmf.delivery@{HASH_HEX}")),
            LinkKind::Lxmf(HASH_HEX.to_string())
        );
        // The prefix and the hash are both case-insensitive; the hash is
        // normalised so the toast and the clipboard agree with the wire form.
        assert_eq!(
            classify_link(&format!("LXMF@{}", HASH_HEX.to_uppercase())),
            LinkKind::Lxmf(HASH_HEX.to_string())
        );
    }

    #[test]
    fn classify_lxmf_rejects_anything_not_a_destination_hash() {
        // NomadNet accepts the target only at exactly the hash length, so a
        // shorter, longer or non-hex one is not an LXMF link at all.
        for bad in ["deadbeef", &format!("{HASH_HEX}00"), &"z".repeat(32)] {
            assert_ne!(
                classify_link(&format!("lxmf@{bad}")),
                LinkKind::Lxmf(bad.to_string()),
                "lxmf@{bad} must not pass as an address"
            );
        }
    }

    #[test]
    fn classify_an_at_sign_elsewhere_stays_a_page_path() {
        // Matching on the prefix rather than on the presence of `@` keeps a
        // page whose name contains one reachable.
        assert_eq!(
            classify_link(&format!("{HASH_HEX}:/page/a@b.mu")),
            LinkKind::Rns
        );
        assert_eq!(classify_link(":/page/a@b.mu"), LinkKind::Rns);
        // An unknown destination type is not ours to interpret.
        assert_eq!(classify_link(&format!("nnn@{HASH_HEX}")), LinkKind::Rns);
    }
}
