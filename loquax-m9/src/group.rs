//! A group's screens on the M9 and the keys that work them (slice six,
//! 1366): the member sheet, "New group" over the contact list with its
//! ticks and then its name, the invitation's Join, and what the board
//! asks its home for each group verb the model hands out.
//!
//! The scene carries the nodes already (`loquax::ui::group_scene`): the
//! sheet's members, standing and verbs, the yes/no question, the picker's
//! rows and ticks, the name's trust sentence. A phone taps a node and runs
//! its [`Action`]; the M9 presses a key and runs the same `Action`. The
//! terminal reaches these verbs through its palette only
//! (`loquax::keys`, the rows `new group` to `start again`, no chord), and
//! the M9 has no palette, so the key table here ([`ROWS`]) names which key
//! runs which node's action. Nothing new enters the model: every key
//! either runs an action the scene offers, moves the M9's own cursor over
//! a sheet's rows, or goes to the model as the terminal's key.
//!
//! The cursor over the sheet's and the picker's rows is the M9's, as a
//! finger is the phone's: the model has none for them. It is a row index,
//! kept by the caller and drawn through [`crate::screen::Look::row`]; it
//! goes back to the first row whenever another screen opens.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::input::{KeyCode, KeyEvent};
use leviculum_ui::keymap::ScrollCmd;
use leviculum_ui::style::{RLine, RStyle};
use leviculum_ui::text::fit_width;
use loquax::home::ask::GroupVerb;
use loquax::home::wire;
use loquax::ui::group_scene::{MemberSheet, NewGroupNode};
use loquax::ui::groups::{self, Step, Verb};
use loquax::ui::intent::Intent;
use loquax::ui::scene::{Action, Scene};
use loquax::ui::strings::{StrId, Text};
use loquax::ui::{update, Pane, UiEffect, UiEvent, UiModel};

use crate::field::Field;
use crate::keys::M9Key;
use crate::screen::{self, cut, fill, muted, narrow, painted, push, push_avatar, Look, BAR};

/// Where the keys are, for a group's keys: which of the group's screens is
/// up, or which pane of the plain screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// The chats list.
    List,
    /// An open chat: a group's, or one holding an invitation.
    Chat,
    /// The member sheet.
    Sheet,
    /// The sheet asking yes or no.
    Ask,
    /// "New group": the friends with their ticks.
    Pick,
    /// "New group": its name.
    Name,
    /// The compose line, or anything else: no group key.
    Other,
}

impl Place {
    /// Where `model`'s keys are: an open sheet or "New group" before the
    /// pane, as the scene draws them over it.
    pub fn of<B>(model: &UiModel<B>) -> Self {
        match (model.group_sheet, &model.group_draft) {
            (Some(sheet), _) if sheet.confirm.is_some() => Self::Ask,
            (Some(_), _) => Self::Sheet,
            (None, Some(draft)) if draft.step == Step::Pick => Self::Pick,
            (None, Some(_)) => Self::Name,
            (None, None) => match model.focus {
                Pane::Conversations => Self::List,
                Pane::Messages => Self::Chat,
                _ => Self::Other,
            },
        }
    }
}

/// One row of the group's key table: a verb, the key that does it, where.
#[derive(Clone, Copy, Debug)]
pub struct KeyRow {
    pub verb: &'static str,
    pub key: M9Key,
    pub place: Place,
}

const fn row(verb: &'static str, key: M9Key, place: Place) -> KeyRow {
    KeyRow { verb, key, place }
}

/// The group's keys, every verb the scene's group nodes carry. A letter is
/// the verb's first letter where it is free; the chat's `m` is the group
/// bar's, where the terminal's `m` opens the peer's map from the list and
/// a group's bar on the phone opens its members instead of the map.
pub const ROWS: [KeyRow; 21] = [
    row("new group", M9Key::Char('n'), Place::List),
    row("members", M9Key::Char('m'), Place::Chat),
    row("join", M9Key::Enter, Place::Chat),
    row("up", M9Key::Up, Place::Sheet),
    row("down", M9Key::Down, Place::Sheet),
    row("leave", M9Key::Char('l'), Place::Sheet),
    row("add members", M9Key::Char('a'), Place::Sheet),
    row("remove", M9Key::Char('r'), Place::Sheet),
    row("delete group", M9Key::Char('d'), Place::Sheet),
    row("start again", M9Key::Char('s'), Place::Sheet),
    row("close", M9Key::Back, Place::Sheet),
    row("yes", M9Key::Char('y'), Place::Ask),
    row("no", M9Key::Char('n'), Place::Ask),
    row("up", M9Key::Up, Place::Pick),
    row("down", M9Key::Down, Place::Pick),
    row("tick", M9Key::Char(' '), Place::Pick),
    row("next", M9Key::Enter, Place::Pick),
    row("close", M9Key::Back, Place::Pick),
    row("type the name", M9Key::Char('a'), Place::Name),
    row("create", M9Key::Enter, Place::Name),
    row("close", M9Key::Back, Place::Name),
];

