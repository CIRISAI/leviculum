//! The Browse tab on the phone (1337, #475): lnomad's browser hosted in
//! Loquax, as `docs/src/concepts/lnomad-everywhere.md` section 4 decides.
//!
//! Two halves, both pure: [`screen`] flattens the hosted browser's scene
//! ([`lnomad::app::scene::Scene`], inside Loquax's) into the
//! [`schema::BrowseScreen`] Java draws, its words resolved in the phone's
//! language; [`event`] reads a [`schema::Request::Browse`] back into the
//! model's [`BrowseEvent`] or a tab switch. Java adds no browser logic: it
//! draws the runs in a monospace grid and sends a node's resource back.

use leviculum_ui::color::Ink;
use leviculum_ui::keymap::ScrollCmd;
use leviculum_ui::scene::Lead;
use leviculum_ui::style::RStyle;
use lnomad::app::scene::{NodeId, Row, Run, Sheet};
use lnomad::app::Focus;
use lnomad::Intent;
use loquax::ui::browse::BrowseEvent;
use loquax::ui::scene::{LayoutClass, TabId};
use loquax::ui::strings::Lang;
use loquax::ui::{UiEvent, UiModel};

use crate::schema::{self, BrowseAction, BrowseLine, BrowseRow, BrowseRun, BrowseScreen};

/// The Browse tab as Java draws it, in `lang`. Closed, it says so and
/// carries nothing else.
pub fn screen<B>(model: &UiModel<B>, lang: Lang, v: u32) -> BrowseScreen {
    let Some(scene) = model.scene(LayoutClass::Compact).browse else {
        return BrowseScreen {
            v,
            enabled: model.browse.enabled,
            ..BrowseScreen::default()
        };
    };
    let bar = &scene.top_bar;
    let actions = bar
        .actions
        .iter()
        .filter_map(|action| {
            let verb = match action.intent {
                Intent::Forward => "forward",
                Intent::Reload => "reload",
                Intent::Bookmark { on: true } => "bookmark_on",
                Intent::Bookmark { on: false } => "bookmark_off",
                Intent::Identify { on: true } => "identify_on",
                Intent::Identify { on: false } => "identify_off",
                _ => return None,
            };
            Some(BrowseAction {
                verb: verb.to_string(),
                label: action.label_key.text(lang).to_string(),
                enabled: action.enabled,
            })
        })
        .collect();
    let sheet = scene.names.as_ref().or(scene.places.as_ref());
    BrowseScreen {
        v,
        enabled: model.browse.enabled,
        open: true,
        title: bar.title.resolve(lang),
        back: matches!(bar.lead, Lead::Back { .. }),
        actions,
        address: scene.address.value.clone(),
        dest: model
            .browse
            .shown_dest()
            .map(|d| d.iter().map(|b| format!("{b:02x}")).collect())
            .unwrap_or_default(),
        address_hint: scene.address.hint.resolve(lang),
        chips: scene.state.iter().map(|c| c.text.resolve(lang)).collect(),
        status: scene
            .status
            .as_ref()
            .map(|c| c.text.resolve(lang))
            .unwrap_or_default(),
        lines: scene
            .page
            .lines
            .iter()
            .map(|line| BrowseLine {
                runs: line.runs.iter().map(run).collect(),
            })
            .collect(),
        first: u32::try_from(scene.page.first).unwrap_or(u32::MAX),
        total: u32::try_from(scene.page.total).unwrap_or(u32::MAX),
        focus: match scene.page.focus {
            Some(Focus::Link(n)) => NodeId::Link(n).resource(),
            Some(Focus::Field(n)) => NodeId::Field(n).resource(),
            None => String::new(),
        },
        sheet: sheet.map(|s| s.title.resolve(lang)).unwrap_or_default(),
        rows: sheet.map(|s| rows(s, lang)).unwrap_or_default(),
        toast: scene
            .toast
            .as_ref()
            .map(|t| t.text.resolve(lang))
            .unwrap_or_default(),
        // The session's to fill, once.
        ..BrowseScreen::default()
    }
}

fn run(run: &Run) -> BrowseRun {
    let RStyle {
        fg,
        bg,
        bold,
        underline,
        italic,
        reverse,
    } = run.style;
    let node = match (run.link, run.field) {
        (Some(n), _) => NodeId::Link(n).resource(),
        (None, Some(n)) => NodeId::Field(n).resource(),
        (None, None) => String::new(),
    };
    BrowseRun {
        text: run.text.clone(),
        fg: ink(fg),
        bg: ink(bg),
        bold,
        underline,
        italic,
        reverse,
        node,
    }
}

