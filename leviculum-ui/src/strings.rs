//! A program's words, keyed, in English and German (1217 for Loquax, 1240
//! moved the machinery here: `docs/src/concepts/lnomad-everywhere.md`,
//! slice 0 and "Shared controls").
//!
//! Each program declares its own catalogue with [`catalogue!`](crate::catalogue):
//! a key type (Loquax's `StrId`) whose variants carry an Android resource
//! name, an English and a German text. A scene node says what it says as a
//! [`Text`] over that key type; a renderer resolves it in the reader's
//! [`Lang`]. [`android_xml`] writes a catalogue as the phone's string
//! resources, so one table serves the phone's `getString` and the core.
//!
//! The texts keep Android's positional format (`%1$s`, `%1$d`), which
//! [`format()`] fills.
//!
//! No terminal and no `std`: the module is `alloc` only, as the scene that
//! carries its texts is.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Debug;

use serde_json::{json, Value};

/// A language the catalogues speak; English unless a locale says other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lang {
    #[default]
    En,
    De,
}

impl Lang {
    /// Both, in the order the generated files are written.
    pub const ALL: [Lang; 2] = [Lang::En, Lang::De];

    /// The language a BCP 47 tag or a locale names (`de`, `de-AT`,
    /// `de_DE`): German for German, English for any other.
    pub fn of_tag(tag: &str) -> Self {
        let primary = tag.split(['-', '_']).next().unwrap_or_default();
        match primary.eq_ignore_ascii_case("de") {
            true => Lang::De,
            false => Lang::En,
        }
    }

    /// The Android resource directory of the language.
    pub fn values_dir(self) -> &'static str {
        match self {
            Lang::En => "values",
            Lang::De => "values-de",
        }
    }
}

/// A string of a program's catalogue: what [`catalogue!`](crate::catalogue)
/// declares.
pub trait Key: Copy + Debug + Eq + 'static {
    /// The key: the Android resource name.
    fn key(self) -> &'static str;

    /// The text in `lang`, unformatted.
    fn text(self, lang: Lang) -> &'static str;
}

/// Declares a catalogue: the key enum, a table of every string, and the
/// enum's [`Key`](crate::strings::Key).
///
/// ```
/// leviculum_ui::catalogue! {
///     enum Words;
///     const WORDS;
///     Hello = "hello", "Hello", "Hallo";
/// }
/// use leviculum_ui::strings::Lang;
/// assert_eq!(Words::Hello.text(Lang::De), "Hallo");
/// assert_eq!(WORDS[0].1, "hello");
/// ```
#[macro_export]
macro_rules! catalogue {
    (enum $name:ident; const $table:ident; $($id:ident = $key:literal, $en:literal, $de:literal;)*) => {
        /// A string of the catalogue.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($id,)*
        }

        /// Every string, key, English and German, in the table's order.
        pub const $table: &[($name, &str, &str, &str)] = &[
            $(($name::$id, $key, $en, $de),)*
        ];

        impl $name {
            /// The key: the Android resource name.
            pub fn key(self) -> &'static str {
                match self {
                    $($name::$id => $key,)*
                }
            }

            /// The text in `lang`, unformatted.
            pub fn text(self, lang: $crate::strings::Lang) -> &'static str {
                match (self, lang) {
                    $(($name::$id, $crate::strings::Lang::En) => $en,
                      ($name::$id, $crate::strings::Lang::De) => $de,)*
                }
            }
        }

        impl $crate::strings::Key for $name {
            fn key(self) -> &'static str {
                $name::key(self)
            }

            fn text(self, lang: $crate::strings::Lang) -> &'static str {
                $name::text(self, lang)
            }
        }
    };
}

/// A value for a text's `%n$s` or `%n$d`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg<K> {
    Str(String),
    Int(i64),
    /// Words of the catalogue themselves, resolved in the same language.
    Text(Box<Text<K>>),
}

impl<K: Key> Arg<K> {
    fn render(&self, lang: Lang) -> String {
        match self {
            Arg::Str(text) => text.clone(),
            Arg::Int(n) => n.to_string(),
            Arg::Text(text) => text.resolve(lang),
        }
    }
}

/// `text` with each `%n$s` and `%n$d` replaced by the `n`th of `args`
/// (counted from one), and `%%` by `%`. A place without its argument stays
/// as written, so a missing one shows rather than vanishes.
pub fn format<K: Key>(text: &str, args: &[Arg<K>], lang: Lang) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        if let Some(after) = tail.strip_prefix('%') {
            out.push('%');
            rest = after;
            continue;
        }
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        let place = tail[..digits].parse::<usize>().ok();
        let spec = tail[digits..]
            .strip_prefix('$')
            .and_then(|s| s.chars().next())
            .filter(|c| *c == 's' || *c == 'd');
        match (place, spec) {
            (Some(n), Some(_)) if n >= 1 && n <= args.len() => {
                out.push_str(&args[n - 1].render(lang));
                rest = &tail[digits + 2..];
            }
            _ => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What a scene node says: a catalogue key with its arguments, resolved by
/// the renderer in the reader's language, or words that are no string of
/// the catalogue (a name, a message's text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Text<K> {
    Key(K, Vec<Arg<K>>),
    Raw(String),
    /// Parts said one after the other, ` · ` between them: `live · every
    /// 10 min · 2 min ago`.
    Join(Vec<Text<K>>),
}

/// What stands between the parts of a [`Text::Join`].
pub const JOIN: &str = " · ";

impl<K: Key> Text<K> {
    /// A key without arguments.
    pub fn key(id: K) -> Self {
        Text::Key(id, Vec::new())
    }

