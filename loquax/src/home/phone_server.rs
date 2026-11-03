//! The phones' port on a new mailbox (1176): a TCP server section of its
//! own in the Reticulum daemon's config, `[[Loquax Mailbox TCP]]`, in
//! `mode = access_point`. A phone that dials it hears no announce of the
//! mesh (the reference's `Transport.py:1193-1197`; every one wakes a
//! sleeping phone's modem), and its path requests are still answered, and
//! asked on for a destination the node does not know (`Interface.py:54`,
//! `Transport.py:2918`). Python's rnsd reads the section as lnsd does.
//!
//! Since 1346 the section probes an idle phone's connection after 240 s
//! instead of the reference's 5 s (`super::keepalive`): the phones' only
//! connection, so a long idle costs no other peer anything. rnsd ignores
//! the key and probes every 5 s, which a phone on its port pays for.
//!
//! Written once, on the mailbox's first start: a config that already runs
//! is never rewritten, and a section of that name already there is left
//! alone. The daemon reads its config only when it starts, so the port
//! opens on its next start; the first-start output says so.

use std::path::{Path, PathBuf};

/// The section's name.
pub const SECTION: &str = "Loquax Mailbox TCP";
/// The port it listens on unless `mailbox setup --phone-port` names
/// another: beside the usual 4242, which stays the mesh's
/// (`docs/drafts/loquax-onboarding-2026-10.md`, the phone port).
pub const PORT: u16 = 4243;

/// The section, listening on `port`, as it is written, indented as rnsd's
/// own config indents its interfaces.
pub fn block(port: u16) -> String {
    format!(
        "  # Written by Loquax Mailbox on its first start: the port phones\n  \
         # dial. In access point mode it sends them no announces of the mesh\n  \
         # and still answers their path requests. The tcp_keepalive_* keys\n  \
         # (lnsd only, rnsd ignores them) set how often an idle phone's\n  \
         # connection is probed: after {idle} s here, unset every 5 s.\n  \
         [[{SECTION}]]\n    type = TCPServerInterface\n    enabled = yes\n    \
         listen_ip = 0.0.0.0\n    listen_port = {port}\n    mode = access_point\n    \
         tcp_keepalive_idle_secs = {idle}\n",
        idle = super::keepalive::TCP_IDLE_SECS,
    )
}

/// `config` with the section on `port` in its `[interfaces]`, or `None`
/// when a section of that name is there already. The section goes right below
/// the `[interfaces]` line, so whatever top-level section follows (lnsd
/// writes `[auto]` after it) keeps its own keys; a config with no
/// `[interfaces]` gets one at its end.
pub fn with_block(config: &str, port: u16) -> Option<String> {
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
            out.push_str(&block(port));
            out.push('\n');
            placed = true;
        }
    }
    if !placed {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n[interfaces]\n\n");
        out.push_str(&block(port));
    }
    Some(out)
}

/// What [`add`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    /// The section is in the file now.
    Written(PathBuf),
    /// The file held a section of that name already, listening on the
    /// port given when the section says one.
    Present(PathBuf, Option<u16>),
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

/// The port a `listen_port` line names, `None` for any other line.
fn listen_port(line: &str) -> Option<u16> {
    let line = line.split('#').next().unwrap_or("");
    let (key, value) = line.split_once('=')?;
    (key.trim() == "listen_port")
        .then(|| value.trim().parse::<u16>().ok())
        .flatten()
}

/// Whether the config names `port` as some section's `listen_port`.
fn names_port(config: &str, port: u16) -> bool {
    config.lines().any(|line| listen_port(line) == Some(port))
}

/// The `listen_port` of the section [`SECTION`], when it names one.
fn section_port(config: &str) -> Option<u16> {
    let header = format!("[[{SECTION}]]");
    config
        .lines()
        .skip_while(|line| line.trim() != header)
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(listen_port)
}

/// Put the section on `port` into `config_dir/config` when it is not
/// there. Written through a file beside it and a rename, so a daemon
/// starting meanwhile reads the old file or the new one, never half of one.
pub fn add(config_dir: &Path, port: u16) -> Added {
    // Bound and let go at once: only whether the port is free now.
    add_with(config_dir, port, |port| {
        std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
    })
}

