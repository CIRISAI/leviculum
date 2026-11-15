//! One daemon per storage directory (#471, 1638).
//!
//! `lnsd --instance-name other` beside a running `lnsd` used to start a
//! second daemon on the same storage directory: a second writer of the same
//! identity, path table and packet-hash files, under a name the first one's
//! shared-instance bind could not catch, because the names differed.
//!
//! The daemon now holds an exclusive, non-blocking `flock` on
//! [`LOCK_FILE`](crate::storage_lock::LOCK_FILE) in its storage directory
//! for its whole life, and writes its instance name
//! and pid into the file. A second daemon that cannot take the lock reads
//! that line back and refuses with one sentence naming the holder. The
//! kernel drops the lock when the holder closes the file or dies, so a lock
//! file left behind by a killed daemon blocks nothing: its stale line is
//! overwritten by the next holder.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The file in the storage directory the running daemon holds locked.
pub const LOCK_FILE: &str = "lnsd.lock";

/// The held lock; the daemon keeps it until it exits.
#[derive(Debug)]
pub struct StorageLock {
    _file: File,
}

/// Why the lock was not taken.
#[derive(Debug)]
pub enum StorageLockError {
    /// Another daemon holds the directory. `instance` and `pid` are what it
    /// wrote into the lock file, `None` where the line could not be read
    /// (a holder between its lock and its write).
    InUse {
        /// The storage directory.
        storage: PathBuf,
        /// The holder's instance name.
        instance: Option<String>,
        /// The holder's pid.
        pid: Option<u32>,
    },
    /// The directory or the lock file could not be created or locked.
    Io {
        /// The lock file.
        path: PathBuf,
        /// The underlying error.
        error: io::Error,
    },
}

impl fmt::Display for StorageLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageLockError::InUse {
                storage,
                instance,
                pid,
            } => {
                let instance = instance.as_deref().unwrap_or("unknown");
                let pid = pid.map_or_else(|| "unknown".to_string(), |p| p.to_string());
                write!(
                    f,
                    "the storage directory {} is in use by the running daemon of instance {instance} (pid {pid}), \
                     so this one does not start; stop that daemon or give this one its own --storage",
                    storage.display()
                )
            }
            StorageLockError::Io { path, error } => {
                write!(f, "could not lock {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for StorageLockError {}

impl StorageLock {
    /// Take the lock on `storage` (created if missing) for the daemon serving
    /// `instance`, or say who holds it.
    pub fn take(storage: &Path, instance: &str) -> Result<Self, StorageLockError> {
        let path = storage.join(LOCK_FILE);
        let io_err = |error| StorageLockError::Io {
            path: path.clone(),
            error,
        };
        std::fs::create_dir_all(storage).map_err(io_err)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(io_err)?;
        if !try_lock(&file).map_err(io_err)? {
            let mut text = String::new();
            let _ = file.read_to_string(&mut text);
            let (instance, pid) = parse(&text);
            return Err(StorageLockError::InUse {
                storage: storage.to_path_buf(),
                instance,
                pid,
            });
        }
        file.set_len(0).map_err(io_err)?;
        file.seek(SeekFrom::Start(0)).map_err(io_err)?;
        writeln!(file, "instance={instance} pid={}", std::process::id()).map_err(io_err)?;
        file.flush().map_err(io_err)?;
        Ok(Self { _file: file })
    }
}

/// `Ok(true)` when the lock was taken, `Ok(false)` when another open file
/// description holds it.
#[cfg(unix)]
fn try_lock(file: &File) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: `flock` on a descriptor the caller owns for the call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return Ok(false),
            _ => return Err(error),
        }
    }
}

/// No `flock` off unix: the directory is not guarded there.
#[cfg(not(unix))]
fn try_lock(_file: &File) -> io::Result<bool> {
    Ok(true)
}

/// The holder's `instance=<name> pid=<pid>` line.
fn parse(text: &str) -> (Option<String>, Option<u32>) {
    let mut instance = None;
    let mut pid = None;
    for token in text.split_whitespace() {
        if let Some(v) = token.strip_prefix("instance=") {
            instance = Some(v.to_string());
        } else if let Some(v) = token.strip_prefix("pid=") {
            pid = v.parse().ok();
        }
    }
    (instance, pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unix paths and sockets (CIRIS fork: the Windows lane).

    #[cfg(unix)]
    #[test]
    fn a_second_holder_on_one_storage_is_refused_with_the_first_ones_name_and_pid() {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = dir.path().join("storage");
        let _first = StorageLock::take(&storage, "default").expect("first takes it");
        let second = StorageLock::take(&storage, "other").expect_err("second is refused");
        let text = second.to_string();
        assert!(
            text.contains("instance default"),
            "names the holder: {text}"
        );
        assert!(
            text.contains(&format!("pid {}", std::process::id())),
            "names the pid: {text}"
        );
    }

    #[test]
    fn the_lock_is_free_again_once_its_holder_drops_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = StorageLock::take(dir.path(), "a").expect("first");
        drop(first);
        let _again = StorageLock::take(dir.path(), "b").expect("free after drop");
        let line = std::fs::read_to_string(dir.path().join(LOCK_FILE)).expect("read");
        assert_eq!(line, format!("instance=b pid={}\n", std::process::id()));
    }

    #[test]
    fn two_storage_directories_lock_independently() {
        let a = tempfile::tempdir().expect("temp dir");
        let b = tempfile::tempdir().expect("temp dir");
        let _first = StorageLock::take(a.path(), "default").expect("a");
        let _second = StorageLock::take(b.path(), "other").expect("b");
    }
}