/// What a key does on a group's screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    /// Run this node's action: the intent a tap on it runs.
    Run(Intent),
    /// Move the M9's cursor to this row.
    Move(usize),
    /// Nothing: the key belongs to the screen, which has no use for it
    /// now (a verb not offered, a disabled Next).
    Stay,
}

/// What `key` does on the group's screen of `scene` at `place`, the
/// M9's cursors where `look` has them (the sheet's row, the chat's
/// bubble); none when the key is not the group's and goes to the model as
/// the terminal's.
pub fn hit(scene: &Scene, place: Place, look: &Look, key: &KeyEvent) -> Option<Hit> {
    let code = key.code;
    let row = look.row;
    match place {
        Place::List => (code == KeyCode::Char('n')).then_some(Hit::Run(Intent::Group(Verb::New))),
        Place::Chat => chat_hit(scene, look.cursor, code),
        Place::Sheet | Place::Ask => scene.members.as_ref().map(|s| sheet_hit(s, row, code)),
        Place::Pick | Place::Name => scene.new_group.as_ref().map(|n| new_hit(n, row, code)),
        Place::Other => None,
    }
}

/// The chat's two: `m` the group bar's action, Enter an invitation's Join,
/// the group's own card first, else the one on the bubble under the
/// cursor. Any other key is the chat's.
fn chat_hit(scene: &Scene, cursor: Option<u64>, code: KeyCode) -> Option<Hit> {
    let chat = scene.chat.as_ref()?;
    match code {
        KeyCode::Char('m') => chat
            .bar
            .actions
            .iter()
            .find(|a| matches!(a.intent, Intent::Group(Verb::Members { .. })))
            .map(|a| Hit::Run(a.intent.clone())),
        KeyCode::Enter => {
            let card = chat.invite.as_ref().and_then(|i| i.join.as_ref());
            card.or_else(|| {
                chat.groups
                    .iter()
                    .flat_map(|g| &g.bubbles)
                    .filter(|b| Some(b.bubble.key) == cursor)
                    .find_map(|b| b.invite.as_ref()?.join.as_ref())
            })
            .map(|a| Hit::Run(a.intent.clone()))
        }
        _ => None,
    }
}

/// An action that is there and enabled, run; else the key stays.
fn run(action: Option<&Action>) -> Hit {
    match action {
        Some(action) if action.enabled => Hit::Run(action.intent.clone()),
        _ => Hit::Stay,
    }
}

fn moved(row: usize, code: KeyCode, rows: usize) -> Option<Hit> {
    match code {
        KeyCode::Up => Some(Hit::Move(row.saturating_sub(1))),
        KeyCode::Down => Some(Hit::Move((row + 1).min(rows.saturating_sub(1)))),
        _ => None,
    }
}

fn sheet_hit(sheet: &MemberSheet, row: usize, code: KeyCode) -> Hit {
    if let Some(ask) = &sheet.ask {
        return match code {
            KeyCode::Char('y') | KeyCode::Enter => run(Some(&ask.yes)),
            KeyCode::Char('n') | KeyCode::Esc => run(Some(&ask.no)),
            _ => Hit::Stay,
        };
    }
    if let Some(hit) = moved(row, code, sheet.rows.len()) {
        return hit;
    }
    match code {
        KeyCode::Char('l') => run(sheet.leave.as_ref()),
        KeyCode::Char('a') => run(sheet.add.as_ref()),
        KeyCode::Char('r') => run(sheet.rows.get(row).and_then(|r| r.remove.as_ref())),
        KeyCode::Char('d') => run(sheet.delete.as_ref()),
        KeyCode::Char('s') => run(sheet.again.as_ref()),
        KeyCode::Esc => run(Some(&sheet.close)),
        _ => Hit::Stay,
    }
}

