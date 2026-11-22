//! Questions for the owner, shown by every connected Rex app.
//!
//! Rex can put a question in the command palette of every app connected to a
//! server, the iOS app included; the first answer wins and the prompt closes
//! everywhere else (`rex.ui.pick`, `rex.ui.input`, API revision 17). Only an
//! action from Rex's configuration file may ask, so Axel installs a small Lua
//! module there ([`install`]) whose `axel_ask` action asks and returns the
//! outcome. [`crate::Rex::ask`] runs that action and waits for the answer.
//!
//! Everything here but [`crate::Rex::ask`] is pure or touches only files, so
//! it is tested without a server.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The action the module defines.
pub const ACTION: &str = "axel_ask";
/// The Lua module, before [`module_source`] fills in `@AXEL@` and `@PATH@`.
const MODULE_TEMPLATE: &str = include_str!("axel.lua");
/// The module's version, as `axel.lua` declares it. A module with another
/// version is replaced on first use; the same version is left alone.
pub const MODULE_VERSION: u32 = 2;
/// The module's file name in Rex's configuration directory.
pub const MODULE_FILE: &str = "axel.lua";
/// Rex keeps at most 2,000 items in a pick (Rex docs, "Custom UI").
pub const ITEMS_MAX: usize = 2_000;
/// Rex waits at most an hour for an answer (Rex docs, "Custom UI").
pub const TIMEOUT_SECONDS_MAX: u32 = 3_600;
/// A title the palette can show in full; longer ones are cut with an ellipsis.
pub const TITLE_CHARS_MAX: usize = 200;

/// The lines `install` adds to `init.lua`. The markers let `remove` find them.
const FENCE_BEGIN: &str = "-- >>> axel: managed by `axel rex setup`; remove with `axel rex setup --remove`";
const FENCE_BODY: &str = "require(\"axel\")";
const FENCE_END: &str = "-- <<< axel";

/// One choice in a pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// What the answer carries back; unique within the pick.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
}

/// How many items the owner may choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// One item, or with `allow_other` a line of typed text instead.
    Single { allow_other: bool },
    /// Any number of items, including none.
    Multiple,
}

/// A question with choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub title: String,
    /// The hint in the field that filters the items.
    pub placeholder: Option<String>,
    pub items: Vec<Item>,
    pub selection: Selection,
    pub timeout_seconds: u32,
}

/// How a question ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The ids of the items chosen, in the pick's order. One for a single pick.
    Ids(Vec<String>),
    /// Text typed instead of choosing (`Selection::Single { allow_other: true }`).
    Text(String),
    Unanswered(Unanswered),
}

/// Why a question has no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unanswered {
    /// The owner closed the prompt.
    Cancelled,
    /// Nobody answered in time.
    Timeout,
    /// No Rex app is connected.
    NoClient,
    /// No connected app can show prompts.
    Unsupported,
    /// Every app that could show it is showing another prompt.
    Busy,
    /// Axel stopped waiting, because the question was answered elsewhere.
    Withdrawn,
    /// Rex's configuration has no `axel_ask`: [`install`] hasn't run, or Rex
    /// hasn't reloaded since.
    NotInstalled,
    /// Anything else, with Rex's explanation.
    Failed(String),
}

impl Unanswered {
    /// The reasons `rex.ui` gives, as its docs list them.
    fn from_reason(reason: &str, detail: Option<&str>) -> Unanswered {
        match reason {
            "cancelled" => Unanswered::Cancelled,
            "timeout" => Unanswered::Timeout,
            "no_client" => Unanswered::NoClient,
            "unsupported" => Unanswered::Unsupported,
            "busy" => Unanswered::Busy,
            other => Unanswered::Failed(detail.map_or_else(|| other.to_string(), |d| format!("{other}: {d}"))),
        }
    }

    /// Whether another surface should ask instead: Rex couldn't put the
    /// question to anyone. The owner's own cancel or silence is not that.
    pub fn is_unreachable(&self) -> bool {
        match self {
            Unanswered::NoClient | Unanswered::Unsupported | Unanswered::NotInstalled | Unanswered::Failed(_) => true,
            Unanswered::Cancelled | Unanswered::Timeout | Unanswered::Busy | Unanswered::Withdrawn => false,
        }
    }
}

