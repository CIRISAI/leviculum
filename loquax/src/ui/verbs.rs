//! The palette's verbs the terminal lacked against the phone (1371, the
//! parity table in `docs/src/concepts/loquax-targets.md`): reply, send
//! again, forward, mute, verify, mark unread, the photo size, the position
//! by hand, sharing it, and deleting every map. Each is the phone's verb on
//! what is open or under the cursor, done by the model's own handler; the
//! window reaches the peer verbs through [`UiModel::peer_word`].

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_ui::input::InputBuffer;
use leviculum_ui::toast::ToastKind;

use super::contacts::{Ask, PathFor};
use super::intent::{Intent, Switch, Target};
use super::photos::PhotoSize;
use super::share::Span;
use super::state::MUTED_ALWAYS;
use super::{Palette, UiEffect, UiEvent, UiModel};

const NO_MESSAGE: &str = "no message here";

/// A peer verb as [`UiModel::peer_verb`] takes it: the intent's word on a
/// peer, once it no longer asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerVerb {
    Pin(Switch),
    /// Seconds, for ever below zero, unmute at zero.
    Mute(i64),
    Archive(Switch),
    Block(Switch),
    Verify(Switch),
    MarkUnread(Switch),
    Rename(String),
    Forget {
        everywhere: bool,
    },
}

impl PeerVerb {
    /// The verb `intent` says and whom it names; none for another intent,
    /// a rename without a name and a forget not yet confirmed.
    pub fn of(intent: &Intent) -> Option<(Target, Self)> {
        Some(match intent {
            Intent::Pin { peer, on } => (*peer, Self::Pin(*on)),
            Intent::Mute { peer, secs } => (*peer, Self::Mute(*secs)),
            Intent::Archive { peer, on } => (*peer, Self::Archive(*on)),
            Intent::Block { peer, on } => (*peer, Self::Block(*on)),
            Intent::Verify { peer, on } => (*peer, Self::Verify(*on)),
            Intent::MarkUnread { peer, on } => (*peer, Self::MarkUnread(*on)),
            Intent::Rename {
                peer,
                name: Some(name),
            } => (*peer, Self::Rename(name.clone())),
            Intent::Forget {
                peer,
                everywhere,
                confirmed: true,
            } => (
                *peer,
                Self::Forget {
                    everywhere: *everywhere,
                },
            ),
            _ => return None,
        })
    }
}

impl<B> UiModel<B> {
    /// What a peer verb of a tap or the palette does in the model, on a
    /// shell whose store the model writes (the window and the terminal):
    /// pin, mute, archive, block, verify, mark unread, rename with a name,
    /// forget once confirmed. None for every other intent, and for a verb
    /// that still asks.
    pub fn peer_word(&self, intent: &Intent) -> Option<UiEvent> {
        let (peer, verb) = PeerVerb::of(intent)?;
        self.peer_verb(peer, verb)
    }

    /// [`UiModel::peer_word`] of a verb already taken out of its intent.
    pub fn peer_verb(&self, peer: Target, verb: PeerVerb) -> Option<UiEvent> {
        let peer = match peer {
            Target::Peer(peer) => peer,
            Target::Open => self.conversations.get(self.selected)?.peer,
        };
        let mut state = self.peer_state(&peer);
        Some(match verb {
            PeerVerb::Pin(on) => {
                state.pinned = on.apply(state.pinned);
                UiEvent::PeerState { peer, state }
            }
            PeerVerb::Archive(on) => {
                state.archived = on.apply(state.archived);
                UiEvent::PeerState { peer, state }
            }
            PeerVerb::Block(on) => {
                state.blocked = on.apply(state.blocked);
                UiEvent::PeerState { peer, state }
            }
            PeerVerb::Mute(secs) => {
                let now = self.now.map_or(0, |now| now.unix_secs);
                state.muted_until = match secs {
                    0 => None,
                    secs if secs < 0 => Some(MUTED_ALWAYS),
                    secs => Some(now.saturating_add(secs)),
                };
                UiEvent::PeerState { peer, state }
            }
            PeerVerb::Verify(on) => UiEvent::Verify {
                peer,
                verified: on.apply(self.verified(&peer)),
            },
            PeerVerb::MarkUnread(on) => {
                let unread = self
                    .conversations
                    .iter()
                    .any(|c| c.peer == peer && c.unread > 0);
                UiEvent::MarkUnread {
                    peer,
                    unread: on.apply(unread),
                }
            }
            PeerVerb::Rename(name) => UiEvent::Rename { peer, name },
            PeerVerb::Forget { everywhere } => UiEvent::DeleteConversation { peer, everywhere },
        })
    }