fn new_hit(node: &NewGroupNode, row: usize, code: KeyCode) -> Hit {
    match node.step {
        Step::Pick => {
            if let Some(hit) = moved(row, code, node.rows.len()) {
                return hit;
            }
            match code {
                KeyCode::Char(' ') => run(node.rows.get(row).map(|r| &r.toggle)),
                KeyCode::Enter => run(Some(&node.next)),
                KeyCode::Esc => run(Some(&node.close)),
                _ => Hit::Stay,
            }
        }
        Step::Name => match code {
            KeyCode::Char(c) => {
                let mut name = node.name.clone();
                name.push(c);
                Hit::Run(Intent::Group(Verb::Name(name)))
            }
            KeyCode::Backspace => {
                let mut name = node.name.clone();
                name.pop();
                Hit::Run(Intent::Group(Verb::Name(name)))
            }
            KeyCode::Enter => run(Some(&node.create)),
            KeyCode::Esc => run(Some(&node.close)),
            _ => Hit::Stay,
        },
    }
}

/// The scroll command `key` is in the open chat (1529): the shared table's
/// (the ring's up and down a line, its left and right a page), which the
/// M9's own lines carry out ([`crate::chat::page`]).
fn chat_scroll<B>(model: &UiModel<B>, place: Place, key: &KeyEvent) -> Option<ScrollCmd> {
    if place != Place::Chat {
        return None;
    }
    match loquax::keys::resolve(key, loquax::keys::panes_of(Pane::Messages), model.ctx())? {
        loquax::keys::Action::Scroll(cmd) => Some(cmd),
        _ => None,
    }
}

/// One key on the M9: a group's key runs its node's action or moves the
/// cursor `row`; any other goes to the model as the terminal's key, the
/// map's own keys first made the terminal's ([`crate::map::key`]). The
/// cursor goes back to the first row when the key opened or closed a
/// group's screen, not when it answered the sheet's question. What the model hands out, as `update` does.
pub fn press(model: &mut UiModel<Field>, row: &mut usize, key: &KeyEvent) -> Vec<UiEffect> {
    let before = Place::of(model);
    let ours = matches!(
        (before, key.code),
        (Place::List, KeyCode::Char('n'))
            | (Place::Chat, KeyCode::Char('m') | KeyCode::Enter)
            | (Place::Sheet | Place::Ask | Place::Pick | Place::Name, _)
    );
    let found = match ours {
        true => {
            let look = Look {
                row: *row,
                ..Look::of(model)
            };
            hit(&screen::scene(model), before, &look, key)
        }
        false => None,
    };
    let mut effects = match found {
        Some(Hit::Run(Intent::Group(verb))) => update(model, UiEvent::Group(verb)),
        Some(Hit::Run(_)) | Some(Hit::Stay) => Vec::new(),
        Some(Hit::Move(to)) => {
            *row = to;
            Vec::new()
        }
        None => match chat_scroll(model, before, key) {
            Some(cmd) => crate::chat::page(model, cmd),
            None => update(model, UiEvent::Key(crate::map::key(model, *key))),
        },
    };
    // The key brought the chat onto the panel (1493): only now is it
    // read. Moving the list's cursor selects without reading
    // (`list_apart`), so the open is said as a phone's tap says it.
    if before == Place::List && Place::of(model) == Place::Chat {
        if let Some(peer) = model.conversations.get(model.selected).map(|c| c.peer) {
            effects.extend(update(model, UiEvent::Open { peer }));
        }
    }
    // The question is the sheet's: answering it keeps the cursor where it
    // was.
    let sheet = |place| match place {
        Place::Ask => Place::Sheet,
        other => other,
    };
    if sheet(Place::of(model)) != sheet(before) {
        *row = 0;
    }
    effects.extend(close_unshown(model));
    effects
}