/// The `client.action` arguments that run `axel_ask` for `pick`.
pub fn action_args(pick: &Pick) -> Value {
    assert!(!pick.items.is_empty(), "a pick offers at least one item");
    assert!(pick.items.len() <= ITEMS_MAX, "Rex shows at most {ITEMS_MAX} items");
    assert!(pick.timeout_seconds > 0 && pick.timeout_seconds <= TIMEOUT_SECONDS_MAX);
    let items: Vec<Value> = pick
        .items
        .iter()
        .map(|item| {
            assert!(!item.id.is_empty(), "an item's id is what the answer carries");
            let mut entry = json!({ "id": item.id, "title": item.title });
            if let Some(subtitle) = &item.subtitle {
                entry["subtitle"] = json!(subtitle);
            }
            entry
        })
        .collect();
    let mut args = json!({
        "title": short_title(&pick.title),
        "items": items,
        "timeout": pick.timeout_seconds,
    });
    // JSON null reaches Lua as a value, not nil: leave absent fields out.
    if let Some(placeholder) = &pick.placeholder {
        args["placeholder"] = json!(placeholder);
    }
    match pick.selection {
        Selection::Multiple => args["multiple"] = json!(true),
        Selection::Single { allow_other: true } => args["allow_other"] = json!(true),
        Selection::Single { allow_other: false } => {}
    }
    json!({ "action": ACTION, "args": args })
}

/// What `axel_ask` returned (the `result` of `client.action`), for `pick`.
/// Ids the pick didn't offer are refused, so a stale module can't answer
/// with something Axel never asked.
pub fn parse_result(result: &Value, pick: &Pick) -> Result<Answer, String> {
    let outcome = result.get("outcome").and_then(Value::as_str).ok_or_else(|| format!("no outcome in {result}"))?;
    match outcome {
        "ids" => {
            // An empty Lua list arrives as `{}`, so `count` says how many there are.
            let count = result.get("count").and_then(Value::as_u64).ok_or("ids without a count")?;
            let ids: Vec<String> = match result.get("ids") {
                Some(Value::Array(ids)) => ids.iter().filter_map(|id| id.as_str().map(String::from)).collect(),
                _ => Vec::new(),
            };
            if ids.len() as u64 != count {
                return Err(format!("{count} ids announced, {} received", ids.len()));
            }
            if let Some(unknown) = ids.iter().find(|id| !pick.items.iter().any(|item| &item.id == *id)) {
                return Err(format!("the answer names {unknown}, which wasn't offered"));
            }
            let is_single = matches!(pick.selection, Selection::Single { .. });
            if is_single && ids.len() != 1 {
                return Err(format!("a single pick answered with {} ids", ids.len()));
            }
            Ok(Answer::Ids(ids))
        }
        "text" => {
            let text = result.get("text").and_then(Value::as_str).ok_or("text outcome without text")?;
            Ok(Answer::Text(text.to_string()))
        }
        reason => {
            let detail = result.get("detail").and_then(Value::as_str);
            Ok(Answer::Unanswered(Unanswered::from_reason(reason, detail)))
        }
    }
}

/// `title` cut to [`TITLE_CHARS_MAX`] characters, on one line.
fn short_title(title: &str) -> String {
    let line = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TITLE_CHARS_MAX {
        return line;
    }
    let mut cut: String = line.chars().take(TITLE_CHARS_MAX - 1).collect();
    cut.push('…');
    cut
}

/// Rex's configuration directory: the one holding `$REX_CONFIG`, else
/// `$XDG_CONFIG_HOME/rex`, else `~/.config/rex` (`rex config --help`).
pub fn config_dir() -> Option<PathBuf> {
    let non_empty = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(file) = non_empty("REX_CONFIG") {
        return file.parent().map(Path::to_path_buf);
    }
    if let Some(xdg) = non_empty("XDG_CONFIG_HOME") {
        return Some(xdg.join("rex"));
    }
    non_empty("HOME").map(|home| home.join(".config/rex"))
}

/// What [`install`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Installed {
    pub is_module_written: bool,
    pub is_init_changed: bool,
}

impl Installed {
    /// Whether Rex has to reload its configuration to see the change.
    pub fn needs_reload(&self) -> bool {
        self.is_module_written || self.is_init_changed
    }
}

/// The module for the `axel` at `axel`, whose commands run with `path` as
/// their PATH (Rex's own is the bare one launchd gives an app).
pub fn module_source(axel: &str, path: &str) -> String {
    let declared = format!("local MODULE_VERSION = {MODULE_VERSION}\n");
    assert!(MODULE_TEMPLATE.contains(&declared), "axel.lua declares MODULE_VERSION {MODULE_VERSION}");
    // Both go into Lua long strings, which nothing inside can end but this.
    assert!(!axel.contains("]==]") && !path.contains("]==]"), "a path can't hold a Lua long-string end");
    MODULE_TEMPLATE.replace("@AXEL@", axel).replace("@PATH@", path)
}

