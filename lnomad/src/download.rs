//! Saving fetched `/file/` payloads to disk.
//!
//! A NomadNet file download arrives as raw bytes (see
//! [`crate::fetch::Session::download_file`]); this module owns everything that
//! happens after: deriving a safe filename from the URL path, resolving where
//! the file lands (the CLI's `--output`, the TUI's download directory, or the
//! current working directory), and de-duplicating against an existing file so
//! nothing is overwritten silently.
//!
//! The filename comes from the URL path, never from server-supplied Resource
//! metadata, and is sanitised anyway: path separators are stripped and a
//! dot-only name is replaced, so a hostile path can never escape the target
//! directory.

use std::io;
use std::path::{Path, PathBuf};

/// The fallback filename when the URL path yields nothing usable (an empty
/// basename, or a name that is only dots).
const FALLBACK_NAME: &str = "download";

/// The filename implied by a request path: everything after the last `/`,
/// sanitised so it cannot escape the directory it is saved into. Any remaining
/// path separator (`\` survives the split) is stripped, and an empty or
/// dot-only result (``, `.`, `..`) falls back to [`FALLBACK_NAME`].
pub fn basename(path: &str) -> String {
    let raw = path.rsplit('/').next().unwrap_or("");
    let name: String = raw.chars().filter(|&c| c != '/' && c != '\\').collect();
    if name.is_empty() || name.chars().all(|c| c == '.') {
        FALLBACK_NAME.to_string()
    } else {
        name
    }
}

/// Resolve where a download lands for the CLI's `--output`.
///
/// Returns `(path, explicit)`: an `--output` that is an existing directory or
/// is spelled with a trailing `/` receives `name` inside it (`explicit` =
/// `false`); any other `--output` names the exact file (`explicit` = `true`);
/// no `--output` puts `name` into `cwd`. Only a non-explicit path is
/// de-duplicated by the caller — naming the exact file opts into overwriting.
pub fn resolve_output(output: Option<&Path>, name: &str, cwd: &Path) -> (PathBuf, bool) {
    match output {
        None => (cwd.join(name), false),
        Some(out) => {
            let trailing_slash = out.as_os_str().to_string_lossy().ends_with('/');
            if trailing_slash || out.is_dir() {
                (out.join(name), false)
            } else {
                (out.to_path_buf(), true)
            }
        }
    }
}

/// The first free variant of `path`: the path itself when nothing exists
/// there, else ` (1)`, ` (2)`, ... inserted before the extension.
pub fn dedup_path(path: &Path) -> PathBuf {
    dedup_path_with(path, |p| p.exists())
}

/// [`dedup_path`] over an injectable existence check, so the numbering is a
/// pure function unit tests can drive without touching the filesystem.
fn dedup_path_with(path: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if !exists(path) {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| FALLBACK_NAME.to_string());
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    let mut n: u64 = 1;
    loop {
        let candidate = match &ext {
            Some(e) => parent.join(format!("{stem} ({n}).{e}")),
            None => parent.join(format!("{stem} ({n})")),
        };
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Save `bytes` into `dir` (created if missing) under `name`, de-duplicating
/// against existing files. Returns the path actually written.
pub fn save_to_dir(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dedup_path(&dir.join(name));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// The directory the TUI saves downloads into: `$XDG_DOWNLOAD_DIR`, falling
/// back to `$HOME/Downloads`. `None` only when neither variable is set.
pub fn download_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_takes_the_last_path_component() {
        assert_eq!(basename("/file/hello.bin"), "hello.bin");
        assert_eq!(basename("/file/docs/manual.pdf"), "manual.pdf");
        assert_eq!(basename("/file/a/b/c.pdf"), "c.pdf");
    }

    #[test]
    fn basename_cannot_escape_the_target_dir() {
        // Traversal components and separators are neutralised.
        assert_eq!(basename("/file/.."), FALLBACK_NAME);
        assert_eq!(basename("/file/../../etc/passwd"), "passwd");
        assert_eq!(basename("/file/evil\\..\\name.bin"), "evil..name.bin");
        assert_eq!(basename("/file/"), FALLBACK_NAME);
        assert_eq!(basename(""), FALLBACK_NAME);
        assert_eq!(basename("/file/."), FALLBACK_NAME);
    }

    #[test]
    fn resolve_omitted_output_uses_cwd() {
        let (path, explicit) = resolve_output(None, "a.bin", Path::new("/work"));
        assert_eq!(path, Path::new("/work/a.bin"));
        assert!(!explicit);
    }

    #[test]
    fn resolve_trailing_slash_output_is_a_directory() {
        // The directory need not exist: the trailing slash alone marks it.
        let (path, explicit) = resolve_output(
            Some(Path::new("/nonexistent/dl/")),
            "a.bin",
            Path::new("/work"),
        );
        assert_eq!(path, Path::new("/nonexistent/dl/a.bin"));
        assert!(!explicit);
    }

    #[test]
    fn resolve_existing_directory_output_receives_the_basename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, explicit) = resolve_output(Some(dir.path()), "a.bin", Path::new("/work"));
        assert_eq!(path, dir.path().join("a.bin"));
        assert!(!explicit);
    }

    #[test]
    fn resolve_file_output_is_explicit_and_verbatim() {
        let (path, explicit) = resolve_output(
            Some(Path::new("/tmp/renamed.bin")),
            "a.bin",
            Path::new("/work"),
        );
        assert_eq!(path, Path::new("/tmp/renamed.bin"));
        assert!(explicit);
    }

    #[test]
    fn dedup_leaves_a_free_path_alone() {
        let path = Path::new("/dl/a.bin");
        assert_eq!(dedup_path_with(path, |_| false), path);
    }

    #[test]
    fn dedup_numbers_before_the_extension() {
        let taken = [PathBuf::from("/dl/a.bin"), PathBuf::from("/dl/a (1).bin")];
        let free = dedup_path_with(Path::new("/dl/a.bin"), |p| taken.contains(&p.to_path_buf()));
        assert_eq!(free, Path::new("/dl/a (2).bin"));
    }

    #[test]
    fn dedup_numbers_an_extensionless_name_at_the_end() {
        let taken = [PathBuf::from("/dl/README")];
        let free = dedup_path_with(Path::new("/dl/README"), |p| {
            taken.contains(&p.to_path_buf())
        });
        assert_eq!(free, Path::new("/dl/README (1)"));
    }

    #[test]
    fn save_to_dir_creates_writes_and_dedups() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("sub");
        let first = save_to_dir(&target, "a.bin", b"one").expect("first save");
        assert_eq!(first, target.join("a.bin"));
        assert_eq!(std::fs::read(&first).expect("read first"), b"one");
        // A second download of the same name lands beside it, not over it.
        let second = save_to_dir(&target, "a.bin", b"two").expect("second save");
        assert_eq!(second, target.join("a (1).bin"));
        assert_eq!(std::fs::read(&first).expect("first intact"), b"one");
        assert_eq!(std::fs::read(&second).expect("read second"), b"two");
    }
}
