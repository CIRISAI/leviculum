//! The desktop's pieces of what the phone has from Android (1341,
//! `docs/src/concepts/loquax-targets.md`): the system's file chooser and
//! its notifications, both over the session's D-Bus, as the window's light
//! or dark already comes (1310).
//!
//! - `pick`, the window's: the freedesktop portal's `FileChooser.OpenFile`,
//!   the dialog the desktop draws for every sandboxed and unsandboxed
//!   program alike (GNOME's, KDE's, the wlroots ones'), answered on its
//!   `Request`'s `Response` signal with `file://` URIs (`file_path`).
//! - [`Notices`] and [`post`]: a notification for each conversation whose
//!   unread grew while the reader is not looking, the rule of the phone's
//!   `LoquaxService.look`: none while the screen is in front, none for a
//!   muted conversation, none at the first look. `org.freedesktop.
//!   Notifications.Notify` shows it; a desktop without a session bus or a
//!   notification daemon shows nothing, and the terminal still rings its
//!   bell.
//!
//! No GTK and no Qt: zbus is the D-Bus crate the window already carries
//! through its theme reader, pure Rust, so the musl-static terminal takes
//! it too.

use std::collections::{BTreeMap, HashMap};
#[cfg(feature = "gui")]
use std::path::PathBuf;

use loquax::ui::chat::ListRow;

/// The portal's bus name and object, from the XDG desktop portal's
/// documentation ("org.freedesktop.portal.FileChooser").
#[cfg(feature = "gui")]
const PORTAL: &str = "org.freedesktop.portal.Desktop";
#[cfg(feature = "gui")]
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// The notification server's, from the Desktop Notifications
/// Specification 1.2, "D-BUS Protocol".
const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

/// The name a notification carries, and the desktop file it is matched
/// to (`packaging/loquax/loquax.desktop`).
pub(crate) const APP: &str = "loquax";

/// One notification: the conversation's name over its newest message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub peer: [u8; 16],
    pub title: String,
    pub body: String,
}

/// One conversation as [`Notices::look`] reads it: the list row's unread
/// count, its mute and the words a notification would carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Count {
    pub peer: [u8; 16],
    pub unread: usize,
    pub muted: bool,
    pub title: String,
    pub body: String,
}

impl Count {
    /// The model's list as the phone's service reads it: the rows of
    /// [`loquax::ui::chat::list_rows`].
    pub(crate) fn of(rows: &[ListRow]) -> Vec<Count> {
        rows.iter()
            .map(|r| Count {
                peer: r.peer,
                unread: r.unread,
                muted: r.muted,
                title: r.name.clone(),
                body: r.last.clone(),
            })
            .collect()
    }
}

/// The unread counts as last looked at; `None` before the first look,
/// which only learns them, so a start does not ring for old mail.
#[derive(Debug, Default)]
pub(crate) struct Notices {
    seen: Option<BTreeMap<[u8; 16], usize>>,
}

impl Notices {
    /// The conversations of `rows` whose unread grew since the last look,
    /// muted ones aside; none while `front`, when the reader sees the
    /// list, and none for `open`, the chat in front of them.
    pub(crate) fn look(
        &mut self,
        rows: &[Count],
        front: bool,
        open: Option<[u8; 16]>,
    ) -> Vec<Notice> {
        let now: BTreeMap<[u8; 16], usize> = rows.iter().map(|r| (r.peer, r.unread)).collect();
        let notices = match &self.seen {
            None => Vec::new(),
            Some(_) if front => Vec::new(),
            Some(before) => rows
                .iter()
                .filter(|r| r.unread > before.get(&r.peer).copied().unwrap_or(0))
                .filter(|r| !r.muted && Some(r.peer) != open)
                .map(|r| Notice {
                    peer: r.peer,
                    title: r.title.clone(),
                    body: r.body.clone(),
                })
                .collect(),
        };
        self.seen = Some(now);
        notices
    }
}