/// The model's states the M9 has no screen for (1612): the message's
/// detail, the profile and the settings, the help, the palette and the
/// copy mode. The shared key table reaches each from a key the M9 sends
/// (Enter on a message opens its detail, 1076; `:` the palette), and a
/// state nobody sees takes the next keys: in window 23 every Back closed
/// a detail view the board never drew, and the screen stood five minutes
/// on a chat whose keys were all read. Each one found is closed and said
/// as a notice, `unshown=<state>`, which the board logs as a `[UI]` line.
pub fn close_unshown<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let mut said = Vec::new();
    if model.detail.is_some() {
        said.push("detail");
    }
    if model.profile_open {
        said.push("profile");
    }
    if model.settings.is_some() {
        said.push("settings");
    }
    // One step closes the detail or the profile, the next a settings
    // section, the last the settings' list.
    for _ in 0..3 {
        if !model.detail_open() {
            break;
        }
        model.close_detail();
    }
    if model.help {
        model.help = false;
        said.push("help");
    }
    if model.palette.is_some() {
        model.palette = None;
        said.push("palette");
    }
    if model.copy_mode.is_some() {
        loquax::ui::copy_mode::leave(model);
        said.push("copy_mode");
    }
    said.into_iter()
        .map(|name| UiEffect::Notice(format!("unshown={name} closed")))
        .collect()
}

/// What the board asks its home for a group effect of the model (1366):
/// the admin's `/group`, `/groupadd` and `/groupdelete`, and a member's
/// Join, Leave or the admin's Remove as the state message to the group.
/// `client_id` is the board's fresh one for this ask. None for any other
/// effect, a plain message among them.
pub fn ask_of(effect: &UiEffect, client_id: [u8; 16]) -> Option<GroupVerb> {
    match effect {
        UiEffect::CreateGroup { name, members } => Some(GroupVerb::Create(wire::GroupRequest {
            client_id,
            name: name.clone(),
            members: members.clone(),
        })),
        UiEffect::AddMembers { group, members } => Some(GroupVerb::Add(wire::GroupAddRequest {
            client_id,
            group: *group,
            members: members.clone(),
        })),
        UiEffect::DeleteGroup { group } => Some(GroupVerb::Delete(wire::GroupDeleteRequest {
            client_id,
            group: *group,
        })),
        UiEffect::Send { to, body, fields } if groups::is_state(body, fields) => {
            Some(GroupVerb::State(wire::Submission {
                client_id,
                destination: *to,
                title: Vec::new(),
                content: body.as_bytes().to_vec(),
                fields: fields.clone(),
            }))
        }
        _ => None,
    }
}

/// The group's screen of `scene`, when one is up: the member sheet or "New
/// group", the whole grid, the M9's cursor on `look.row`.
pub(crate) fn lines(scene: &Scene, look: &Look, cols: usize, rows: usize) -> Option<Vec<RLine>> {
    let out = match (&scene.members, &scene.new_group) {
        (Some(sheet), _) => sheet_lines(sheet, look, cols, rows),
        (None, Some(node)) => match node.step {
            Step::Pick => pick_lines(node, look, cols, rows),
            Step::Name => name_lines(node, look, cols, rows),
        },
        (None, None) => return None,
    };
    Some(out)
}

/// A sheet's bar: its title on the left, `right` on the right, in the
/// chrome.
fn bar(title: &str, right: &str, look: &Look, cols: usize) -> RLine {
    let chrome = painted(look.tokens.chrome);
    let right = cut(right, cols / 2);
    let title = cut(title, cols.saturating_sub(fit_width(&right) + 3));
    let mut line = RLine::default();
    push(&mut line, " ", chrome);
    push(
        &mut line,
        &title,
        RStyle {
            bold: true,
            ..chrome
        },
    );
    let gap = cols.saturating_sub(1 + fit_width(&title) + fit_width(&right) + 1);
    push(&mut line, &" ".repeat(gap), chrome);
    push(&mut line, &right, chrome);
    fill(line, cols, chrome)
}

/// A key's hint: the key in brackets, then what it does; muted when the
/// verb is not offered now.
fn hint(line: &mut RLine, key: &str, words: &str, on: bool, look: &Look) {
    let style = match on {
        true => RStyle::default(),
        false => muted(look),
    };
    push(line, &format!("[{key}]"), RStyle { bold: on, ..style });
    push(line, &format!(" {words}"), style);
}

fn label(action: &Action, look: &Look) -> String {
    word(action.label_key, look)
}

fn word(id: StrId, look: &Look) -> String {
    narrow(&Text::key(id).resolve(look.lang))
}

/// The first of `count` rows to show in `room` so that `at` shows.
fn first_shown(at: usize, count: usize, room: usize) -> usize {
    match count <= room {
        true => 0,
        false => (at + 1).saturating_sub(room).min(count - room),
    }
}

