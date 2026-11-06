//! The scene's shared vocabulary: the nodes and the parts of a node that
//! every program's scene names the same (1240, slice 0 of
//! `docs/src/concepts/lnomad-everywhere.md`, its section 3 "Shared
//! controls"). Loquax's scene (`loquax/src/ui/scene.rs`) built them first
//! (1217); lnomad's builds on them from slice 1.
//!
//! A scene is a value a renderer turns into its own drawing: a phone's
//! Views, a window's widgets, a terminal's cells, the M9's pixels. It
//! carries no geometry. Each node carries what a screen reader and
//! `uiautomator` need ([`A11y`]) and what a tap on it does ([`Action`]).
//!
//! What a program names itself comes in through [`Contract`]: its
//! catalogue's keys, its intents, its node ids and tabs, its avatar and the
//! row its sheets list. A program sets those once on a marker type and
//! names the nodes through aliases (`type TopBar = scene::TopBar<Loquax>`),
//! so its scene builder reads as if the nodes were its own.
//!
//! No terminal and no `std`: the module is `alloc` only.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Debug;

use serde_json::{json, Value};

use crate::strings::{Key, Text};

/// An intent a node raises, with the catalogue's word for it.
pub trait Labelled: Clone + Debug + PartialEq {
    /// The catalogue the label is a key of.
    type Key: Key;

    /// What a menu or a screen reader calls the intent.
    fn label(&self) -> Self::Key;
}

/// A node's stable identity: a renderer diffs on it, hit tests to it, and
/// moves the keyboard's focus over it.
pub trait Node: Copy + Debug + Eq {
    /// The id `uiautomator` and the device walk find the node by: lower
    /// case, digits and `_`, as an Android resource name is.
    fn resource(&self) -> String;
}

/// A value that goes into a golden file as JSON.
pub trait ToJson {
    fn to_json(&self) -> Value;
}

/// What a program names in its scene.
pub trait Contract: Clone + Debug + PartialEq {
    /// The catalogue's keys (`catalogue!`).
    type Key: Key;
    /// The program's verbs.
    type Intent: Labelled<Key = Self::Key>;
    /// The program's node ids.
    type Node: Node;
    /// The tabs of its tab bar.
    type Tab: Copy + Debug + Eq;
    /// What leads a top bar: Loquax's round avatar.
    type Avatar: Clone + Debug + PartialEq + ToJson;
    /// What a sheet lists.
    type Row: Clone + Debug + PartialEq;
}

/// What a node is to a screen reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Bar,
    TabBar,
    Tab,
    List,
    Row,
    Chat,
    Group,
    Bubble,
    Button,
    TextField,
    Sheet,
    Card,
    Chip,
    /// A laid-out document: the browser's page (1255).
    Page,
    /// A link in a page.
    Link,
    /// A form field in a page.
    Field,
    /// A picture in a page.
    Image,
}

impl Role {
    pub fn word(self) -> &'static str {
        match self {
            Role::Bar => "bar",
            Role::TabBar => "tab_bar",
            Role::Tab => "tab",
            Role::List => "list",
            Role::Row => "row",
            Role::Chat => "chat",
            Role::Group => "group",
            Role::Bubble => "bubble",
            Role::Button => "button",
            Role::TextField => "text_field",
            Role::Sheet => "sheet",
            Role::Card => "card",
            Role::Chip => "chip",
            Role::Page => "page",
            Role::Link => "link",
            Role::Field => "field",
            Role::Image => "image",
        }
    }
}

/// What TalkBack and `uiautomator` need of a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct A11y<K> {
    pub role: Role,
    pub label: Text<K>,
    pub selected: bool,
    pub enabled: bool,
    /// The stable resource id ([`Node::resource`]).
    pub resource: String,
}

impl<K: Key> A11y<K> {
    /// Node `id`'s, enabled and not selected.
    pub fn of(id: impl Node, role: Role, label: Text<K>) -> Self {
        Self {
            role,
            label,
            selected: false,
            enabled: true,
            resource: id.resource(),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "role": self.role.word(),
            "label": self.label.to_json(),
            "selected": self.selected,
            "enabled": self.enabled,
            "resource": self.resource,
        })
    }
}

/// Something a node does: the intent a tap on it raises, whether it may
/// now, and its label.
#[derive(Clone, Debug, PartialEq)]
pub struct Action<I: Labelled> {
    pub intent: I,
    pub enabled: bool,
    pub label_key: I::Key,
}

impl<I: Labelled> Action<I> {
    /// `intent`, enabled, with its own label.
    pub fn of(intent: I) -> Self {
        let label_key = intent.label();
        Self {
            intent,
            enabled: true,
            label_key,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "intent": format!("{:?}", self.intent),
            "enabled": self.enabled,
            "label_key": self.label_key.key(),
        })
    }
}

/// A list of actions for a golden file.
pub fn actions_json<I: Labelled>(list: &[Action<I>]) -> Value {
    Value::Array(list.iter().map(Action::to_json).collect())
}

/// What leads the top bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Lead<C: Contract> {
    /// The reader's own avatar, which opens the profile, with the mailbox
    /// link's dot when the shell knows it. `open` is what a tap or a click
    /// on it raises (Loquax's profile, 1443); none where the avatar is a
    /// picture alone.
    Me {
        avatar: C::Avatar,
        link: Option<bool>,
        open: Option<Action<C::Intent>>,
    },
    /// A sub view: back, and the avatar of what it shows.
    Back {
        back: Action<C::Intent>,
        avatar: C::Avatar,
    },
}

