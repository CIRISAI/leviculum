//! The Browse tab on the desktop (1369, #475): what the terminal and the
//! window do for the browser the model hosts, the phone's half of it
//! (`loquax-android`'s session) on a PC.
//!
//! The fetch is the phone's ([`loquax::browse_fetch`]) over the node the
//! messenger attached to its daemon, lent by its engine
//! ([`loquax::engine::Attached::lend`]), not a second node beside it. The
//! browser's effects ([`UiEffect::Browse`](loquax::ui::UiEffect::Browse))
//! go to that fetch, to the clipboard (OSC 52 in the terminal, the
//! window's clipboard), to the system's handler, to the download
//! directory and to the files beside the store that keep the tab's
//! bookmarks and pins. What the fetch brings comes back as the hosted
//! browser's events.
//!
//! The window draws a page's pictures with lnomad's own window code
//! ([`lnomad::gui::Pictures`]); the terminal asks for none
//! ([`lnomad::Model::no_images`]), since its map already owns the
//! terminal's picture protocol.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leviculum_core::Identity;
use lnomad::app::strings::Text;
use lnomad::{AppEvent, Bookmarks, Effect};
use loquax::browse_fetch::{self, Ask, Fetching, Told};
use loquax::engine::Attached;
use loquax::ui::browse::BrowseEvent;
use loquax::ui::UiEvent;

use crate::shell::{Model, Output};

/// The file beside the store that keeps the tab's bookmarks, lnomad's
/// `bookmarks.toml` in Loquax's home: the tab's own, not the standalone
/// browser's.
pub(crate) const BOOKMARKS_FILE: &str = "browse-bookmarks.toml";

/// The file beside it that keeps the tab's pinned names.
pub(crate) const PINS_FILE: &str = "browse-pins.toml";

/// The hosted browser as the config says: the tab on or off (`browse_tab`,
/// off unless the file says on), its bookmarks and pins read from the
/// files under `home`, which their saves write.
pub(crate) fn set_up(model: &mut Model, home: &Path, on: bool) {
    model.browse.enabled = on;
    let browser = &mut model.browse.model;
    let bookmarks = home.join(BOOKMARKS_FILE);
    browser.bookmarks = Bookmarks::load(&bookmarks);
    // The file is the import (1579): on a device of a home its bookmarks
    // wait to be handed there, stamped when the file was written, so a
    // newer word from another device wins over them. A device without a
    // home never hands them.
    let written = std::fs::metadata(&bookmarks)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
    browser.bookmarks_path = Some(bookmarks);
    let pins = home.join(PINS_FILE);
    browser.pins = lnomad::names::PinStore::load(&pins);
    browser.pins_path = Some(pins);
    let browse = &mut model.browse;
    browse.shared.loaded(&browse.model.bookmarks);
    browse.shared.import(&browse.model.bookmarks, written);
}

/// What the shell folds for the browser: one of Loquax's events, or one of
/// lnomad's own (a picture's).
#[derive(Debug)]
pub(crate) enum Folded {
    Ui(UiEvent),
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    App(AppEvent),
}

/// The window's pictures, shared with the window that draws them.
#[cfg(feature = "gui")]
pub(crate) type Pictures = Arc<std::sync::Mutex<lnomad::gui::Pictures>>;

/// The tab's platform half: the fetch, where downloads land, and the
/// window's pictures.
pub(crate) struct Browsing {
    /// The fetch's ends; none while the tab is off.
    fetching: Option<Fetching>,
    /// Where a download lands: the desktop's download directory, as the
    /// standalone browser's.
    downloads: Option<PathBuf>,
    /// The window's half of the pictures; none in the terminal.
    #[cfg(feature = "gui")]
    pub(crate) pictures: Option<Pictures>,
    /// Moves on every picture the window's half took, so the window draws
    /// it.
    #[cfg(feature = "gui")]
    pub(crate) epoch: u64,
    /// The newest text the browser copied, with its serial, for the
    /// window's clipboard.
    #[cfg(feature = "gui")]
    pub(crate) copied: Option<(u64, String)>,
}

impl Browsing {
    /// The tab's half with no fetch: the tab is off.
    pub(crate) fn off() -> Self {
        Browsing {
            fetching: None,
            downloads: None,
            #[cfg(feature = "gui")]
            pictures: None,
            #[cfg(feature = "gui")]
            epoch: 0,
            #[cfg(feature = "gui")]
            copied: None,
        }
    }

