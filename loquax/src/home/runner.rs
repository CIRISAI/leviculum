//! The client runners of a home node's device, one copy for every frontend
//! that is such a client: the terminal and the window (`shell.rs` in the
//! binary) and the Android app (`loquax-android`'s session). Each frontend
//! keeps its own queue, its own tasks and its own way of saying what
//! happened; what a send, a pull, a mark and the store's side of an engine
//! event do is here (leviculum 1116, the rule against duplicated code).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::Identity;
use leviculum_lxmf::Field;
use leviculum_std::ReticulumNode;

use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};
use crate::store::Store;
use crate::ui::UiEvent;

/// How often a send runner reads its tap while it waits for its bubble to
/// be named.
const POLL: Duration = Duration::from_millis(20);

/// What a client's runners need: the home identity's key, the device's own
/// identity, and the node to open the link on.
#[derive(Clone)]
pub struct HomeClient {
    pub public_key: [u8; 64],
    pub device: Identity,
    pub node: Arc<ReticulumNode>,
}

/// One compose send on a client: handed to the home node with `/send`,
/// which delivers it. The bubble is named after the id the home node gave
/// it, and takes its later states from the archive's entries on each pull.
/// `settle` is how long the runner waits for that name to reach the
/// screen: the screen names the oldest running send, so the name has to
/// arrive before this runner's end does. `progress` hears how far a
/// submission that goes as a resource has gone (1662), as the event the
/// screen folds ([`UiEvent::SendProgress`]).
#[allow(clippy::too_many_arguments)]
pub async fn send_home(
    tap: &mut impl Outbox,
    client: &HomeClient,
    to: [u8; 16],
    body: String,
    fields: Vec<Field>,
    budget: Duration,
    settle: Duration,
    progress: &mut (dyn FnMut(UiEvent) + Send),
) -> UiEvent {
    let submission = super::wire::Submission {
        client_id: super::client::client_id(),
        destination: to,
        title: Vec::new(),
        content: body.into_bytes(),
        fields,
    };
    let mut moved = |share: f32| {
        progress(UiEvent::SendProgress(
            crate::ui::transfer::Transfer::permille(share),
        ))
    };
    let accepted = super::client::send_watching(
        &client.node,
        &client.public_key,
        &client.device,
        &submission,
        budget,
        &mut moved,
    )
    .await;
    let message_id = match accepted {
        Ok(id) => id,
        Err(error) => return UiEvent::SendDone(Err(format!("mailbox: {error}"))),
    };
    tracing::info!(
        "HOME send accepted message={}",
        crate::address::to_hex(&message_id)
    );
    if tap.submit(Command::HomeAccepted { message_id }).is_err() {
        return UiEvent::SendDone(Err(OutboxGone.to_string()));
    }
    let deadline = tokio::time::Instant::now() + settle;
    while tokio::time::Instant::now() < deadline {
        match tap.try_next_event() {
            Ok(Some(OutboxEvent::Queued { message_id: id })) if id == message_id => break,
            Ok(Some(_)) => {}
            Ok(None) => tokio::time::sleep(POLL).await,
            Err(gone) => return UiEvent::SendDone(Err(gone.to_string())),
        }
    }
    UiEvent::SendDone(Ok(()))
}

/// How a client's pull ended, for the frontend that ran it.
#[derive(Debug)]
pub struct PullEnd {
    /// The received messages that came ([`super::client::Pull::messages`]),
    /// never entries: a pull that brought only names, marks, states or the
    /// user's own sent message brought nothing new (1136).
    pub result: Result<usize, String>,
    /// For [`settle_pull`].
    pub cursor: Option<u64>,
    /// The imported history rows ([`crate::home::KIND_IMPORTED`]) new to
    /// the store, which the runner wrote there itself: the frontend shows
    /// them with [`show_imported`] when there are any (1159). Not counted in
    /// `result`, which says what was received.
    pub imported: usize,
}

/// A client's fetch: one pull from its home node, with twice a send's
/// `budget`, as on the command line. The cursor comes back for
/// [`settle_pull`]: the runner sees the fence on its tap in the same pump
/// that hands the screen the pulled messages, and the frontend writes those
/// to the store before it takes the runner's end.
pub async fn pull(
    outbox: &mut impl Outbox,
    client: &HomeClient,
    home: &Path,
    budget: Duration,
) -> PullEnd {
    let ended = |result, cursor, imported| PullEnd {
        result,
        cursor,
        imported,
    };
    match super::client::pull_into(
        outbox,
        &client.node,
        home,
        &client.public_key,
        &client.device,
        budget * 2,
    )
    .await
    {
        Ok(pull) => {
            if pull.imported.is_empty() {
                return ended(Ok(pull.messages()), pull.cursor, 0);
            }
            // Imported history goes to the store before the cursor may pass
            // it: one that cannot be kept is pulled again next time.
            match keep_imported(home, &client.public_key, &pull.imported) {
                Ok(new) => ended(Ok(pull.messages()), pull.cursor, new),
                Err(error) => ended(Err(error), None, 0),
            }
        }
        Err(error) => ended(Err(error.to_string()), None, 0),
    }
}

