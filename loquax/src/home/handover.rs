//! Moving a mailbox to another device (1281, walk finding 24;
//! `docs/drafts/loquax-onboarding-2026-10.md`, section 4): the files under
//! the mailbox's home that carry the move's two facts.
//!
//! - [`MOVE_FILE`]: the owner opened a move. It names the token the new
//!   device joined with and when it ends. Only the mailbox's own disk can
//!   say it, so an admitted device alone never gets the identity: the
//!   authority comes from the mailbox side, as the draft asks.
//! - [`GONE_FILE`]: the identity went to the new device, which confirmed
//!   it holds everything (`/moved`). A mailbox with it does not start
//!   (`home::daemon::run` refuses), so one identity never answers on two
//!   devices at once; [`take_back`] is the owner saying otherwise.
//!
//! The requests themselves are `/move` and `/moved` ([`super::wire`]), and
//! the server answers them (`home::server`).

use std::path::Path;

/// The open move under the mailbox's home: the token in hex and its end in
/// unix seconds, on one line.
pub const MOVE_FILE: &str = "move";
/// The mailbox's identity went to another device: the unix time it did.
pub const GONE_FILE: &str = "moved-away";

/// Open a move for the device that joins with `token`, good until
/// `expires_at`. A newer move replaces an older one.
pub fn open(home: &Path, token: &[u8; 16], expires_at: i64) -> Result<(), String> {
    let path = home.join(MOVE_FILE);
    let line = format!("{} {expires_at}\n", crate::address::to_hex(token));
    leviculum_std::atomic_write_secret(&path, line.as_bytes())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// The open move's token at `now`, when one is open and not over.
pub fn token(home: &Path, now: i64) -> Option<[u8; 16]> {
    let text = std::fs::read_to_string(home.join(MOVE_FILE)).ok()?;
    let mut words = text.split_whitespace();
    let token = crate::address::parse(words.next()?).ok()?;
    let ends: i64 = words.next()?.parse().ok()?;
    (now < ends).then_some(token)
}

/// Withdraw the open move, if there is one.
pub fn withdraw(home: &Path) -> Result<(), String> {
    let path = home.join(MOVE_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Note that the identity went to another device now; the open move ends
/// with it.
pub fn mark_gone(home: &Path, now: i64) -> Result<(), String> {
    let path = home.join(GONE_FILE);
    std::fs::write(&path, format!("{now}\n"))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    withdraw(home)
}

/// When the identity went to another device, if it did and was not taken
/// back.
pub fn gone_at(home: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(home.join(GONE_FILE)).ok()?;
    Some(text.trim().parse().unwrap_or(0))
}

/// The owner takes the mailbox back on this device: it starts again. The
/// other device still holds the identity; two live copies are the owner's
/// choice from here.
pub fn take_back(home: &Path) -> Result<(), String> {
    let path = home.join(GONE_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_is_open_until_it_ends_and_gone_closes_it() {
        let home = tempfile::tempdir().expect("home");
        assert_eq!(token(home.path(), 0), None);
        open(home.path(), &[4; 16], 100).expect("opened");
        assert_eq!(token(home.path(), 99), Some([4; 16]));
        assert_eq!(token(home.path(), 100), None);
        withdraw(home.path()).expect("withdrawn");
        assert_eq!(token(home.path(), 0), None);
        withdraw(home.path()).expect("twice is no error");

        open(home.path(), &[5; 16], 100).expect("opened");
        assert_eq!(gone_at(home.path()), None);
        mark_gone(home.path(), 42).expect("gone");
        assert_eq!(gone_at(home.path()), Some(42));
        assert_eq!(token(home.path(), 0), None);
        take_back(home.path()).expect("taken back");
        assert_eq!(gone_at(home.path()), None);
    }
}