    /// The fetch started over `attached`'s node, shown `identity` on
    /// identify, the identify decisions kept under `home`.
    pub(crate) fn start(attached: &Attached, identity: Identity, home: &Path) -> Self {
        let (lend, lends) = tokio::sync::mpsc::unbounded_channel();
        let _ = lend.send(attached.lend());
        let fetching = browse_fetch::start(
            &tokio::runtime::Handle::current(),
            browse_fetch::Options {
                lends,
                identity,
                identify: home.join(browse_fetch::IDENTIFY_FILE),
            },
            // The shell reads the fetch on its own pump; nobody waits on
            // the wake.
            Arc::new(tokio::sync::Notify::new()),
        );
        let mut browsing = Browsing::off();
        browsing.fetching = Some(fetching);
        browsing.downloads = lnomad::download::download_dir();
        browsing
    }

    /// One of the browser's effects done, or handed to the fetch; what
    /// the browser is told at once is returned.
    pub(crate) fn effect(
        &mut self,
        model: &mut Model,
        effect: Effect,
        outputs: &mut Vec<Output>,
    ) -> Vec<Folded> {
        let ask = match effect {
            Effect::Navigate(target) => {
                #[cfg(feature = "gui")]
                self.with_pictures(|pictures| pictures.clear());
                Ask::Navigate(target)
            }
            Effect::Download(target) => Ask::Download(target),
            Effect::SetIdentify { dest, on } => Ask::Identify { dest, on },
            Effect::Cancel => Ask::Cancel,
            Effect::FetchImages(requests) => return self.images(model, requests),
            Effect::Copy(text) => {
                #[cfg(feature = "gui")]
                {
                    let serial = self.copied.as_ref().map_or(1, |(n, _)| n + 1);
                    self.copied = Some((serial, text.clone()));
                }
                outputs.push(Output::Copy(text));
                return Vec::new();
            }
            Effect::OpenExternal(url) => {
                outputs.push(Output::Open(url));
                return Vec::new();
            }
            Effect::SaveBookmarks => {
                let browser = &model.browse.model;
                if let Some(path) = &browser.bookmarks_path {
                    if let Err(error) = browser.bookmarks.save(path) {
                        tracing::warn!("BROWSE {}: not saved: {error}", path.display());
                    }
                }
                return Vec::new();
            }
            Effect::SavePins => {
                let browser = &model.browse.model;
                if let Some(path) = &browser.pins_path {
                    if let Err(error) = browser.pins.save(path) {
                        tracing::warn!("BROWSE {}: not saved: {error}", path.display());
                    }
                }
                return Vec::new();
            }
            Effect::SaveImage { link } => {
                #[cfg(feature = "gui")]
                if let Some(saved) = self.with_pictures(|pictures| pictures.save(link)) {
                    lnomad::transfer::say_saved(&mut model.browse.model, saved);
                }
                #[cfg(not(feature = "gui"))]
                let _ = link;
                return Vec::new();
            }
            Effect::OpenImage { link } => {
                #[cfg(feature = "gui")]
                if let Some(saved) = self.with_pictures(|pictures| pictures.save(link)) {
                    lnomad::transfer::open_saved(&mut model.browse.model, saved);
                }
                #[cfg(not(feature = "gui"))]
                let _ = link;
                return Vec::new();
            }
            // The core does these itself (`loquax::ui::browse::effects_of`).
            Effect::Write(_) | Effect::Quit => return Vec::new(),
        };
        self.ask(ask);
        Vec::new()
    }

    /// A page's pictures: the ones the window's cache holds drawn at once,
    /// the rest fetched one after another. The terminal asks for none.
    fn images(&mut self, model: &Model, requests: Vec<lnomad::app::ImageRequest>) -> Vec<Folded> {
        #[cfg(feature = "gui")]
        if let Some(pictures) = self.pictures.clone() {
            let (events, pending) = {
                let mut pictures = pictures
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                pictures.cached(&model.browse.model, requests)
            };
            self.epoch += 1;
            // Even an empty list goes: it stops the last page's queue.
            self.ask(Ask::Images(pending));
            return events.into_iter().map(Folded::App).collect();
        }
        let _ = (model, requests);
        Vec::new()
    }