/// Write `source` (from [`module_source`]) into `dir` as the module and
/// require it from `init.lua`, creating either file as needed. Running it
/// again with the same source changes nothing.
pub fn install(dir: &Path, source: &str) -> std::io::Result<Installed> {
    std::fs::create_dir_all(dir)?;
    let module = dir.join(MODULE_FILE);
    let is_module_written = std::fs::read_to_string(&module).ok().as_deref() != Some(source);
    if is_module_written {
        write_atomically(&module, source)?;
    }
    let init = dir.join("init.lua");
    let before = read_or_empty(&init)?;
    let after = with_fence(&before);
    let is_init_changed = after != before;
    if is_init_changed {
        write_atomically(&init, &after)?;
    }
    Ok(Installed { is_module_written, is_init_changed })
}

/// Undo [`install`]: drop the module and the lines it added to `init.lua`.
/// Returns whether anything changed.
pub fn remove(dir: &Path) -> std::io::Result<bool> {
    let module = dir.join(MODULE_FILE);
    let is_module_removed = match std::fs::remove_file(&module) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    let init = dir.join("init.lua");
    let before = read_or_empty(&init)?;
    let after = without_fence(&before);
    if after != before {
        write_atomically(&init, &after)?;
    }
    Ok(is_module_removed || after != before)
}

/// Make the Rex server read its configuration again, as after [`install`]
/// or [`remove`] (`rex config reload`; `REX_SERVER` names the server).
pub fn reload() -> Result<(), crate::Error> {
    let program = crate::program();
    let out = std::process::Command::new(&program)
        .args(["config", "reload"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|source| crate::Error::Spawn { program, source })?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Err(crate::Error::Rex { method: "config reload".into(), stderr })
}

/// Whether `dir` holds this version of the module, required from `init.lua`.
/// Where it points and its PATH may differ: `axel rex setup` refreshes those.
pub fn is_current(dir: &Path) -> bool {
    let module = std::fs::read_to_string(dir.join(MODULE_FILE)).unwrap_or_default();
    let init = std::fs::read_to_string(dir.join("init.lua")).unwrap_or_default();
    module.contains(&format!("local MODULE_VERSION = {MODULE_VERSION}\n")) && init.contains(FENCE_BEGIN)
}

/// `init` with Axel's fenced `require` at its end, once.
fn with_fence(init: &str) -> String {
    let mut out = without_fence(init);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&format!("{FENCE_BEGIN}\n{FENCE_BODY}\n{FENCE_END}\n"));
    out
}

/// `init` without Axel's fenced lines, and without the blank line before them.
fn without_fence(init: &str) -> String {
    let Some(begin) = init.find(FENCE_BEGIN) else { return init.to_string() };
    let Some(end_at) = init[begin..].find(FENCE_END).map(|i| begin + i + FENCE_END.len()) else {
        // A begin marker without its end: someone edited it. Leave it alone.
        return init.to_string();
    };
    let tail = init[end_at..].strip_prefix('\n').unwrap_or(&init[end_at..]);
    let head = init[..begin].strip_suffix("\n\n").map(|h| format!("{h}\n")).unwrap_or_else(|| init[..begin].to_string());
    format!("{head}{tail}")
}