fn ink(ink: Option<Ink>) -> String {
    match ink {
        None => String::new(),
        Some(Ink::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
        Some(Ink::Indexed(n)) => n.to_string(),
    }
}

fn rows(sheet: &Sheet, lang: Lang) -> Vec<BrowseRow> {
    sheet
        .rows
        .iter()
        .map(|row| match row {
            Row::Heading(text) => BrowseRow {
                kind: "heading".to_string(),
                text: text.resolve(lang),
                ..BrowseRow::default()
            },
            Row::Placeholder(text) => BrowseRow {
                kind: "placeholder".to_string(),
                text: text.resolve(lang),
                ..BrowseRow::default()
            },
            Row::Item { item, title, .. } => BrowseRow {
                kind: "item".to_string(),
                text: title.resolve(lang),
                node: item.id.resource(),
                selected: item.selected,
            },
            Row::Note { text, .. } => BrowseRow {
                kind: "note".to_string(),
                text: text.resolve(lang),
                ..BrowseRow::default()
            },
        })
        .collect()
}

/// A scene node by its resource, as [`NodeId::resource`] wrote it.
pub fn node(resource: &str) -> Result<NodeId, String> {
    let index = |prefix: &str| {
        resource
            .strip_prefix(prefix)
            .and_then(|n| n.parse::<usize>().ok())
    };
    if resource == "address" {
        return Ok(NodeId::Address);
    }
    if let Some(n) = index("link_") {
        return Ok(NodeId::Link(n));
    }
    if let Some(n) = index("field_") {
        return Ok(NodeId::Field(n));
    }
    if let Some(n) = index("place_") {
        return Ok(NodeId::Place(n));
    }
    if let Some(n) = index("name_") {
        return Ok(NodeId::Name(n));
    }
    Err(format!("no browser node {resource:?}"))
}

/// The model's event for a [`schema::Request::Browse`].
pub fn event(
    verb: &str,
    node_id: &str,
    text: &str,
    cols: u16,
    rows: u16,
) -> Result<UiEvent, String> {
    let intent = |intent| Ok(UiEvent::Browse(BrowseEvent::Intent(intent)));
    match verb {
        "tab" => Ok(UiEvent::Tab(TabId::Browse)),
        "chats" => Ok(UiEvent::Tab(TabId::Chats)),
        "size" => Ok(UiEvent::Browse(BrowseEvent::Resize { cols, rows })),
        "back" => intent(Intent::Back),
        "forward" => intent(Intent::Forward),
        "reload" => intent(Intent::Reload),
        "places" => intent(Intent::Places),
        "dismiss" => intent(Intent::Dismiss),
        "bookmark_on" => intent(Intent::Bookmark { on: true }),
        "bookmark_off" => intent(Intent::Bookmark { on: false }),
        "identify_on" => intent(Intent::Identify { on: true }),
        "identify_off" => intent(Intent::Identify { on: false }),
        "activate" => intent(Intent::Activate(node(node_id)?)),
        "go" => intent(Intent::Go {
            url: Some(text.to_string()),
        }),
        "fill" => match node(node_id)? {
            NodeId::Field(field) => intent(Intent::Fill {
                field,
                text: text.to_string(),
            }),
            other => Err(format!("{} is no field", other.resource())),
        },
        "scroll" => intent(Intent::Scroll(match text {
            "up" => ScrollCmd::LineUp,
            "down" => ScrollCmd::LineDown,
            "pageup" => ScrollCmd::PageUp,
            "pagedown" => ScrollCmd::PageDown,
            "top" => ScrollCmd::Top,
            "bottom" => ScrollCmd::Bottom,
            other => return Err(format!("no scroll {other:?}")),
        })),
        other => Err(format!("no browse verb {other:?}")),
    }
}

/// The request's encoded answer, for the facade's tests and Java alike.
pub fn encode<B>(model: &UiModel<B>, lang: Lang, v: u32) -> Vec<u8> {
    schema::encode(&screen(model, lang, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_node_reads_back_from_its_resource() {
        for id in [
            NodeId::Address,
            NodeId::Link(3),
            NodeId::Field(1),
            NodeId::Place(0),
            NodeId::Name(12),
        ] {
            assert_eq!(node(&id.resource()), Ok(id));
        }
        assert!(node("page").is_err(), "the page is no tap target");
        assert!(node("link_x").is_err());
    }

    #[test]
    fn a_verb_the_tab_does_not_know_is_refused() {
        assert!(event("jump", "", "", 0, 0).is_err());
        assert!(event("scroll", "", "sideways", 0, 0).is_err());
        assert!(event("fill", "link_1", "x", 0, 0).is_err());
        assert_eq!(event("tab", "", "", 0, 0), Ok(UiEvent::Tab(TabId::Browse)));
    }
}