/// A row behind the selection bar, as the list's selected entry.
fn selected(mut line: RLine, look: &Look) -> RLine {
    let selection = painted(look.tokens.selection);
    if let Some(cell) = line.cells.first_mut() {
        cell.ch = BAR;
    }
    for cell in line.cells.iter_mut().skip(1) {
        cell.st.fg = selection.fg.or(cell.st.fg);
        cell.st.bg = selection.bg.or(cell.st.bg);
        cell.st.reverse = selection.reverse;
    }
    line
}

/// The member sheet: the group's name and how many are in it, a row a
/// member with its standing, then the verbs the reader has with their
/// keys, or the question with its yes and no.
fn sheet_lines(sheet: &MemberSheet, look: &Look, cols: usize, rows: usize) -> Vec<RLine> {
    let lang = look.lang;
    let mut out = Vec::with_capacity(rows);
    out.push(bar(
        &sheet.title.resolve(lang),
        &sheet.count.resolve(lang),
        look,
        cols,
    ));

    let mut foot: Vec<RLine> = Vec::new();
    match &sheet.ask {
        Some(ask) => {
            let words = narrow(&ask.words.resolve(lang));
            let question = RStyle {
                bold: true,
                ..painted(look.tokens.unread)
            };
            for line in loquax::ui::wrap(&[styled(&words, question)], cols.saturating_sub(2), 0) {
                let mut row = styled(" ", RStyle::default());
                row.cells.extend(line.cells);
                foot.push(row);
            }
            let mut keys = styled(" ", RStyle::default());
            hint(&mut keys, "y", &word(StrId::GroupYes, look), true, look);
            push(&mut keys, "   ", RStyle::default());
            hint(&mut keys, "n", &word(StrId::GroupNo, look), true, look);
            foot.push(keys);
        }
        None => {
            let remove = sheet.rows.get(look.row).and_then(|r| r.remove.as_ref());
            let verbs: [(&str, Option<&Action>); 5] = [
                ("l", sheet.leave.as_ref()),
                ("a", sheet.add.as_ref()),
                ("r", remove),
                ("d", sheet.delete.as_ref()),
                ("s", sheet.again.as_ref()),
            ];
            for (key, action) in verbs {
                let Some(action) = action else {
                    continue;
                };
                let mut line = styled(" ", RStyle::default());
                let words = match key {
                    // The Remove names whom it removes: the cursor's row.
                    "r" => format!(
                        "{} {}",
                        label(action, look),
                        narrow(&sheet.rows[look.row].name.resolve(lang))
                    ),
                    _ => label(action, look),
                };
                hint(&mut line, key, &words, action.enabled, look);
                foot.push(line);
            }
        }
    }
    let mut close = styled(" ", painted(look.tokens.chrome));
    push(
        &mut close,
        &format!("[Back] {}", word(StrId::Close, look)),
        painted(look.tokens.chrome),
    );

    // Members, then a blank row, the verbs and the close line at the
    // bottom.
    let room = rows.saturating_sub(1 + 1 + foot.len() + 1).max(1);
    let first = first_shown(look.row, sheet.rows.len(), room);
    for (i, member) in sheet.rows.iter().enumerate().skip(first).take(room) {
        let name = narrow(&member.name.resolve(lang));
        let state = narrow(&member.state.resolve(lang));
        let mut line = styled("  ", RStyle::default());
        let width = cols.saturating_sub(4 + fit_width(&state) + 1);
        let name = cut(&name, width);
        push(
            &mut line,
            &name,
            RStyle {
                bold: member.you,
                ..RStyle::default()
            },
        );
        let gap = cols.saturating_sub(2 + fit_width(&name) + fit_width(&state) + 1);
        push(&mut line, &" ".repeat(gap), RStyle::default());
        push(&mut line, &state, muted(look));
        let line = fill(line, cols, RStyle::default());
        out.push(match i == look.row && sheet.ask.is_none() {
            true => selected(line, look),
            false => line,
        });
    }
    while out.len() < rows.saturating_sub(foot.len() + 1) {
        out.push(fill(RLine::default(), cols, RStyle::default()));
    }
    out.extend(foot.into_iter().map(|l| fill(l, cols, RStyle::default())));
    out.push(fill(close, cols, painted(look.tokens.chrome)));
    out.truncate(rows);
    out
}