fn read_or_empty(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Write `text` to `path` through a temporary file, so Rex never reads half.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let temporary = path.with_extension("lua.axel-tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(selection: Selection) -> Pick {
        Pick {
            title: "Which database?".into(),
            placeholder: None,
            items: vec![
                Item { id: "answer:0".into(), title: "Postgres".into(), subtitle: None },
                Item { id: "answer:1".into(), title: "SQLite".into(), subtitle: Some("embedded".into()) },
            ],
            selection,
            timeout_seconds: 60,
        }
    }

    #[test]
    fn action_args_leave_absent_fields_out() {
        let args = action_args(&pick(Selection::Single { allow_other: false }));
        assert_eq!(args["action"], "axel_ask");
        let fields = &args["args"];
        assert_eq!(fields["items"][0], json!({"id": "answer:0", "title": "Postgres"}));
        assert_eq!(fields["items"][1]["subtitle"], "embedded");
        for absent in ["placeholder", "multiple", "allow_other"] {
            assert!(fields.get(absent).is_none(), "{absent} in {fields}");
        }
        let multiple = action_args(&pick(Selection::Multiple));
        assert_eq!(multiple["args"]["multiple"], true);
        let other = action_args(&pick(Selection::Single { allow_other: true }));
        assert_eq!(other["args"]["allow_other"], true);
    }

    #[test]
    fn long_titles_are_cut_to_one_line() {
        let mut p = pick(Selection::Multiple);
        p.title = format!("first\nsecond {}", "x".repeat(400));
        let title = action_args(&p)["args"]["title"].as_str().unwrap().to_string();
        assert_eq!(title.chars().count(), TITLE_CHARS_MAX);
        assert!(title.starts_with("first second ") && title.ends_with('…'), "{title}");
    }

    #[test]
    fn answers_parse_and_unknown_ids_are_refused() {
        let single = pick(Selection::Single { allow_other: true });
        let ids = json!({"outcome": "ids", "ids": ["answer:1"], "count": 1});
        assert_eq!(parse_result(&ids, &single), Ok(Answer::Ids(vec!["answer:1".into()])));
        let text = json!({"outcome": "text", "text": "MySQL"});
        assert_eq!(parse_result(&text, &single), Ok(Answer::Text("MySQL".into())));
        let unknown = json!({"outcome": "ids", "ids": ["answer:9"], "count": 1});
        assert!(parse_result(&unknown, &single).is_err());
        let two = json!({"outcome": "ids", "ids": ["answer:0", "answer:1"], "count": 2});
        assert!(parse_result(&two, &single).is_err(), "a single pick has one answer");
        assert!(parse_result(&json!({}), &single).is_err());
    }

    #[test]
    fn an_empty_multiple_answer_arrives_as_an_object() {
        let none = json!({"outcome": "ids", "ids": {}, "count": 0});
        assert_eq!(parse_result(&none, &pick(Selection::Multiple)), Ok(Answer::Ids(vec![])));
        let lying = json!({"outcome": "ids", "ids": {}, "count": 2});
        assert!(parse_result(&lying, &pick(Selection::Multiple)).is_err());
    }

    #[test]
    fn reasons_map_and_only_unreachable_ones_fall_back() {
        let p = pick(Selection::Multiple);
        let reason = |r: &str| match parse_result(&json!({"outcome": r, "detail": "why"}), &p) {
            Ok(Answer::Unanswered(u)) => u,
            other => panic!("{other:?}"),
        };
        assert_eq!(reason("cancelled"), Unanswered::Cancelled);
        assert_eq!(reason("busy"), Unanswered::Busy);
        assert_eq!(reason("failed"), Unanswered::Failed("failed: why".into()));
        assert!(reason("no_client").is_unreachable());
        assert!(reason("unsupported").is_unreachable());
        assert!(!reason("cancelled").is_unreachable());
        assert!(!reason("timeout").is_unreachable());
        assert!(!Unanswered::Withdrawn.is_unreachable());
    }

    #[test]
    fn the_fence_is_added_once_and_removed_cleanly() {
        let own = "rex.bind(\"ctrl+g\", \"pane.zoom\")\n";
        let once = with_fence(own);
        assert!(once.starts_with(own) && once.contains("require(\"axel\")"), "{once}");
        assert_eq!(with_fence(&once), once, "installing twice changes nothing");
        assert_eq!(without_fence(&once), own);
        assert_eq!(without_fence(&with_fence("")), "");
        let no_newline = with_fence("local x = 1");
        assert_eq!(without_fence(&no_newline), "local x = 1\n");
    }

    #[test]
    fn the_module_is_rendered_with_its_program_and_path() {
        let source = module_source("/Users/me/.local/bin/axel", "/opt/homebrew/bin:/usr/bin");
        assert!(source.contains("local AXEL = [==[/Users/me/.local/bin/axel]==]"));
        assert!(source.contains("local PATH = [==[/opt/homebrew/bin:/usr/bin]==]"));
        assert!(!source.contains("@AXEL@") && !source.contains("@PATH@"));
    }

    #[test]
    fn a_fence_someone_broke_is_left_alone() {
        let broken = format!("{FENCE_BEGIN}\nrequire(\"axel\")\n-- the end marker was deleted\n");
        assert_eq!(without_fence(&broken), broken);
    }

    #[test]
    fn install_and_remove_round_trip_in_a_directory() {
        let dir = std::env::temp_dir().join(format!("axel-rex-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // A leftover from a crashed run; absent is fine.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.lua"), "-- mine\n").unwrap();
        let source = module_source("/opt/axel/bin/axel", "/usr/bin:/bin");
        let first = install(&dir, &source).unwrap();
        assert!(first.needs_reload());
        assert!(is_current(&dir));
        assert!(!install(&dir, &source).unwrap().needs_reload(), "a second install is a no-op");
        let moved = module_source("/elsewhere/axel", "/usr/bin:/bin");
        assert!(install(&dir, &moved).unwrap().is_module_written, "a new path rewrites the module");
        assert!(remove(&dir).unwrap());
        assert!(!is_current(&dir));
        assert_eq!(std::fs::read_to_string(dir.join("init.lua")).unwrap(), "-- mine\n");
        assert!(!remove(&dir).unwrap(), "nothing left to remove");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