    #[cfg(feature = "gui")]
    fn with_pictures<R>(&mut self, f: impl FnOnce(&mut lnomad::gui::Pictures) -> R) -> Option<R> {
        let pictures = self.pictures.as_ref()?;
        let mut pictures = pictures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.epoch += 1;
        Some(f(&mut pictures))
    }

    fn ask(&self, ask: Ask) {
        if let Some(fetching) = &self.fetching {
            // A fetch that ended has nothing more to bring either.
            let _ = fetching.asks.send(ask);
        }
    }

    /// What the fetch brought since the last look, as the browser's events.
    pub(crate) fn told(&mut self, model: &Model) -> Vec<Folded> {
        let mut folded = Vec::new();
        while let Some(told) = self
            .fetching
            .as_mut()
            .and_then(|fetching| fetching.told.try_recv().ok())
        {
            match told {
                Told::Browse(event) => folded.push(Folded::Ui(UiEvent::Browse(event))),
                Told::File { name, bytes } => {
                    folded.push(Folded::Ui(UiEvent::Browse(self.keep(&name, &bytes))))
                }
                Told::Picture { link, target, shot } => {
                    #[cfg(feature = "gui")]
                    {
                        let result = match shot {
                            browse_fetch::Shot::Started => lnomad::gui::Picture::Started,
                            browse_fetch::Shot::Bytes(bytes) => lnomad::gui::Picture::Bytes(bytes),
                            browse_fetch::Shot::Failed(why) => lnomad::gui::Picture::Failed(why),
                        };
                        if let Some(event) = self.with_pictures(|pictures| {
                            pictures.arrived(&model.browse.model, link, target, result)
                        }) {
                            folded.push(Folded::App(event));
                        }
                    }
                    #[cfg(not(feature = "gui"))]
                    let _ = (link, target, shot, model);
                }
            }
        }
        folded
    }

