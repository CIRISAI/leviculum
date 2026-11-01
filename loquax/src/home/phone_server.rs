//! The phones' port on a new mailbox (1176): a TCP server section of its
//! own in the Reticulum daemon's config, `[[Loquax Mailbox TCP]]`, in
//! `mode = access_point`. A phone that dials it hears no announce of the
//! mesh (the reference's `Transport.py:1193-1197`; every one wakes a
//! sleeping phone's modem), and its path requests are still answered, and
//! asked on for a destination the node does not know (`Interface.py:54`,
//! `Transport.py:2918`). Python's rnsd reads the section as lnsd does.
//!
//! Written once, on the mailbox's first start: a config that already runs
//! is never rewritten, and a section of that name already there is left
//! alone. The daemon reads its config only when it starts, so the port
//! opens on its next start; the first-start output says so.

use std::path::{Path, PathBuf};

/// The section's name.
pub const SECTION: &str = "Loquax Mailbox TCP";
/// The port it listens on: beside the usual 4242, which stays the mesh's
/// (`docs/drafts/loquax-onboarding-2026-10.md`, the phone port).
pub const PORT: u16 = 4243;

/// The section as it is written, indented as rnsd's own config indents its
/// interfaces.
pub fn block() -> String {
    format!(
        "  # Written by Loquax Mailbox on its first start: the port phones\n  \
         # dial. In access point mode it sends them no announces of the mesh\n  \
         # and still answers their path requests. The tcp_keepalive_* keys\n  \
         # (lnsd only, rnsd ignores them) set how often an idle phone's\n  \
         # connection is probed; unset is every 5 s.\n  \
         [[{SECTION}]]\n    type = TCPServerInterface\n    enabled = yes\n    \
         listen_ip = 0.0.0.0\n    listen_port = {PORT}\n    mode = access_point\n"
    )
}

/// `config` with the section in its `[interfaces]`, or `None` when a
/// section of that name is there already. The section goes right below
/// the `[interfaces]` line, so whatever top-level section follows (lnsd
/// writes `[auto]` after it) keeps its own keys; a config with no
/// `[interfaces]` gets one at its end.
pub fn with_block(config: &str) -> Option<String> {
    let header = format!("[[{SECTION}]]");
    if config.lines().any(|line| line.trim() == header) {
        return None;
    }
    let mut out = String::with_capacity(config.len() + 512);
    let mut placed = false;
    for line in config.split_inclusive('\n') {
        out.push_str(line);
        if !placed && line.trim() == "[interfaces]" {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(&block());
            out.push('\n');
            placed = true;
        }
    }
    if !placed {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[interfaces]\n\n");
        out.push_str(&block());
    }
    Some(out)
}

/// What [`add`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    /// The section is in the file now.
    Written(PathBuf),
    /// The file held a section of that name already.
    Present(PathBuf),
    /// There is no config file: the daemon writes its default on its first
    /// start, and a file holding only this section would replace that.
    NoConfig(PathBuf),
    /// The file could not be read or written (the packaged service sees
    /// `/etc/reticulum` read-only).
    Unwritable(PathBuf, String),
    /// Something on this host holds the port already, or the file names
    /// it for another section: a daemon refuses to start on a server it
    /// cannot bind, so the section is not written.
    PortTaken(PathBuf),
}

/// Whether the config names [`PORT`] as some section's `listen_port`.
fn names_port(config: &str) -> bool {
    config.lines().any(|line| {
        let line = line.split('#').next().unwrap_or("");
        line.split_once('=').is_some_and(|(key, value)| {
            key.trim() == "listen_port" && value.trim().parse::<u16>() == Ok(PORT)
        })
    })
}

/// Put the section into `config_dir/config` when it is not there. Written
/// through a file beside it and a rename, so a daemon starting meanwhile
/// reads the old file or the new one, never half of one.
pub fn add(config_dir: &Path) -> Added {
    // Bound and let go at once: only whether the port is free now.
    add_with(config_dir, || {
        std::net::TcpListener::bind(("0.0.0.0", PORT)).is_ok()
    })
}

/// [`add`], with the question whether [`PORT`] is free on this host
/// asked of `port_free`.
fn add_with(config_dir: &Path, port_free: impl Fn() -> bool) -> Added {
    let path = config_dir.join("config");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Added::NoConfig(path),
        Err(error) => return Added::Unwritable(path, error.to_string()),
    };
    let Some(new) = with_block(&text) else {
        return Added::Present(path);
    };
    if names_port(&text) || !port_free() {
        return Added::PortTaken(path);
    }
    let staged = config_dir.join(".config.loquax-mailbox");
    let written = std::fs::write(&staged, new)
        .and_then(|()| {
            let permissions = std::fs::metadata(&path)?.permissions();
            std::fs::set_permissions(&staged, permissions)
        })
        .and_then(|()| std::fs::rename(&staged, &path));
    match written {
        Ok(()) => Added::Written(path),
        Err(error) => {
            let _ = std::fs::remove_file(&staged);
            Added::Unwritable(path, error.to_string())
        }
    }
}