    /// The open conversation's peer and the message under the cursor's
    /// key and id, or the toast that says there is none.
    fn under_cursor(&mut self) -> Option<([u8; 16], u64, Option<[u8; 32]>)> {
        let found = self.conversations.get(self.selected).and_then(|c| {
            c.messages
                .get(self.message_cursor)
                .map(|m| (c.peer, m.activity, m.id))
        });
        if found.is_none() {
            self.toast(ToastKind::Info, NO_MESSAGE.to_string());
        }
        found
    }

    /// The reply the open conversation's draft holds, if any.
    pub(super) fn open_reply(&self) -> Option<([u8; 16], [u8; 32])> {
        let c = self.conversations.get(self.selected)?;
        c.draft.reply.map(|target| (c.peer, target))
    }

    /// The palette's `send again`: the message under the cursor, one of
    /// ours that failed, goes again as its red mark sends it.
    pub(super) fn send_again_here(&mut self) -> Vec<UiEffect> {
        let Some((peer, key, _)) = self.under_cursor() else {
            return Vec::new();
        };
        let effects = self.send_again(peer, key);
        if effects.is_empty() {
            self.toast(
                ToastKind::Info,
                "only a message of ours that failed goes again".to_string(),
            );
        }
        effects
    }

    /// The palette's `mute`: the open conversation silent until unmuted,
    /// or loud again.
    pub(super) fn toggle_mute(&mut self) -> Vec<UiEffect> {
        let Some(peer) = self.conversations.get(self.selected).map(|c| c.peer) else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return Vec::new();
        };
        let muted = self.muted(&peer);
        let secs = if muted { 0 } else { -1 };
        let Some(UiEvent::PeerState { state, .. }) = self.peer_word(&Intent::Mute {
            peer: Target::Peer(peer),
            secs,
        }) else {
            return Vec::new();
        };
        let effects = self.set_peer_state(peer, state);
        let label = self.names.label(&peer, self.lang);
        let text = match muted {
            true => format!("{label} is no longer muted"),
            false => format!("muted {label}: messages count, nothing rings"),
        };
        self.toast(ToastKind::Info, text);
        effects
    }

    /// The palette's `verify`: the open peer's identity marked compared
    /// in person, or no longer.
    pub(super) fn toggle_verified(&mut self) {
        let Some(peer) = self.conversations.get(self.selected).map(|c| c.peer) else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return;
        };
        let verified = !self.verified(&peer);
        self.set_verified(peer, verified);
    }

    /// The palette's `mark unread`: the open conversation unread, or read.
    pub(super) fn toggle_unread(&mut self) {
        let Some((peer, unread)) = self
            .conversations
            .get(self.selected)
            .map(|c| (c.peer, c.unread > 0))
        else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return;
        };
        match unread {
            true => self.mark_read(peer),
            false => self.mark_unread(peer),
        }
        let text = match unread {
            true => "marked read",
            false => "marked unread",
        };
        self.toast(ToastKind::Info, text.to_string());
    }

    /// The palette's `photo size`: the next of the three tiers, kept.
    pub(super) fn next_photo_size(&mut self) -> Vec<UiEffect> {
        let at = PhotoSize::ALL
            .iter()
            .position(|s| *s == self.photo_size)
            .unwrap_or(0);
        let size = PhotoSize::ALL[(at + 1) % PhotoSize::ALL.len()];
        self.photo_size = size;
        let words = match size {
            PhotoSize::Small => "small: 800 px, for slow links",
            PhotoSize::Phone => "phone: 1600 px, as Columba shows it",
            PhotoSize::Original => "original: as taken, only its metadata goes",
        };
        self.toast(ToastKind::Info, format!("photo size {words}"));
        alloc::vec![UiEffect::Remember {
            key: "photo_size",
            value: size.word(),
        }]
    }

    /// The palette's `stop sharing`: the open conversation's share ends.
    pub(super) fn stop_share_here(&mut self) -> Vec<UiEffect> {
        let Some(peer) = self.conversations.get(self.selected).map(|c| c.peer) else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return Vec::new();
        };
        if self.share(&peer).is_none() {
            self.toast(
                ToastKind::Info,
                "your position is not shared here".to_string(),
            );
            return Vec::new();
        }
        self.stop_share(peer)
    }

    /// The rows of the palette while it asks one of these verbs.
    pub(super) fn verb_ask_rows(&self, ask: Ask) -> Option<Vec<String>> {
        Some(match ask {
            Ask::Forward { .. } => alloc::vec![
                super::strings::StrId::AskForwardTo
                    .text(self.lang)
                    .to_string(),
                "Enter sends the copy".to_string(),
            ],
            Ask::Position => {
                let mut rows = alloc::vec![
                    "your position: latitude, longitude, like 52.5163, 13.3777".to_string(),
                ];
                rows.push(match self.map_centre() {
                    Some(_) => "Enter alone takes the map's centre".to_string(),
                    None => "a geo: link works too".to_string(),
                });
                rows
            }
            Ask::Share(_) => alloc::vec![
                match self.bar.own_fix {
                    Some(_) => "share your position: Enter once now".to_string(),
                    None => "no position yet: :position sets it".to_string(),
                },
                "or for a while: 15m, 1h, 8h, or until (stopped)".to_string(),
            ],
            Ask::Path(what) => alloc::vec![
                match what {
                    PathFor::Picture(_) => "the picture's path".to_string(),
                    PathFor::File(_) => "the file's path".to_string(),
                    PathFor::Code => "the path of a picture of the code".to_string(),
                },
                "Enter reads it; ~ is your home".to_string(),
            ],
            Ask::DeleteAllMaps => alloc::vec![
                format!(
                    "delete all {} maps? They come back only when you load an area again.",
                    self.map_setup.regions.len()
                ),
                "Enter deletes them, Esc keeps them".to_string(),
            ],
            _ => return None,
        })
    }
}