    /// A downloaded file kept in the download directory, under the name
    /// the standalone browser would give it; what the browser is told.
    fn keep(&self, name: &str, bytes: &[u8]) -> BrowseEvent {
        let Some(dir) = &self.downloads else {
            return BrowseEvent::DownloadFailed(Text::Raw(format!(
                "{name}: no download directory (XDG_DOWNLOAD_DIR and HOME are unset)"
            )));
        };
        match lnomad::download::save_to_dir(dir, name, bytes) {
            Ok(path) => BrowseEvent::Downloaded {
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.to_string()),
                bytes: bytes.len(),
                dir: dir.display().to_string(),
            },
            Err(error) => BrowseEvent::DownloadFailed(Text::Raw(format!("{name}: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lnomad::{Bookmark, Target};
    use tokio::sync::mpsc;

    /// The tab's half over a fetch the test plays, downloads into `dir`.
    fn rigged(
        dir: Option<PathBuf>,
    ) -> (
        Browsing,
        mpsc::UnboundedReceiver<Ask>,
        mpsc::UnboundedSender<Told>,
    ) {
        let (asks, asked) = mpsc::unbounded_channel();
        let (tell, told) = mpsc::unbounded_channel();
        let mut browsing = Browsing::off();
        browsing.fetching = Some(Fetching { asks, told });
        browsing.downloads = dir;
        (browsing, asked, tell)
    }

    fn target(path: &str) -> Target {
        Target {
            dest_hash: [0x5a; 16],
            path: path.to_string(),
            fields: Vec::new(),
            is_file: path.starts_with("/file/"),
        }
    }

    /// 1369: what the hosted browser asks of the desktop. The network's
    /// effects go to the fetch in order, the clipboard and a link out of
    /// NomadNet to the terminal's or window's outputs, the bookmarks and
    /// pins to their files under the home; a write and a quit are the
    /// core's and reach nothing here. The control: with the tab off there
    /// is no fetch, and the same effects ask nothing.
    #[test]
    fn the_browsers_effects_reach_the_fetch_the_clipboard_and_the_files() {
        let home = tempfile::tempdir().expect("home");
        let mut model = Model::new(&[]);
        set_up(&mut model, home.path(), true);
        assert!(model.browse.enabled);
        let (mut browsing, mut asked, _tell) = rigged(None);
        let mut outputs = Vec::new();
        for effect in [
            Effect::Navigate(target("/page/index.mu")),
            Effect::Download(target("/file/map.pdf")),
            Effect::SetIdentify {
                dest: [0x5a; 16],
                on: true,
            },
            Effect::Cancel,
            Effect::Copy("5a5a:/page/index.mu".to_string()),
            Effect::OpenExternal("https://reticulum.network".to_string()),
            Effect::Write("0e".repeat(16)),
            Effect::Quit,
        ] {
            assert!(browsing.effect(&mut model, effect, &mut outputs).is_empty());
        }
        let mut asks = Vec::new();
        while let Ok(ask) = asked.try_recv() {
            asks.push(ask);
        }
        assert_eq!(
            asks,
            vec![
                Ask::Navigate(target("/page/index.mu")),
                Ask::Download(target("/file/map.pdf")),
                Ask::Identify {
                    dest: [0x5a; 16],
                    on: true
                },
                Ask::Cancel,
            ]
        );
        assert_eq!(
            outputs,
            vec![
                Output::Copy("5a5a:/page/index.mu".to_string()),
                Output::Open("https://reticulum.network".to_string()),
            ]
        );

        model.browse.model.bookmarks.add(Bookmark {
            url: "5a5a:/page/index.mu".to_string(),
            title: "Marcel's node".to_string(),
        });
        browsing.effect(&mut model, Effect::SaveBookmarks, &mut outputs);
        let kept = Bookmarks::load(&home.path().join(BOOKMARKS_FILE));
        assert!(kept.contains("5a5a:/page/index.mu"), "{kept:?}");
        // The next start reads them back.
        let mut again = Model::new(&[]);
        set_up(&mut again, home.path(), true);
        assert!(again.browse.model.bookmarks.contains("5a5a:/page/index.mu"));

        let mut off = Browsing::off();
        let mut none = Vec::new();
        off.effect(
            &mut model,
            Effect::Navigate(target("/page/index.mu")),
            &mut none,
        );
        assert!(
            none.is_empty() && off.told(&model).is_empty(),
            "the control"
        );
    }

    /// What the fetch brings is the browser's: a page as it came, a file
    /// kept in the download directory under its own name and a second of
    /// the same name beside it, and without a download directory a
    /// failure that says why (the control).
    #[test]
    fn what_the_fetch_brings_is_the_browsers() {
        let downloads = tempfile::tempdir().expect("downloads");
        let model = Model::new(&[]);
        let (mut browsing, _asked, tell) = rigged(Some(downloads.path().to_path_buf()));
        let loaded = BrowseEvent::Loaded {
            micron: ">Hello".to_string(),
            title: "Marcel's node".to_string(),
        };
        tell.send(Told::Browse(loaded.clone())).expect("told");
        for _ in 0..2 {
            tell.send(Told::File {
                name: "map.pdf".to_string(),
                bytes: b"%PDF".to_vec(),
            })
            .expect("told");
        }
        let folded = browsing.told(&model);
        let events: Vec<_> = folded
            .into_iter()
            .map(|folded| match folded {
                Folded::Ui(UiEvent::Browse(event)) => event,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(events[0], loaded);
        let dir = downloads.path().display().to_string();
        let BrowseEvent::Downloaded { name, bytes, .. } = &events[1] else {
            panic!("{:?}", events[1]);
        };
        assert_eq!((name.as_str(), *bytes), ("map.pdf", 4));
        let BrowseEvent::Downloaded { name: second, .. } = &events[2] else {
            panic!("{:?}", events[2]);
        };
        assert_ne!(second, "map.pdf", "beside it, not over it");
        assert!(matches!(&events[1], BrowseEvent::Downloaded { dir: d, .. } if *d == dir));
        assert_eq!(
            std::fs::read(downloads.path().join("map.pdf")).expect("kept"),
            b"%PDF"
        );

        let (mut nowhere, _asked, tell) = rigged(None);
        tell.send(Told::File {
            name: "map.pdf".to_string(),
            bytes: b"%PDF".to_vec(),
        })
        .expect("told");
        let folded = nowhere.told(&model);
        assert!(
            matches!(
                &folded[..],
                [Folded::Ui(UiEvent::Browse(BrowseEvent::DownloadFailed(_)))]
            ),
            "{folded:?}"
        );
    }
}
