//! Axel's user-facing text in every language it speaks.
//!
//! The English text is its own key: `t!("Waiting for the agent…")` shows the
//! translation for the active locale, or the English when there is none.
//! Every translation lives in one file, `translations.yaml` at this crate's
//! root, compiled into the binary. Placeholders are named:
//! `t!("{who} asks: {title}", who = name, title = prompt)`.
//!
//! The locale is picked once, from `AXEL_LANG`, then `LC_ALL`, `LC_MESSAGES`
//! and `LANG`, then (on macOS) the system's preferred languages.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;

/// The translations, as written: English key → locale code → text.
pub const TRANSLATIONS_YAML: &str = include_str!("../translations.yaml");

/// A language Axel speaks.  English is the source text, so it needs no table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
    ZhHans,
}

impl Locale {
    pub const ALL: [Locale; 2] = [Locale::En, Locale::ZhHans];

    /// The code used as a column in `translations.yaml` (BCP 47).
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::ZhHans => "zh-Hans",
        }
    }

    /// The locale a language tag asks for: `zh_CN.UTF-8`, `zh-Hans`, `en_US`…
    /// `None` for a language Axel doesn't speak, and for `C` / `POSIX` (no
    /// preference at all).
    pub fn from_tag(tag: &str) -> Option<Locale> {
        let tag = tag.trim();
        let base = tag.split(['.', '@']).next().unwrap_or(tag).replace('_', "-").to_ascii_lowercase();
        let language = base.split('-').next().unwrap_or("");
        match language {
            "en" => Some(Locale::En),
            // Every Chinese variant reads Simplified for now: closer than English
            // for a Traditional reader, until a zh-Hant column exists.
            "zh" => Some(Locale::ZhHans),
            _ => None,
        }
    }
}

/// Environment variables that name a locale, most specific first.
const LOCALE_VARIABLES: [&str; 4] = ["AXEL_LANG", "LC_ALL", "LC_MESSAGES", "LANG"];

/// The locale this process shows text in.
pub fn locale() -> Locale {
    *ACTIVE.get_or_init(detect)
}

/// Fix the locale before any text is shown (tests, a `--lang` flag).
/// Returns `false` when text has already been shown in another one.
pub fn init(locale: Locale) -> bool {
    ACTIVE.set(locale).is_ok() || ACTIVE.get() == Some(&locale)
}

static ACTIVE: OnceLock<Locale> = OnceLock::new();

/// The first variable that is set decides: a language Axel doesn't speak
/// means English, not a look further down the list.  Unset (or `C`) all the
/// way down, macOS's own language preference decides.
pub fn detect() -> Locale {
    for name in LOCALE_VARIABLES {
        let Ok(value) = std::env::var(name) else { continue };
        let value = value.trim();
        if value.is_empty() || value == "C" || value == "POSIX" || value.starts_with("C.") {
            continue;
        }
        return Locale::from_tag(value).unwrap_or(Locale::En);
    }
    system_locale().unwrap_or(Locale::En)
}

/// macOS keeps the user's language order in `AppleLanguages`, which
/// terminals don't always export as `LANG`.
#[cfg(target_os = "macos")]
fn system_locale() -> Option<Locale> {
    let output = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    // `(\n    "zh-Hans-CN",\n    "en-US"\n)`: the first quoted tag is the preferred one.
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.split('"').nth(1)?;
    Locale::from_tag(first)
}

#[cfg(not(target_os = "macos"))]
fn system_locale() -> Option<Locale> {
    None
}

/// The active locale's table: English key → translation.
fn table() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| load(locale()))
}

fn load(locale: Locale) -> HashMap<String, String> {
    if locale == Locale::En {
        return HashMap::new();
    }
    // The file is checked by this crate's tests, so a parse failure is a bug
    // in a build that skipped them; English is still a working UI.
    let all = parse(TRANSLATIONS_YAML).unwrap_or_default();
    all.into_iter().filter_map(|(key, mut by_locale)| Some((key, by_locale.remove(locale.code())?))).collect()
}

/// English key → locale code → text.
pub fn parse(yaml: &str) -> Result<HashMap<String, HashMap<String, String>>, serde_norway::Error> {
    serde_norway::from_str(yaml)
}

/// `key` in the active locale, or `key` itself.
pub fn translate(key: &'static str) -> &'static str {
    table().get(key).map(String::as_str).unwrap_or(key)
}

/// `template` with each `{name}` replaced by its value; braces that name no
/// argument are left as they are.
pub fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16 * args.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let named = after.find('}').and_then(|close| {
            let name = &after[..close];
            args.iter().find(|(n, _)| *n == name).map(|(_, value)| (close, *value))
        });
        match named {
            Some((close, value)) => {
                out.push_str(&value.to_string());
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Text shown to a person, in their language.
///
/// `t!("Esc to cancel")` is a `&'static str`;
/// `t!("{n} agents", n = count)` is a `String` with the placeholders filled.
/// The key must be a plain string literal: the tests read it from the source
/// to check that `translations.yaml` has it.
#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::translate($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::fill($crate::translate($key), &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

#[cfg(test)]
mod tests;