impl<B: InputBuffer + Default> UiModel<B> {
    /// The palette's `reply`: the message under the cursor is what the
    /// next send answers, kept with the draft as the phone keeps it.
    pub(super) fn start_reply(&mut self) -> Vec<UiEffect> {
        let Some((peer, _, id)) = self.under_cursor() else {
            return Vec::new();
        };
        let Some(target) = id else {
            self.toast(
                ToastKind::Info,
                "not sent yet: reply once it has an id".to_string(),
            );
            return Vec::new();
        };
        let index = self.conversation(peer);
        let text = match self.compose_for == Some(peer) {
            true => self.compose.value().to_string(),
            false => self.conversations[index].draft.text.clone(),
        };
        self.set_draft(peer, text, Some(target));
        self.focus = super::Pane::Compose;
        self.toast(
            ToastKind::Info,
            "replying: what you send answers it".to_string(),
        );
        Vec::new()
    }

    /// The palette's `forward`: ask whom the message under the cursor goes
    /// to.
    pub(super) fn ask_forward(&mut self) {
        let Some((from, key, _)) = self.under_cursor() else {
            return;
        };
        self.palette = Some(Palette {
            ask: Some(Ask::Forward { from, key }),
            ..Palette::default()
        });
    }

    /// The palette's `position`: ask for it.
    pub(super) fn ask_position(&mut self) {
        self.palette = Some(Palette {
            ask: Some(Ask::Position),
            ..Palette::default()
        });
    }

    /// The palette's `share position`: ask for how long.
    pub(super) fn ask_share(&mut self) {
        let Some(peer) = self.conversations.get(self.selected).map(|c| c.peer) else {
            self.toast(ToastKind::Info, "no conversation here".to_string());
            return;
        };
        self.palette = Some(Palette {
            ask: Some(Ask::Share(peer)),
            ..Palette::default()
        });
    }

