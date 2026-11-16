//! Which configuration directory a client tool took, and why (#471, order
//! 1675).
//!
//! The lookup order itself is Python's and does not move: `/etc/reticulum`
//! when `/etc/reticulum/config` exists, then `~/.config/reticulum` when its
//! `config` exists, then `~/.reticulum`
//! (`reference/Reticulum/RNS/Reticulum.py:229-236`; the canonical statement
//! for users is the "Configuration directory" section of
//! `docs/src/guide/installation.md`). What this module adds is the voice: a
//! tool that cannot reach or authenticate to its daemon says which directory
//! it resolved, why, which socket it dialled, and which other candidate
//! directories hold a `config` of their own. A host with the package's
//! `/etc/reticulum/config` and the user's daemon on `~/.reticulum` otherwise
//! fails with "authentication failed" and nothing to act on.
//!
//! One helper for every tool, so the wording is the same everywhere.

use std::path::{Path, PathBuf};

/// Why a directory was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirReason {
    /// The caller named it (`--config`).
    Flag,
    /// `<system>/config` exists, the first rule of the order.
    SystemConfig,
    /// No system config, and `~/.config/reticulum/config` exists.
    XdgConfig,
    /// Neither of the two above: the final fallback `~/.reticulum`.
    Fallback,
}

/// The directory a tool resolved, the reason, and the other candidates that
/// hold a `config` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDirChoice {
    /// The directory in use.
    pub dir: PathBuf,
    /// Why it is the one in use.
    pub reason: DirReason,
    /// The other candidate directories of the lookup order that hold a
    /// `config` file, in lookup order. The ones a user may have meant.
    pub others: Vec<PathBuf>,
    system_dir: PathBuf,
    home: Option<PathBuf>,
}

/// The system directory of the lookup order.
pub const SYSTEM_CONFIG_DIR: &str = "/etc/reticulum";

/// The three candidates of the lookup order, in order: the system
/// directory, `~/.config/reticulum`, `~/.reticulum`. Without a home the
/// last two are relative to the current directory, as the resolver has
/// always done.
pub fn candidates(system_dir: &Path, home: Option<&Path>) -> [PathBuf; 3] {
    let home = home.unwrap_or(Path::new("."));
    [
        system_dir.to_path_buf(),
        home.join(".config/reticulum"),
        home.join(".reticulum"),
    ]
}