/// What the first start prints about it.
pub fn first_start_lines(added: &Added) -> String {
    match added {
        Added::Written(path) => format!(
            "Loquax Mailbox, first start: added [[{SECTION}]] to {}: a TCP server on port \
             {PORT} for your phones, in access point mode, so they hear no announces of the \
             mesh and still find every address they ask for. Restart the Reticulum daemon \
             (lnsd or rnsd) to open it, and admit a phone with \
             `loquax mailbox admit --server <this host>:{PORT}`.",
            path.display()
        ),
        Added::Present(path) => format!(
            "Loquax Mailbox, first start: {} already has [[{SECTION}]]; left as it is.",
            path.display()
        ),
        Added::NoConfig(path) | Added::Unwritable(path, _) | Added::PortTaken(path) => {
            let why = match added {
                Added::Unwritable(_, error) => format!("could not write it ({error})"),
                Added::PortTaken(_) => {
                    format!("port {PORT} is taken here; pick another listen_port")
                }
                _ => "it does not exist yet".to_string(),
            };
            format!(
                "Loquax Mailbox, first start: no port for phones was added to {}, {why}. \
                 Put this into its [interfaces] section and restart the Reticulum daemon, \
                 then admit a phone with `loquax mailbox admit --server <this host>:{PORT}`:\n\n{}",
                path.display(),
                block()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file lnsd writes on its first start: `[interfaces]`, then
    /// `[auto]`.
    const LNSD_LIKE: &str = leviculum_std::config::LNSD_DEFAULT_CONFIG;

    /// The section lands inside `[interfaces]`, before the `[auto]` that
    /// follows it, and the daemon's own parser reads it as an access point
    /// TCP server on the phones' port; everything else is kept.
    #[test]
    fn the_section_goes_into_interfaces_and_parses_as_an_access_point_server() {
        let out = with_block(LNSD_LIKE).expect("added");
        let section = out.find(&format!("[[{SECTION}]]")).expect("section");
        assert!(out.find("[interfaces]").expect("interfaces") < section);
        assert!(section < out.find("[auto]").expect("auto"));
        assert!(out.contains("[[Default Interface]]") && out.contains("lan = yes"));
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("config"), &out).expect("write");
        let config = leviculum_std::Config::load(dir.path().join("config")).expect("parses");
        let phones = config.interfaces.get(SECTION).expect("the section");
        assert_eq!(phones.interface_type, "TCPServerInterface");
        assert_eq!(phones.listen_port, Some(PORT));
        assert_eq!(phones.mode.as_deref(), Some("access_point"));
        assert!(config.interfaces.contains_key("Default Interface"));
    }

    /// A config that has the section already is not touched again.
    #[test]
    fn a_config_with_the_section_is_left_alone() {
        let once = with_block(LNSD_LIKE).expect("added");
        assert_eq!(with_block(&once), None);
    }

    /// A port another section names is taken, wherever the line sits.
    #[test]
    fn a_section_naming_the_port_takes_it() {
        assert!(names_port("  [[Mine]]\n    listen_port = 4243 # phones\n"));
        assert!(!names_port("  [[Mine]]\n    listen_port = 4242\n"));
        assert!(!names_port("  # listen_port = 4243\n"));
    }

    /// A config without `[interfaces]` gets one at its end.
    #[test]
    fn a_config_without_interfaces_gets_the_section_at_its_end() {
        let out = with_block("[reticulum]\n  share_instance = Yes").expect("added");
        assert!(out.starts_with("[reticulum]\n  share_instance = Yes\n"));
        assert!(out.contains("[interfaces]\n\n  # Written"));
    }

    /// `add` writes the file once, keeps its mode, says so the second time,
    /// and writes nothing where there is no config.
    #[cfg(unix)]
    #[test]
    fn add_writes_once_and_keeps_the_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let add = |dir: &Path| add_with(dir, || true);
        assert_eq!(add(dir.path()), Added::NoConfig(dir.path().join("config")));
        assert!(!dir.path().join("config").exists());
        let path = dir.path().join("config");
        std::fs::write(&path, LNSD_LIKE).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert_eq!(add(dir.path()), Added::Written(path.clone()));
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert_eq!(add(dir.path()), Added::Present(path.clone()));
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text.matches(&format!("[[{SECTION}]]")).count(), 1);
    }

    /// A port the host holds already writes nothing: the daemon would not
    /// start on a server it cannot bind.
    #[test]
    fn a_taken_port_writes_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config");
        std::fs::write(&path, LNSD_LIKE).expect("write");
        assert_eq!(
            add_with(dir.path(), || false),
            Added::PortTaken(path.clone())
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read"), LNSD_LIKE);
    }
}