/// [`add`], with the question whether `port` is free on this host asked
/// of `port_free`.
fn add_with(config_dir: &Path, port: u16, port_free: impl Fn(u16) -> bool) -> Added {
    let path = config_dir.join("config");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Added::NoConfig(path),
        Err(error) => return Added::Unwritable(path, error.to_string()),
    };
    let Some(new) = with_block(&text, port) else {
        return Added::Present(path, section_port(&text));
    };
    if names_port(&text, port) || !port_free(port) {
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

/// What the first start prints about it, which asked for `port`.
pub fn first_start_lines(added: &Added, port: u16) -> String {
    match added {
        Added::Written(path) => format!(
            "Loquax Mailbox, first start: added [[{SECTION}]] to {}: a TCP server on port \
             {port} for your phones, in access point mode, so they hear no announces of the \
             mesh and still find every address they ask for. Restart the Reticulum daemon \
             (lnsd or rnsd) to open it, and admit a phone with \
             `loquax mailbox admit --server <this host>:{port}`.",
            path.display()
        ),
        Added::Present(path, _) => format!(
            "Loquax Mailbox, first start: {} already has [[{SECTION}]]; left as it is.",
            path.display()
        ),
        Added::NoConfig(path) | Added::Unwritable(path, _) | Added::PortTaken(path) => {
            let why = match added {
                Added::Unwritable(_, error) => format!("could not write it ({error})"),
                Added::PortTaken(_) => {
                    format!("port {port} is taken here; pick another listen_port")
                }
                _ => "it does not exist yet".to_string(),
            };
            format!(
                "Loquax Mailbox, first start: no port for phones was added to {}, {why}. \
                 Put this into its [interfaces] section and restart the Reticulum daemon, \
                 then admit a phone with `loquax mailbox admit --server <this host>:{port}`:\n\n{}",
                path.display(),
                block(port)
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
        let out = with_block(LNSD_LIKE, PORT).expect("added");
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
        // 1346: the phones' port probes an idle connection after the long
        // idle, which lnsd checks and takes.
        assert_eq!(
            phones.tcp_keepalive_idle_secs,
            Some(crate::home::keepalive::TCP_IDLE_SECS)
        );
        assert_eq!(
            phones.tcp_silent_peer_bound(),
            Ok(std::time::Duration::from_secs(242))
        );
        assert!(config.interfaces.contains_key("Default Interface"));
    }

    /// A config that has the section already is not touched again.
    #[test]
    fn a_config_with_the_section_is_left_alone() {
        let once = with_block(LNSD_LIKE, PORT).expect("added");
        assert_eq!(with_block(&once, PORT), None);
        assert_eq!(with_block(&once, 5000), None);
    }

    /// A port another section names is taken, wherever the line sits.
    #[test]
    fn a_section_naming_the_port_takes_it() {
        assert!(names_port(
            "  [[Mine]]\n    listen_port = 4243 # phones\n",
            4243
        ));
        assert!(!names_port("  [[Mine]]\n    listen_port = 4242\n", 4243));
        assert!(!names_port("  # listen_port = 4243\n", 4243));
        assert!(names_port("  [[Mine]]\n    listen_port = 5000\n", 5000));
    }

    /// A config without `[interfaces]` gets one at its end.
    #[test]
    fn a_config_without_interfaces_gets_the_section_at_its_end() {
        let out = with_block("[reticulum]\n  share_instance = Yes", PORT).expect("added");
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
        let add = |dir: &Path| add_with(dir, PORT, |_| true);
        assert_eq!(add(dir.path()), Added::NoConfig(dir.path().join("config")));
        assert!(!dir.path().join("config").exists());
        let path = dir.path().join("config");
        std::fs::write(&path, LNSD_LIKE).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert_eq!(add(dir.path()), Added::Written(path.clone()));
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert_eq!(add(dir.path()), Added::Present(path.clone(), Some(PORT)));
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
            add_with(dir.path(), PORT, |_| false),
            Added::PortTaken(path.clone())
        );
        assert_eq!(std::fs::read_to_string(&path).expect("read"), LNSD_LIKE);
    }

    /// A port other than the default is the one asked about, written, and
    /// read back from the section on a second run; a port another section
    /// names is taken without asking the host.
    #[test]
    fn another_port_is_probed_written_and_read_back() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config");
        std::fs::write(&path, LNSD_LIKE).expect("write");
        let asked = std::cell::Cell::new(None);
        let free = |port| {
            asked.set(Some(port));
            true
        };
        assert_eq!(
            add_with(dir.path(), 5000, free),
            Added::Written(path.clone())
        );
        assert_eq!(asked.get(), Some(5000));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("    listen_port = 5000\n"), "{text}");
        assert_eq!(
            add_with(dir.path(), PORT, |_| true),
            Added::Present(path.clone(), Some(5000))
        );

        let other = tempfile::tempdir().expect("dir");
        std::fs::write(other.path().join("config"), text.replace(SECTION, "Mine")).expect("write");
        assert_eq!(
            add_with(other.path(), 5000, |_| panic!("the config answers first")),
            Added::PortTaken(other.path().join("config"))
        );
    }
}