impl ConfigDirChoice {
    /// Resolve against the real filesystem: `flag` when given, else the
    /// lookup order over `/etc/reticulum` and `$HOME`.
    pub fn resolve(flag: Option<&Path>) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::resolve_with(flag, Path::new(SYSTEM_CONFIG_DIR), home.as_deref(), |p| {
            p.is_file()
        })
    }

    /// The pure resolver: `config_file_exists` stands in for the filesystem
    /// so tests drive every layout without touching `/etc` or `$HOME`.
    pub fn resolve_with<F: Fn(&Path) -> bool>(
        flag: Option<&Path>,
        system_dir: &Path,
        home: Option<&Path>,
        config_file_exists: F,
    ) -> Self {
        let [system, xdg, dot] = candidates(system_dir, home);
        let (dir, reason) = if let Some(flag) = flag {
            (flag.to_path_buf(), DirReason::Flag)
        } else if config_file_exists(&system.join("config")) {
            (system.clone(), DirReason::SystemConfig)
        } else if config_file_exists(&xdg.join("config")) {
            (xdg.clone(), DirReason::XdgConfig)
        } else {
            (dot.clone(), DirReason::Fallback)
        };
        let others = [system, xdg, dot]
            .into_iter()
            .filter(|c| *c != dir && config_file_exists(&c.join("config")))
            .collect();
        Self {
            dir,
            reason,
            others,
            system_dir: system_dir.to_path_buf(),
            home: home.map(Path::to_path_buf),
        }
    }

    /// `path` with the home directory written as `~`, for reading; the
    /// `--config` suggestion keeps the full path so it can be pasted.
    pub fn short(&self, path: &Path) -> String {
        if let Some(home) = &self.home {
            if let Ok(rest) = path.strip_prefix(home) {
                return Path::new("~").join(rest).display().to_string();
            }
        }
        path.display().to_string()
    }

    /// One line: the directory in use and why.
    ///
    /// `config /etc/reticulum (taken because /etc/reticulum/config exists)`
    pub fn why(&self) -> String {
        let dir = self.short(&self.dir);
        let system_config = self.short(&self.system_dir.join("config"));
        let [_, xdg, _] = candidates(&self.system_dir, self.home.as_deref());
        let xdg_config = self.short(&xdg.join("config"));
        let because = match self.reason {
            DirReason::Flag => "given by --config".to_string(),
            DirReason::SystemConfig => format!("taken because {system_config} exists"),
            DirReason::XdgConfig => {
                format!("taken because {system_config} does not exist and {xdg_config} does")
            }
            DirReason::Fallback => {
                format!("the fallback, neither {system_config} nor {xdg_config} exists")
            }
        };
        format!("config {dir} ({because})")
    }

    /// The lines a tool prints under a connect or authentication failure:
    /// [`Self::why`] with the socket it dialled, then one line per other
    /// candidate holding a `config`.
    ///
    /// ```text
    /// config /etc/reticulum (taken because /etc/reticulum/config exists), socket @rns/default
    /// ~/.reticulum also holds a config; if your daemon runs on it, pass --config /home/lew/.reticulum
    /// ```
    pub fn hint(&self, socket: &str) -> String {
        let mut out = format!("{}, socket {socket}", self.why());
        for other in &self.others {
            out.push_str(&format!(
                "\n{} also holds a config; if your daemon runs on it, pass --config {}",
                self.short(other),
                other.display()
            ));
        }
        out
    }

    /// [`Self::hint`] with the socket this process dials for `instance_name`:
    /// the shared-instance transport the loaded config set
    /// ([`crate::daemon_rpc::load_config`]), so call it after loading.
    pub fn hint_for(&self, instance_name: &str) -> String {
        self.hint(
            &crate::interfaces::shared_instance_transport()
                .describe(&format!("rns/{instance_name}")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/alice";

    fn choice(flag: Option<&str>, present: &[&str]) -> ConfigDirChoice {
        ConfigDirChoice::resolve_with(
            flag.map(Path::new),
            Path::new("/etc/reticulum"),
            Some(Path::new(HOME)),
            |p| present.iter().any(|q| p == Path::new(q)),
        )
    }

    /// The hamster layout of 2026-11-11: the package's system config and the
    /// user's own daemon on `~/.reticulum`. The order takes the system
    /// directory; the hint names it, says why, and points at the other.
    // Unix paths and sockets (CIRIS fork: the Windows lane).
    #[cfg(unix)]
    #[test]
    fn both_candidates_present_names_the_choice_and_the_other() {
        let c = choice(
            None,
            &["/etc/reticulum/config", "/home/alice/.reticulum/config"],
        );
        assert_eq!(c.dir, PathBuf::from("/etc/reticulum"));
        assert_eq!(c.reason, DirReason::SystemConfig);
        assert_eq!(c.others, vec![PathBuf::from("/home/alice/.reticulum")]);
        assert_eq!(
            c.hint("@rns/default"),
            "config /etc/reticulum (taken because /etc/reticulum/config exists), \
             socket @rns/default\n\
             ~/.reticulum also holds a config; if your daemon runs on it, \
             pass --config /home/alice/.reticulum"
        );
    }

    // Unix paths and sockets (CIRIS fork: the Windows lane).

    #[cfg(unix)]
    #[test]
    fn xdg_reason_names_both_files() {
        let c = choice(None, &["/home/alice/.config/reticulum/config"]);
        assert_eq!(c.reason, DirReason::XdgConfig);
        assert!(c.others.is_empty());
        assert_eq!(
            c.why(),
            "config ~/.config/reticulum (taken because /etc/reticulum/config \
             does not exist and ~/.config/reticulum/config does)"
        );
    }

    // Unix paths and sockets (CIRIS fork: the Windows lane).

    #[cfg(unix)]
    #[test]
    fn fallback_lists_no_others_when_none_hold_a_config() {
        let c = choice(None, &[]);
        assert_eq!(c.dir, PathBuf::from("/home/alice/.reticulum"));
        assert_eq!(c.reason, DirReason::Fallback);
        assert!(c.others.is_empty());
        assert_eq!(
            c.hint("@rns/default"),
            "config ~/.reticulum (the fallback, neither /etc/reticulum/config \
             nor ~/.config/reticulum/config exists), socket @rns/default"
        );
    }

    /// `--config` wins, and every candidate with a config that is not the
    /// flag's directory is still named, in lookup order.
    #[test]
    fn flag_wins_and_the_candidates_stay_listed() {
        let c = choice(
            Some("/srv/rns"),
            &[
                "/etc/reticulum/config",
                "/home/alice/.config/reticulum/config",
                "/home/alice/.reticulum/config",
            ],
        );
        assert_eq!(c.dir, PathBuf::from("/srv/rns"));
        assert_eq!(c.reason, DirReason::Flag);
        assert_eq!(
            c.others,
            vec![
                PathBuf::from("/etc/reticulum"),
                PathBuf::from("/home/alice/.config/reticulum"),
                PathBuf::from("/home/alice/.reticulum"),
            ]
        );
        assert!(c.why().ends_with("(given by --config)"), "{}", c.why());
    }

    /// The flag naming a candidate does not list that candidate as "other".
    #[test]
    fn flag_on_a_candidate_is_not_its_own_other() {
        let c = choice(
            Some("/home/alice/.reticulum"),
            &["/etc/reticulum/config", "/home/alice/.reticulum/config"],
        );
        assert_eq!(c.others, vec![PathBuf::from("/etc/reticulum")]);
    }
}