/// Show the history a pull wrote into `store` while the screen ran
/// ([`PullEnd::imported`]): the model takes the store's conversations as
/// [`crate::ui::UiModel::grown`] says, so the list and an open conversation
/// show it without a restart (1159). On Android the session outlives the
/// app's screen, so a restart is not something a user does.
pub fn show_imported<B: leviculum_ui::input::InputBuffer>(
    model: &mut crate::ui::UiModel<B>,
    store: &Store,
) -> Result<(), String> {
    let stored = store.restore().map_err(|e| e.to_string())?;
    show_restored(model, store, stored)
}

/// The store's conversations of the home identity's address under `home`,
/// read on a connection of its own (1464): the read [`show_imported`] does,
/// for a frontend that does it off the lock its screens wait on and hands
/// the rows to [`show_restored`] under it. Nothing there is read as none.
pub fn read_imported(
    home: &Path,
    public_key: &[u8; 64],
) -> Result<Vec<crate::ui::history::Restored>, String> {
    let address = super::lxmf_address(public_key)?;
    match Store::open_read_only(&home.join(crate::store::STORE_FILE), address) {
        Ok(Some(store)) => store.restore().map_err(|e| e.to_string()),
        Ok(None) => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

/// [`show_imported`] with the conversations [`read_imported`] read: the
/// model takes those it lacks, and the older pages of those it has come
/// from `store`. Rows read before the model's last fold are fine: a
/// conversation the fold brought is in the model by then and only its older
/// pages are asked.
pub fn show_restored<B: leviculum_ui::input::InputBuffer>(
    model: &mut crate::ui::UiModel<B>,
    store: &Store,
    stored: Vec<crate::ui::history::Restored>,
) -> Result<(), String> {
    model
        .grown(stored, |peer, before| store.earlier(peer, before, None))
        .map_err(|e| e.to_string())
}

/// Write the imported history entries of a pull
/// ([`crate::home::KIND_IMPORTED`]) into the store of the home identity's
/// address under `home`, one conversation at a time, as the import on the
/// home node wrote them: how many rows were new. An entry of another shape
/// is passed by and said in the log. The store keeps them in time order
/// below its own rows; a running screen shows them once its frontend calls
/// [`show_imported`].
pub fn keep_imported(
    home: &Path,
    public_key: &[u8; 64],
    entries: &[Vec<u8>],
) -> Result<usize, String> {
    let address = super::lxmf_address(public_key)?;
    let mut groups = std::collections::BTreeMap::<_, Vec<_>>::new();
    for bytes in entries {
        let Some((origin, peer, row)) = crate::import::archive::read_entry(bytes) else {
            tracing::warn!("HOME pull: an imported entry of another shape");
            continue;
        };
        groups.entry((origin, peer)).or_default().push(row);
    }
    let mut store = Store::open_in(home, address).map_err(|e| e.to_string())?;
    let mut new = 0;
    for ((origin, peer), rows) in &groups {
        let counts = store
            .import_conversation(*peer, origin, rows)
            .map_err(|e| e.to_string())?;
        tracing::info!(
            "HOME imported source={origin} conv={} new={} held={}",
            crate::address::to_hex(peer),
            counts.new,
            counts.held
        );
        new += counts.new;
    }
    Ok(new)
}

/// A pull ended: what it brought is in the store by now, so the cursor may
/// pass it.
pub fn settle_pull(home: &Path, cursor: Option<u64>) {
    if let Some(cursor) = cursor {
        if let Err(error) = super::client::save_cursor(home, cursor) {
            tracing::warn!("HOME cursor: {error}");
        }
    }
}

/// The marks a fold made, handed to the home node in order: the screen
/// already shows them. A mark the home node did not take is said in the
/// log; the other devices keep the message unread until this device marks
/// it again.
pub async fn hand_marks(client: HomeClient, marks: Vec<super::wire::Mark>, budget: Duration) {
    for mark in marks {
        let outcome = super::client::mark(
            &client.node,
            &client.public_key,
            &client.device,
            &mark,
            budget,
        )
        .await;
        match outcome {
            Ok(()) => {
                tracing::info!("HOME mark {} messages={}", mark.flag.word(), mark.ids.len())
            }
            Err(error) => tracing::warn!(
                "HOME mark {} messages={} not taken: {error}",
                mark.flag.word(),
                mark.ids.len()
            ),
        }
    }
}

/// Ask the home node to make a group (1307, `/group`): the event the
/// model folds, the group's address or why not. One client id for the
/// whole ask, so a lost answer asked again is the same group.
pub async fn create_group(
    client: HomeClient,
    name: String,
    members: Vec<([u8; 16], String)>,
    budget: Duration,
) -> UiEvent {
    let request = super::wire::GroupRequest {
        client_id: super::client::client_id(),
        name,
        members,
    };
    let made = super::client::group(
        &client.node,
        &client.public_key,
        &client.device,
        &request,
        budget,
    )
    .await;
    match &made {
        Ok(group) => tracing::info!("HOME group made group={}", crate::address::to_hex(group)),
        Err(error) => tracing::warn!("HOME group not made: {error}"),
    }
    UiEvent::GroupMade(made.map_err(|error| match error {
        super::client::HomeError::Unsupported(_) => crate::ui::groups::NotMade::HomeTooOld,
        error => crate::ui::groups::NotMade::Failed(error.to_string()),
    }))
}

/// Ask the home node to add members to its group (1307 G2, `/groupadd`):
/// the event the model folds. One client id for the whole ask, so a lost
/// answer asked again adds nobody twice.
pub async fn add_members(
    client: HomeClient,
    group: [u8; 16],
    members: Vec<([u8; 16], String)>,
    budget: Duration,
) -> UiEvent {
    let request = super::wire::GroupAddRequest {
        client_id: super::client::client_id(),
        group,
        members,
    };
    let added = super::client::group_add(
        &client.node,
        &client.public_key,
        &client.device,
        &request,
        budget,
    )
    .await;
    match &added {
        Ok(group) => tracing::info!("HOME group added group={}", crate::address::to_hex(group)),
        Err(error) => tracing::warn!("HOME group members not added: {error}"),
    }
    UiEvent::MembersAdded(added.map_err(|error| error.to_string()))
}

/// Ask the home node to delete the owner's group for everyone (1307 pass
/// four, `/groupdelete`): the event the model folds.
pub async fn delete_group(client: HomeClient, group: [u8; 16], budget: Duration) -> UiEvent {
    let request = super::wire::GroupDeleteRequest {
        client_id: super::client::client_id(),
        group,
    };
    let deleted = super::client::group_delete(
        &client.node,
        &client.public_key,
        &client.device,
        &request,
        budget,
    )
    .await;
    match &deleted {
        Ok(()) => tracing::info!(
            "HOME group deleted group={}",
            crate::address::to_hex(&group)
        ),
        Err(error) => tracing::warn!("HOME group not deleted: {error}"),
    }
    UiEvent::GroupDeleted(deleted.map(|()| group).map_err(|error| error.to_string()))
}

/// The petnames this device owes, handed to the home node in order (1139,
/// [`crate::ui::UiModel::take_petnames`]): the screen and the contacts
/// file have them already. The event the model folds names those the home
/// took (1635); one it did not take is said in the log and stays owed,
/// handed again with the next hand or after a restart.
pub async fn hand_petnames(
    client: HomeClient,
    petnames: Vec<([u8; 16], Option<String>)>,
    budget: Duration,
) -> UiEvent {
    let mut taken = Vec::new();
    for (peer, petname) in petnames {
        let outcome = super::client::petname(
            &client.node,
            &client.public_key,
            &client.device,
            &peer,
            petname.as_deref(),
            budget,
        )
        .await;
        let hex = crate::address::to_hex(&peer);
        match outcome {
            Ok(()) => {
                tracing::info!("HOME petname peer={hex}");
                taken.push((peer, petname));
            }
            Err(error) => tracing::warn!("HOME petname peer={hex} not taken: {error}"),
        }
    }
    UiEvent::PetnamesHanded { taken }
}

/// The bookmark changes a fold made (1579), handed to the home in order:
/// the screen has them already. One the home did not take stays pending in
/// the model and goes again before the next `/pages`.
pub async fn hand_bookmarks(
    client: HomeClient,
    rows: Vec<super::pages::BookmarkRow>,
    budget: Duration,
) {
    for row in rows {
        let outcome = super::client::bookmark(
            &client.node,
            &client.public_key,
            &client.device,
            &row,
            budget,
        )
        .await;
        match outcome {
            Ok(()) => tracing::info!("HOME bookmark deleted={} at={}", row.deleted, row.at),
            Err(error) => tracing::warn!(
                "HOME bookmark deleted={} at={} not taken: {error}",
                row.deleted,
                row.at
            ),
        }
    }
}

/// The home's pages and bookmarks after `stamp` (1579): the changes still
/// pending go first, so the answer carries their echo, then `/pages`. The
/// event the model folds, `None` when the home did not answer (a home from
/// before 1579 among them), and the device keeps the list it heard.
pub async fn home_pages(
    client: HomeClient,
    pending: Vec<super::pages::BookmarkRow>,
    stamp: u64,
    budget: Duration,
) -> Option<UiEvent> {
    hand_bookmarks(client.clone(), pending, budget).await;
    match super::client::pages(
        &client.node,
        &client.public_key,
        &client.device,
        stamp,
        budget * 2,
    )
    .await
    {
        Ok(delta) => {
            tracing::info!(
                "HOME pages pulled rows={} stamp={}",
                delta.rows.len(),
                delta.stamp
            );
            Some(UiEvent::Browse(crate::ui::browse::BrowseEvent::Home(delta)))
        }
        Err(error) => {
            tracing::warn!("HOME pages: {error}");
            None
        }
    }
}

/// What the mailbox says of itself (1642, `/about`), as the event the
/// model folds: its answer, or that it is too old to know the question;
/// `None` when it did not answer, and the page keeps what it had.
pub async fn home_about(client: HomeClient, budget: Duration) -> Option<UiEvent> {
    use crate::ui::mailbox_page::Told;
    match super::client::about(&client.node, &client.public_key, &client.device, budget).await {
        Ok(about) => {
            crate::events::home_about(
                &about.host,
                &about.version,
                crate::VERSION,
                about.started,
                about.devices.len(),
            );
            Some(UiEvent::MailboxTold(Box::new(Told::Told(about))))
        }
        Err(super::ask::HomeError::Unsupported(_)) => {
            tracing::info!("HOME about unsupported: the mailbox is older");
            Some(UiEvent::MailboxTold(Box::new(Told::Older)))
        }
        Err(error) => {
            tracing::warn!("HOME about: {error}");
            None
        }
    }
}

/// The peer states this device owes, handed to the home node in order
/// (1146, [`crate::ui::UiModel::take_peer_states`]), as petnames are: the
/// event the model folds names those the home took (1640); one it did not
/// take is said in the log and stays owed, handed again with the next hand
/// or after a restart.
pub async fn hand_peer_states(
    client: HomeClient,
    states: Vec<([u8; 16], super::wire::PeerState)>,
    budget: Duration,
) -> UiEvent {
    let mut taken = Vec::new();
    for (peer, state) in states {
        let outcome = super::client::peer_state(
            &client.node,
            &client.public_key,
            &client.device,
            &peer,
            &state,
            budget,
        )
        .await;
        let hex = crate::address::to_hex(&peer);
        match outcome {
            Ok(()) => {
                tracing::info!("HOME peerstate peer={hex}");
                taken.push((peer, state));
            }
            Err(error) => tracing::warn!("HOME peerstate peer={hex} not taken: {error}"),
        }
    }
    UiEvent::PeerStatesHanded { taken }
}

/// A received message the store already holds, which the screen drops
/// before it folds: the mailbox copy of one that came directly, perhaps
/// before a restart and below the window the screen loaded. A store that
/// cannot answer lets the message through; a duplicate beats a lost one.
pub fn already_held(store: &Store, event: &OutboxEvent) -> bool {
    let OutboxEvent::Received { message_id, .. } = event else {
        return false;
    };
    match store.holds_received(message_id) {
        Ok(held) => {
            if held {
                tracing::info!("HELD message={}", crate::address::to_hex(message_id));
            }
            held
        }
        Err(error) => {
            tracing::warn!("{error}");
            false
        }
    }
}

/// The store's side of a signature that waits on its sender's key: the
/// message's bytes while it waits, the verdict once it is in. The verdict
/// goes to the store here because the screen holds only a window of each
/// conversation (leviculum 1079).
pub fn keep_signature(store: &mut Store, event: &OutboxEvent) {
    let kept = match event {
        OutboxEvent::Unverified {
            message_id,
            source,
            packed,
        } => store.keep_unverified(message_id, source, packed),
        OutboxEvent::Verified {
            message_id, valid, ..
        } => store.settle_verdict(message_id, *valid),
        _ => return,
    };
    if let Err(error) = kept {
        crate::events::store_failed(&error.to_string());
    }
}

/// A name an announce carried, kept for the next run. The reader's own
/// is kept too (1449 item 2): the mailbox's archive hands it once, and the
/// avatar and the profile need it after a restart.
pub fn keep_name(store: &mut Store, names: &crate::names::Names, destination: [u8; 16]) {
    let Some(name) = names.announced(&destination) else {
        return;
    };
    if let Err(error) = store.save_name(destination, name) {
        crate::events::store_failed(&error.to_string());
    }
}