    /// The palette's `send picture`, `send file` and `code in a picture`:
    /// ask for the path.
    pub(super) fn ask_path(&mut self, picture: bool, code: bool) {
        let what = match (code, self.conversations.get(self.selected).map(|c| c.peer)) {
            (true, _) => PathFor::Code,
            (false, Some(peer)) if picture => PathFor::Picture(peer),
            (false, Some(peer)) => PathFor::File(peer),
            (false, None) => {
                self.toast(ToastKind::Info, "no conversation here".to_string());
                return;
            }
        };
        self.palette = Some(Palette {
            ask: Some(Ask::Path(what)),
            ..Palette::default()
        });
    }

    /// The palette's `delete all maps`: ask first, as the phone does.
    pub(super) fn ask_delete_all_maps(&mut self) {
        if self.map_setup.regions.is_empty() {
            self.toast(ToastKind::Info, "there are no maps to delete".to_string());
            return;
        }
        self.palette = Some(Palette {
            ask: Some(Ask::DeleteAllMaps),
            ..Palette::default()
        });
    }

    /// `Enter` in a palette that asks one of these verbs: the effects, or
    /// none when `ask` is another's.
    pub(super) fn answer_verb_ask(&mut self, ask: Ask, typed: &str) -> Option<Vec<UiEffect>> {
        let typed = typed.trim();
        Some(match ask {
            Ask::Forward { from, key } => {
                let lower = typed.to_lowercase();
                let to = self
                    .conversations
                    .iter()
                    .map(|c| c.peer)
                    .filter(|peer| !typed.is_empty() && self.shows(peer))
                    .find(|peer| {
                        self.names
                            .label(peer, self.lang)
                            .to_lowercase()
                            .contains(&lower)
                    });
                match to {
                    Some(to) => {
                        let label = self.names.label(&to, self.lang);
                        let effects = self.forward(from, key, to);
                        self.toast(ToastKind::Info, format!("forwarded to {label}"));
                        effects
                    }
                    None => {
                        self.toast(ToastKind::Info, format!("no conversation called {typed}"));
                        Vec::new()
                    }
                }
            }
            Ask::Position if typed.is_empty() => self.fix_at_map_centre(),
            Ask::Position => self.enter_position(typed),
            Ask::Share(peer) if typed.is_empty() || typed == "now" => {
                if self.bar.own_fix.is_none() {
                    self.toast(
                        ToastKind::Info,
                        "no position yet: :position sets it".to_string(),
                    );
                    return Some(Vec::new());
                }
                self.share_fix(peer)
            }
            Ask::Share(peer) => match span_of(typed) {
                Some(span) => self.start_share(peer, span),
                None => {
                    self.toast(
                        ToastKind::Info,
                        "how long: now, 15m, 1h, 8h or until".to_string(),
                    );
                    Vec::new()
                }
            },
            Ask::DeleteAllMaps => alloc::vec![UiEffect::DeleteAllMaps],
            Ask::Path(_) if typed.is_empty() => Vec::new(),
            Ask::Path(PathFor::Picture(peer)) => alloc::vec![UiEffect::Attach {
                peer,
                path: typed.to_string(),
                photo: true,
            }],
            Ask::Path(PathFor::File(peer)) => alloc::vec![UiEffect::Attach {
                peer,
                path: typed.to_string(),
                photo: false,
            }],
            Ask::Path(PathFor::Code) => alloc::vec![UiEffect::ScanCode {
                path: typed.to_string(),
            }],
            _ => return None,
        })
    }
}

/// The span a typed word names: the facade's words and the short ones.
fn span_of(typed: &str) -> Option<Span> {
    match typed {
        "15m" | "15" => Some(Span::Minutes15),
        "1h" | "1" => Some(Span::Hour1),
        "8h" | "8" => Some(Span::Hours8),
        "until" | "stopped" | "always" => Some(Span::UntilStopped),
        word => Span::of_word(word),
    }
}