    /// The words in `lang`.
    pub fn resolve(&self, lang: Lang) -> String {
        match self {
            Text::Key(id, args) => format(id.text(lang), args, lang),
            Text::Raw(text) => text.clone(),
            Text::Join(parts) => {
                let parts: Vec<String> = parts.iter().map(|p| p.resolve(lang)).collect();
                parts.join(JOIN)
            }
        }
    }

    /// The text for a golden file: its key and arguments, not resolved, so
    /// one golden pins both languages' structure.
    pub fn to_json(&self) -> Value {
        match self {
            Text::Key(id, args) => json!({
                "key": id.key(),
                "args": args.iter().map(|arg| match arg {
                    Arg::Str(s) => Value::String(s.clone()),
                    Arg::Int(n) => json!(n),
                    Arg::Text(text) => text.to_json(),
                }).collect::<Vec<_>>(),
            }),
            Text::Raw(text) => json!({ "raw": text }),
            Text::Join(parts) => {
                json!({ "join": parts.iter().map(Text::to_json).collect::<Vec<_>>() })
            }
        }
    }
}

/// An Android string resource's text escaped as aapt reads it: `\`, `'`,
/// `"`, `@` and `?` at the start, `&`, `<` and `>`.
pub fn android_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '@' | '?' if i == 0 => {
                out.push('\\');
                out.push(c);
            }
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// A phone's string resources for `lang`: every string of `catalogue`, in
/// the table's order, headed by a note that the file is generated from
/// `source` by the command `by`.
pub fn android_xml<K: Key>(
    lang: Lang,
    catalogue: &[(K, &str, &str, &str)],
    source: &str,
    by: &str,
) -> String {
    let mut xml = String::from("<?xml version='1.0' encoding='utf-8'?>\n<!-- Generated from ");
    xml.push_str(source);
    xml.push_str(" by `");
    xml.push_str(by);
    xml.push_str("`. Do not edit. -->\n<resources>\n");
    for (id, key, _, _) in catalogue {
        xml.push_str("    <string name=\"");
        xml.push_str(key);
        xml.push_str("\">");
        xml.push_str(&android_escape(id.text(lang)));
        xml.push_str("</string>\n");
    }
    xml.push_str("</resources>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    crate::catalogue! {
        enum Words;
        const WORDS;
        Updates = "updates", "%1$d updates since %2$s", "%1$d Meldungen seit %2$s";
        Ago = "ago", "%1$s ago", "vor %1$s";
        Minutes = "minutes", "%1$d min", "%1$d Min.";
        Quote = "quote", "it's <b> & \"x\"", "@da";
    }

    #[test]
    fn the_macro_declares_keys_texts_and_the_table() {
        assert_eq!(WORDS.len(), 4);
        for (id, key, en, de) in WORDS {
            assert_eq!(Key::key(*id), *key);
            assert_eq!(Key::text(*id, Lang::En), *en);
            assert_eq!(Key::text(*id, Lang::De), *de);
        }
    }

    #[test]
    fn the_format_fills_android_places() {
        let args = [Arg::<Words>::Int(3), Arg::Str("12:00".to_string())];
        assert_eq!(
            format(Words::Updates.text(Lang::De), &args, Lang::De),
            "3 Meldungen seit 12:00"
        );
        assert_eq!(
            format("100%% %1$s %3$s", &args[..1], Lang::En),
            "100% 3 %3$s"
        );
        assert_eq!(format::<Words>("%", &[], Lang::En), "%");
        assert_eq!(format::<Words>("%1$x %0$s", &[], Lang::En), "%1$x %0$s");
    }

    #[test]
    fn a_text_resolves_nested_and_joined() {
        let minutes = Text::Key(Words::Minutes, vec![Arg::Int(2)]);
        let ago = Text::Key(Words::Ago, vec![Arg::Text(Box::new(minutes))]);
        assert_eq!(ago.resolve(Lang::En), "2 min ago");
        assert_eq!(ago.resolve(Lang::De), "vor 2 Min.");
        let joined = Text::Join(vec![Text::Raw("live".to_string()), ago]);
        assert_eq!(joined.resolve(Lang::En), "live · 2 min ago");
    }

    #[test]
    fn a_text_goes_to_json_unresolved() {
        let ago = Text::Key(
            Words::Ago,
            vec![Arg::Text(Box::new(Text::Key(
                Words::Minutes,
                vec![Arg::Int(2)],
            )))],
        );
        let joined = Text::Join(vec![Text::Raw("x".to_string()), ago]);
        assert_eq!(
            joined.to_json().to_string(),
            r#"{"join":[{"raw":"x"},{"args":[{"args":[2],"key":"minutes"}],"key":"ago"}]}"#
        );
    }

    #[test]
    fn a_locale_names_its_language() {
        for tag in ["de", "DE", "de-AT", "de_DE"] {
            assert_eq!(Lang::of_tag(tag), Lang::De, "{tag}");
        }
        for tag in ["", "en", "en-US", "fr", "dee", "d"] {
            assert_eq!(Lang::of_tag(tag), Lang::En, "{tag}");
        }
    }

    #[test]
    fn android_gets_every_string_escaped_under_its_header() {
        let xml = android_xml(Lang::De, WORDS, "x/strings.rs", "just x");
        assert!(xml.starts_with(
            "<?xml version='1.0' encoding='utf-8'?>\n\
             <!-- Generated from x/strings.rs by `just x`. Do not edit. -->\n\
             <resources>\n"
        ));
        assert!(xml.contains("    <string name=\"quote\">\\@da</string>\n"));
        assert!(xml.ends_with("</resources>\n"));
        assert_eq!(
            android_escape("it's <b> & \"x\""),
            "it\\'s &lt;b&gt; &amp; \\\"x\\\""
        );
        assert_eq!(android_escape("@home ?"), "\\@home ?");
    }
}