/// Show `notice` through the session's notification server; the error is
/// the bus's when there is none.
pub(crate) async fn post(notice: &Notice) -> Result<(), String> {
    let bus = zbus::Connection::session()
        .await
        .map_err(|e| e.to_string())?;
    let hints: HashMap<&str, zbus::zvariant::Value<'_>> =
        HashMap::from([("desktop-entry", zbus::zvariant::Value::from(APP))]);
    bus.call_method(
        Some(NOTIFICATIONS),
        NOTIFICATIONS_PATH,
        Some(NOTIFICATIONS),
        "Notify",
        &(
            APP,
            0u32,
            APP,
            notice.title.as_str(),
            notice.body.as_str(),
            Vec::<&str>::new(),
            hints,
            -1i32,
        ),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// What the chooser is asked for.
#[cfg(feature = "gui")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    /// A picture: the chooser filters to images.
    Picture,
    /// Any file.
    File,
}

/// The portal's chooser for one file, titled `title`: the path picked,
/// `None` when the reader cancelled, an error when there is no portal.
#[cfg(feature = "gui")]
pub(crate) async fn pick(title: &str, what: Pick) -> Result<Option<PathBuf>, String> {
    use futures::StreamExt;
    use zbus::zvariant::{OwnedValue, Value};
    let say = |e: zbus::Error| format!("the desktop's file chooser: {e}");
    let bus = zbus::Connection::session().await.map_err(say)?;
    // The request object's path is known before the call (the portal's
    // "Shared Request Interface"): subscribe first, so a fast answer is
    // not missed.
    let token = format!("loquax{}", std::process::id());
    let sender = bus
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let request = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let answers = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(&bus)
        .destination(PORTAL)
        .map_err(say)?
        .path(request.as_str())
        .map_err(say)?
        .interface("org.freedesktop.portal.Request")
        .map_err(say)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .map_err(say)?;
    let mut answer = answers.receive_signal("Response").await.map_err(say)?;
    let mut options: HashMap<&str, Value<'_>> =
        HashMap::from([("handle_token", Value::from(token.as_str()))]);
    if what == Pick::Picture {
        let filters = vec![("Pictures", vec![(1u32, "image/*")])];
        options.insert("filters", Value::from(filters));
    }
    bus.call_method(
        Some(PORTAL),
        PORTAL_PATH,
        Some("org.freedesktop.portal.FileChooser"),
        "OpenFile",
        &("", title, options),
    )
    .await
    .map_err(say)?;
    let Some(message) = answer.next().await else {
        return Err("the desktop's file chooser went away".to_string());
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) =
        message.body().deserialize().map_err(say)?;
    // 0 picked, 1 cancelled, 2 ended otherwise.
    if code != 0 {
        return Ok(None);
    }
    let uris = results
        .get("uris")
        .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
        .unwrap_or_default();
    Ok(uris.first().and_then(|uri| file_path(uri)))
}

/// The local path of a `file://` URI (RFC 8089), its `%XX` escapes
/// undone byte for byte; `None` for another scheme or a broken escape.
#[cfg(feature = "gui")]
pub(crate) fn file_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    // An authority before the path: only the local host's is ours.
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(at) if &rest[..at] == "localhost" => &rest[at..],
        _ => return None,
    };
    let mut bytes = Vec::with_capacity(path.len());
    let mut chars = path.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hex = [chars.next()?, chars.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(peer: u8, unread: usize, muted: bool) -> Count {
        Count {
            peer: [peer; 16],
            unread,
            muted,
            title: format!("peer {peer}"),
            body: "hello".to_string(),
        }
    }

    /// The phone's rule: the first look learns, a grown count notifies
    /// once, a muted one never, the open chat never, the front nothing.
    #[test]
    fn a_grown_unread_notifies_once_and_the_rule_holds() {
        let mut notices = Notices::default();
        assert!(notices.look(&[row(1, 3, false)], false, None).is_empty());
        let grown = notices.look(&[row(1, 4, false), row(2, 1, true)], false, None);
        assert_eq!(grown.len(), 1);
        assert_eq!(grown[0].title, "peer 1");
        assert_eq!(grown[0].body, "hello");
        assert!(notices.look(&[row(1, 4, false)], false, None).is_empty());
        assert!(notices
            .look(&[row(1, 5, false)], false, Some([1; 16]))
            .is_empty());
        assert!(notices.look(&[row(1, 6, false)], true, None).is_empty());
        assert_eq!(notices.look(&[row(1, 7, false)], false, None).len(), 1);
    }

    #[cfg(feature = "gui")]
    #[test]
    fn a_file_uri_is_the_path_it_escapes() {
        assert_eq!(
            file_path("file:///home/a/My%20Pictures/caf%C3%A9.jpg"),
            Some(PathBuf::from("/home/a/My Pictures/café.jpg"))
        );
        assert_eq!(
            file_path("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(file_path("file://elsewhere/tmp/x"), None);
        assert_eq!(file_path("https://example.org/x"), None);
        assert_eq!(file_path("file:///tmp/%zz"), None);
        assert_eq!(file_path("file:///tmp/%2"), None);
    }
}
