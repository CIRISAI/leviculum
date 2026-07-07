//! NomadNet URL parsing.
//!
//! A NomadNet page URL selects a destination and a path on it, optionally
//! carrying query fields. It mirrors `Browser.retrieve_url` from the reference
//! NomadNet text UI:
//!
//! ```text
//! <dest_hash>                     -> dest + /page/index.mu
//! <dest_hash>:/page/about.mu      -> dest + /page/about.mu
//! <dest_hash>:                    -> dest + /page/index.mu   (empty path)
//! :/page/about.mu                 -> current dest + /page/about.mu
//! <dest_hash>:/page/x.mu`a=1|b=2  -> dest + path + fields {var_a:1, var_b:2}
//! ```
//!
//! `<dest_hash>` is exactly [`TRUNCATED_HASH_HEX_LEN`] hex characters (the
//! 16-byte Reticulum truncated destination hash). Query fields follow a single
//! backtick and are `key=value` pairs joined by `|`; each key is stored with the
//! NomadNet `var_` prefix, matching how the reference browser passes URL query
//! variables to a page's request handler.

/// The default path when a URL names only a destination or an empty path,
/// matching `Browser.DEFAULT_PATH`.
pub const DEFAULT_PATH: &str = "/page/index.mu";

/// Length in hex characters of a Reticulum truncated destination hash
/// (`RNS.Reticulum.TRUNCATED_HASHLENGTH // 8 * 2` = 16 bytes = 32 chars).
pub const TRUNCATED_HASH_HEX_LEN: usize = 32;

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
    /// Only page fetches are implemented; [`crate::fetch`] rejects file targets.
    pub is_file: bool,
}

/// Errors from [`parse_url`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UrlError {
    /// The URL did not match any accepted form (bad hash length, non-hex
    /// destination, empty destination without a current one, or too many `:`).
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
        [only] => {
            // Bare destination hash -> default path.
            let dest = parse_dest_hash(only)?;
            (dest, DEFAULT_PATH.to_string())
        }
        [head, tail] => {
            if head.len() == TRUNCATED_HASH_HEX_LEN {
                let dest = parse_dest_hash(head)?;
                (dest, normalize_path(tail))
            } else if head.is_empty() {
                // Same-destination form: reuse the current destination.
                let dest = current_dest.ok_or(UrlError::Malformed)?;
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

/// How a followed link target should be handled.
///
/// A NomadNet page can carry three kinds of link: an in-mesh RNS destination we
/// fetch ourselves, an external URL we hand to the system's default handler, and
/// a URL whose scheme we refuse to open. The split is a security boundary: a
/// page comes from an untrusted node, so only a whitelisted scheme is ever
/// passed to a system handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkKind {
    /// An RNS/NomadNet destination (`<hash>:/page/...`, `<hash>`, or the
    /// same-destination `:/...` form). Followed by fetching it in-mesh.
    Rns,
    /// An external URL with a safe scheme (`http`/`https`/`mailto`), openable in
    /// the user's default handler. Carries the original URL verbatim.
    External(String),
    /// A URL with a scheme we will not hand to a system handler (`file`,
    /// `javascript`, any custom scheme). Carries the offending scheme, lowercased.
    Unsafe(String),
}

/// Classify a link target so the browser knows whether to fetch it in-mesh, open
/// it externally, or refuse it. A pure function over the target string.
///
/// RNS targets keep NomadNet's colon grammar (`<32-hex>:/page/...`, a bare
/// 32-hex hash, or the same-destination `:/...`). An external URL is recognised
/// by a URI scheme (`<alpha><alnum|+-.>* :`); only `http`, `https` and `mailto`
/// are safe to open, and every other scheme is refused. A 32-hex destination
/// prefix is always read as an RNS hash, never a scheme, so a hash that happens
/// to start with a letter is not mistaken for an external URL.
pub fn classify_link(target: &str) -> LinkKind {
    // The same-destination form starts with the colon and never has a scheme.
    if target.starts_with(':') {
        return LinkKind::Rns;
    }
    match uri_scheme(target) {
        Some(scheme) => {
            // A full-length hex destination prefix is an RNS hash, not a scheme.
            if scheme.len() == TRUNCATED_HASH_HEX_LEN
                && scheme.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return LinkKind::Rns;
            }
            match scheme.to_ascii_lowercase().as_str() {
                "http" | "https" | "mailto" => LinkKind::External(target.to_string()),
                other => LinkKind::Unsafe(other.to_string()),
            }
        }
        // No scheme and no leading colon: a bare hash or relative page path.
        None => LinkKind::Rns,
    }
}

/// The URI scheme of `s` (the run before the first `:`), if it is a well-formed
/// scheme name: a leading ASCII letter followed by letters, digits, `+`, `-` or
/// `.` (RFC 3986). Returns `None` when there is no colon or the prefix is not a
/// valid scheme (e.g. it starts with a digit, as a numeric destination hash does).
fn uri_scheme(s: &str) -> Option<&str> {
    let idx = s.find(':')?;
    let scheme = &s[..idx];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        Some(scheme)
    } else {
        None
    }
}

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
    fn same_destination_form_reuses_current() {
        let t = parse_url(":/page/next.mu", Some(OTHER_HASH)).unwrap();
        assert_eq!(t.dest_hash, OTHER_HASH);
        assert_eq!(t.path, "/page/next.mu");
    }

    #[test]
    fn same_destination_empty_path_uses_default() {
        let t = parse_url(":", Some(OTHER_HASH)).unwrap();
        assert_eq!(t.dest_hash, OTHER_HASH);
        assert_eq!(t.path, DEFAULT_PATH);
    }

    #[test]
    fn same_destination_without_current_is_malformed() {
        assert_eq!(parse_url(":/page/x.mu", None), Err(UrlError::Malformed));
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
}