/// "New group", its first step: the friends, a row each with its tick and
/// avatar, the cursor's behind the bar; how many are ticked in the bar;
/// the keys at the bottom.
fn pick_lines(node: &NewGroupNode, look: &Look, cols: usize, rows: usize) -> Vec<RLine> {
    let lang = look.lang;
    let mut out = Vec::with_capacity(rows);
    out.push(bar(
        &node.title.resolve(lang),
        &node.picked.resolve(lang),
        look,
        cols,
    ));
    let room = rows.saturating_sub(2).max(1);
    let first = first_shown(look.row, node.rows.len(), room);
    for (i, pick) in node.rows.iter().enumerate().skip(first).take(room) {
        let mut line = styled(" ", RStyle::default());
        let tick = match pick.picked {
            true => "[x] ",
            false => "[ ] ",
        };
        push(
            &mut line,
            tick,
            RStyle {
                bold: pick.picked,
                ..RStyle::default()
            },
        );
        push_avatar(&mut line, &pick.avatar);
        push(&mut line, " ", RStyle::default());
        push(
            &mut line,
            &cut(&pick.name, cols.saturating_sub(10)),
            RStyle::default(),
        );
        let line = fill(line, cols, RStyle::default());
        out.push(match i == look.row {
            true => selected(line, look),
            false => line,
        });
    }
    while out.len() < rows.saturating_sub(1) {
        out.push(fill(RLine::default(), cols, RStyle::default()));
    }
    let mut keys = styled(" ", RStyle::default());
    hint(&mut keys, "Space", "tick", true, look);
    push(&mut keys, "  ", RStyle::default());
    hint(
        &mut keys,
        "Enter",
        &label(&node.next, look),
        node.next.enabled,
        look,
    );
    out.push(fill(keys, cols, RStyle::default()));
    out.truncate(rows);
    out
}

/// "New group", its second step: whom it is for, the name typed with its
/// caret at the end, the trust sentence, the keys.
fn name_lines(node: &NewGroupNode, look: &Look, cols: usize, rows: usize) -> Vec<RLine> {
    let lang = look.lang;
    let mut out = Vec::with_capacity(rows);
    out.push(bar(&node.title.resolve(lang), "", look, cols));
    let names: Vec<&str> = node.rows.iter().map(|r| r.name.as_str()).collect();
    let mut whom = styled(" ", RStyle::default());
    push(
        &mut whom,
        &cut(&names.join(", "), cols.saturating_sub(2)),
        muted(look),
    );
    out.push(whom);
    out.push(RLine::default());

    let chrome = painted(look.tokens.chrome);
    let width = cols.saturating_sub(2).max(1);
    let mut field = styled(" ", chrome);
    match node.name.is_empty() {
        true => {
            let hint = cut(&node.hint.resolve(lang), width.saturating_sub(1));
            push(
                &mut field,
                " ",
                RStyle {
                    reverse: true,
                    ..chrome
                },
            );
            push(
                &mut field,
                &hint,
                RStyle {
                    fg: look.tokens.chrome_muted.fg.or(chrome.fg),
                    ..chrome
                },
            );
        }
        false => {
            let shown = narrow(&node.name);
            let len = shown.chars().count();
            let skip = (len + 1).saturating_sub(width);
            for ch in shown.chars().skip(skip) {
                push(&mut field, ch.encode_utf8(&mut [0; 4]), chrome);
            }
            push(
                &mut field,
                " ",
                RStyle {
                    reverse: true,
                    ..chrome
                },
            );
        }
    }
    out.push(fill(field, cols, chrome));
    out.push(RLine::default());
    let trust = narrow(&node.trust.resolve(lang));
    for line in loquax::ui::wrap(&[styled(&trust, muted(look))], cols.saturating_sub(2), 0) {
        let mut row = styled(" ", RStyle::default());
        row.cells.extend(line.cells);
        out.push(row);
    }
    out.truncate(rows.saturating_sub(1));
    while out.len() < rows.saturating_sub(1) {
        out.push(RLine::default());
    }
    let mut keys = styled(" ", RStyle::default());
    hint(
        &mut keys,
        "Enter",
        &label(&node.create, look),
        node.create.enabled,
        look,
    );
    push(&mut keys, "  ", RStyle::default());
    hint(&mut keys, "Back", &label(&node.close, look), true, look);
    out.push(keys);
    out.into_iter()
        .map(|l| fill(l, cols, RStyle::default()))
        .collect()
}

fn styled(text: &str, st: RStyle) -> RLine {
    let mut line = RLine::default();
    push(&mut line, text, st);
    line
}