/// The bar at the top of a screen.
#[derive(Clone, Debug, PartialEq)]
pub struct TopBar<C: Contract> {
    pub id: C::Node,
    pub lead: Lead<C>,
    pub title: Text<C::Key>,
    pub actions: Vec<Action<C::Intent>>,
    pub a11y: A11y<C::Key>,
}

impl<C: Contract> TopBar<C> {
    pub fn to_json(&self) -> Value {
        let lead = match &self.lead {
            Lead::Me { avatar, link, open } => {
                let mut me = json!({ "avatar": avatar.to_json(), "link": link });
                if let Some(open) = open {
                    me["open"] = open.to_json();
                }
                json!({ "me": me })
            }
            Lead::Back { back, avatar } => {
                json!({ "back": { "action": back.to_json(), "avatar": avatar.to_json() } })
            }
        };
        json!({
            "id": self.id.resource(),
            "lead": lead,
            "title": self.title.to_json(),
            "actions": actions_json(&self.actions),
            "a11y": self.a11y.to_json(),
        })
    }
}

/// A tab of the bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab<C: Contract> {
    pub id: C::Tab,
    pub label: Text<C::Key>,
    /// The count on it: Loquax's chats with unread messages.
    pub badge: Option<u32>,
    pub action: Action<C::Intent>,
    pub a11y: A11y<C::Key>,
}

/// The tab bar: none on a sub view ("two levels only").
#[derive(Clone, Debug, PartialEq)]
pub struct Tabs<C: Contract> {
    pub id: C::Node,
    pub items: Vec<Tab<C>>,
    pub active: C::Tab,
    pub a11y: A11y<C::Key>,
}

impl<C: Contract> Tabs<C> {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "active": format!("{:?}", self.active),
            "items": self.items.iter().map(|tab| json!({
                "id": format!("{:?}", tab.id),
                "label": tab.label.to_json(),
                "badge": tab.badge,
                "action": tab.action.to_json(),
                "a11y": tab.a11y.to_json(),
            })).collect::<Vec<_>>(),
            "a11y": self.a11y.to_json(),
        })
    }
}

/// The generic part of a list's row: its id, whether it is the chosen
/// one, what a tap and a long press do, and what a screen reader says. A
/// program's row keeps its own content beside it (Loquax's `Row`: the
/// conversation's `ListRow` and its avatar).
#[derive(Clone, Debug, PartialEq)]
pub struct ListItem<C: Contract> {
    pub id: C::Node,
    /// Whether it is the open or chosen one.
    pub selected: bool,
    /// A tap.
    pub activate: Action<C::Intent>,
    /// A long press or a right click.
    pub menu: Vec<Action<C::Intent>>,
    pub a11y: A11y<C::Key>,
}

/// A sheet over the screen: a list of rows and actions under a title.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet<C: Contract> {
    pub id: C::Node,
    pub title: Text<C::Key>,
    pub rows: Vec<C::Row>,
    pub actions: Vec<Action<C::Intent>>,
    pub a11y: A11y<C::Key>,
}

/// A transient state, never a status line.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip<C: Contract> {
    pub id: C::Node,
    pub text: Text<C::Key>,
    pub a11y: A11y<C::Key>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    crate::catalogue! {
        enum Words;
        const WORDS;
        Back = "back", "Back", "Zurück";
        Page = "page", "Page", "Seite";
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Verb {
        Back,
    }

    impl Labelled for Verb {
        type Key = Words;
        fn label(&self) -> Words {
            Words::Back
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Bar;

    impl Node for Bar {
        fn resource(&self) -> String {
            "top_bar".to_string()
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct Mark;

    impl ToJson for Mark {
        fn to_json(&self) -> Value {
            json!("mark")
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct Test;

    impl Contract for Test {
        type Key = Words;
        type Intent = Verb;
        type Node = Bar;
        type Tab = Bar;
        type Avatar = Mark;
        type Row = ();
    }

    /// A program's own types fill the shared nodes, and the bar goes to
    /// JSON with its keys sorted and its texts unresolved.
    #[test]
    fn a_program_fills_the_shared_bar() {
        let bar: TopBar<Test> = TopBar {
            id: Bar,
            lead: Lead::Back {
                back: Action::of(Verb::Back),
                avatar: Mark,
            },
            title: Text::key(Words::Page),
            actions: vec![],
            a11y: A11y::of(Bar, Role::Bar, Text::key(Words::Page)),
        };
        assert_eq!(bar.a11y.resource, "top_bar");
        assert_eq!(WORDS.len(), 2);
        assert_eq!(
            bar.to_json().to_string(),
            concat!(
                r#"{"a11y":{"enabled":true,"label":{"args":[],"key":"page"},"#,
                r#""resource":"top_bar","role":"bar","selected":false},"actions":[],"#,
                r#""id":"top_bar","lead":{"back":{"action":{"enabled":true,"#,
                r#""intent":"Back","label_key":"back"},"avatar":"mark"}},"#,
                r#""title":{"args":[],"key":"page"}}"#
            )
        );
    }
}
