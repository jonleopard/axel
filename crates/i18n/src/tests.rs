use super::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Directory levels below `crates/` the source scan descends: far deeper than
/// any module tree here, and a bound against a symlink loop.
const SCAN_DEPTH_MAX: usize = 12;

fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/i18n sits in crates/").to_path_buf()
}

fn rust_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    assert!(depth <= SCAN_DEPTH_MAX, "source tree deeper than {SCAN_DEPTH_MAX} levels at {}", dir.display());
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() && path.file_name().is_some_and(|n| n != "target") {
            rust_files(&path, depth + 1, out);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The literal at the start of `src` (just past its opening quote), unescaped
/// as Rust would, and the rest after its closing quote.
fn string_literal(src: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = src.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &src[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '0' => out.push('\0'),
                // A line continuation: the newline and the next line's indent vanish.
                '\n' => {
                    let rest = &src[i + 2..];
                    let skipped = rest.len() - rest.trim_start().len();
                    for _ in 0..rest[..skipped].chars().count() {
                        chars.next();
                    }
                }
                other => panic!("unsupported escape \\{other} in a t!() key"),
            },
            c => out.push(c),
        }
    }
    None
}

/// Every `t!("…")` key in the workspace's code, with where it is.
fn keys_in_code() -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(&workspace_crates(), 0, &mut files);
    let mut keys = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let mut from = 0;
        while let Some(found) = text[from..].find("t!(") {
            let at = from + found;
            from = at + 3;
            let before = text[..at].chars().next_back();
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            // `format!(` and `eprint!(` end in `t!(` too; comments and strings may mention it.
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '"') || text[line_start..at].contains("//") {
                continue;
            }
            let body = text[from..].trim_start();
            // The macro's own definition, and prose that mentions `t!()`.
            if body.starts_with("$key") || body.starts_with(')') {
                continue;
            }
            let Some(after_quote) = body.strip_prefix('"') else {
                panic!("{}: t!() takes a plain string literal as its key, found {:?}", file.display(), body.chars().take(40).collect::<String>());
            };
            let (key, after) = string_literal(after_quote).unwrap_or_else(|| panic!("{}: unterminated t!() key", file.display()));
            keys.push((key, file.display().to_string()));
            from = text.len() - after.len();
        }
    }
    keys
}

fn translations() -> HashMap<String, HashMap<String, String>> {
    parse(TRANSLATIONS_YAML).expect("translations.yaml parses as English key → locale → text")
}

fn placeholders(text: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if after[..close].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && close > 0 => {
                out.insert(&after[..close]);
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    out
}

#[test]
fn every_key_in_the_code_is_translated_into_every_locale() {
    let file = translations();
    let mut missing = BTreeSet::new();
    for (key, path) in keys_in_code() {
        for locale in Locale::ALL.into_iter().filter(|l| *l != Locale::En) {
            let translated = file.get(&key).and_then(|by| by.get(locale.code())).is_some_and(|t| !t.trim().is_empty());
            if !translated {
                missing.insert(format!("{} {key:?} ({path})", locale.code()));
            }
        }
    }
    assert!(missing.is_empty(), "add these to crates/i18n/translations.yaml:\n{}", missing.into_iter().collect::<Vec<_>>().join("\n"));
}

#[test]
fn every_key_in_the_file_is_used_in_the_code() {
    let used: BTreeSet<String> = keys_in_code().into_iter().map(|(k, _)| k).collect();
    let stale: Vec<String> = translations().into_keys().filter(|k| !used.contains(k)).collect();
    assert!(stale.is_empty(), "no t!() uses these keys any more; remove them from translations.yaml:\n{}", stale.join("\n"));
}

#[test]
fn translations_keep_the_keys_placeholders_and_name_known_locales() {
    let known: BTreeSet<&str> = Locale::ALL.iter().map(|l| l.code()).collect();
    for (key, by_locale) in translations() {
        for (code, text) in &by_locale {
            assert!(known.contains(code.as_str()) && code != "en", "{key:?}: unknown locale column {code:?}");
            assert_eq!(placeholders(text), placeholders(&key), "{key:?} in {code}: placeholders differ: {text:?}");
        }
    }
}

#[test]
fn a_tag_names_its_locale_whatever_its_spelling() {
    assert_eq!(Locale::from_tag("zh_CN.UTF-8"), Some(Locale::ZhHans));
    assert_eq!(Locale::from_tag("zh-Hans-CN"), Some(Locale::ZhHans));
    assert_eq!(Locale::from_tag("zh_TW"), Some(Locale::ZhHans));
    assert_eq!(Locale::from_tag("en_US.UTF-8"), Some(Locale::En));
    assert_eq!(Locale::from_tag("fr_FR"), None);
    assert_eq!(Locale::from_tag(""), None);
}

#[test]
fn fill_replaces_named_placeholders_and_leaves_other_braces() {
    let who = "atlas";
    let filled = fill("{who} asks: {title} {not_an_arg} {", &[("who", &who), ("title", &"Ready?")]);
    assert_eq!(filled, "atlas asks: Ready? {not_an_arg} {");
    // The macro, against the English source (no locale table is loaded for En).
    assert!(init(Locale::En));
    assert_eq!(t!("{who} asks: {title}", who = who, title = "Ready?"), "atlas asks: Ready?");
}

#[test]
fn the_chinese_table_holds_the_file_and_falls_back_to_english() {
    let zh = load(Locale::ZhHans);
    assert_eq!(zh.get("{who} asks: {title}").map(String::as_str), Some("{who} 提问：{title}"));
    assert!(load(Locale::En).is_empty());
}
