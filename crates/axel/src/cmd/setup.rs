//! `axel setup`: the first-run walkthrough.  `axel` runs it by itself the first
//! time, when there is no agent yet.  It checks this machine (Rex, the harness
//! CLIs, their ACP adapters), offers to install what's missing, and creates the
//! first agent.  Dialoguer, themed like the TUI: coral red accent and values,
//! dim hints.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use axel_i18n::t;
use axel_store::{AgentColor, Harness, RuntimeMode};
use console::{Style, Term, style};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, MultiSelect, Select};

use super::Ctx;
use super::launch::which;

// The TUI's palette (`crates/tui/src/theme.rs`).
const ACCENT: console::Color = console::Color::TrueColor(255, 117, 101);
const YOU: console::Color = ACCENT;
const OK: u8 = 108;
const DANGER: u8 = 167;
const DIM: u8 = 244;
/// The mascot's spectrum, for the wordmark: sun, tangerine, pink, violet, blue.
const SPECTRUM: [u8; 5] = [220, 208, 205, 135, 33];

/// Dialoguer's colourful theme, recoloured to match the TUI.
fn theme() -> ColorfulTheme {
    let c = |n: u8| Style::new().for_stderr().color256(n);
    ColorfulTheme {
        defaults_style: c(DIM),
        prompt_style: Style::new().for_stderr().bold(),
        prompt_prefix: style("  ›".to_string())
            .for_stderr()
            .fg(ACCENT)
            .bold(),
        prompt_suffix: style("".to_string()).for_stderr(),
        success_prefix: style("  ✓".to_string()).for_stderr().color256(OK),
        success_suffix: style("·".to_string()).for_stderr().color256(DIM),
        error_prefix: style("  ✗".to_string()).for_stderr().color256(DANGER),
        error_style: c(DANGER),
        hint_style: c(DIM),
        values_style: Style::new().for_stderr().fg(YOU),
        active_item_style: Style::new().for_stderr().fg(YOU).bold(),
        inactive_item_style: Style::new().for_stderr(),
        active_item_prefix: style("    ▸".to_string()).for_stderr().fg(ACCENT),
        inactive_item_prefix: style("     ".to_string()).for_stderr(),
        checked_item_prefix: style("    ◼".to_string()).for_stderr().fg(ACCENT),
        unchecked_item_prefix: style("    ◻".to_string()).for_stderr().color256(DIM),
        picked_item_prefix: style("    ▸".to_string()).for_stderr().fg(ACCENT),
        unpicked_item_prefix: style("     ".to_string()).for_stderr(),
    }
}

fn dim(s: &str) -> String {
    style(s).color256(DIM).to_string()
}

fn heading(s: &str) {
    eprintln!("\n  {}", style(s).bold());
}

/// `axel` in the spectrum, one colour per letter.
fn wordmark() -> String {
    "axel"
        .chars()
        .zip(SPECTRUM)
        .map(|(ch, n)| style(ch).color256(n).bold().to_string())
        .collect()
}

/// The program behind the ACP adapter that lets Axel drive a harness (its
/// thread view, policies).  Claude's and Codex's are built in.
fn adapter(h: &Harness) -> Option<String> {
    axel_acp::adapter_command(h).first().cloned()
}

/// Whether `axel` should walk through setup before opening: no agent yet.
pub fn needed(ctx: &Ctx) -> bool {
    ctx.store
        .list_agents()
        .map(|a| a.is_empty())
        .unwrap_or(false)
}

/// Whether we can ask questions here.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// What to do once setup is through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Open the launcher.
    Open,
    /// Back to the shell.
    Quit,
}

/// Names offered as the default, the first one not taken.
const NAMES: [&str; 6] = ["atlas", "nova", "vega", "lyra", "orion", "juno"];

pub fn run() -> Result<Next> {
    if !interactive() {
        bail!("{}", t!("axel setup asks questions; run it in a terminal"));
    }
    let ctx = Ctx::open()?;
    let theme = theme();
    let term = Term::stderr();
    // Clearing is cosmetic: on a terminal that refuses it, setup still reads fine.
    let _ = term.clear_screen();

    eprintln!();
    eprintln!(
        "  {}  {}",
        wordmark(),
        dim(t!("agent orchestration, designed for Rex"))
    );
    eprintln!("  {}", dim(t!("Let's check this machine and create an agent.")));

    let rex = check_machine();
    let installed = installed_harnesses();
    if installed.is_empty() {
        explain_no_harness();
        return Ok(Next::Quit);
    }

    // ---- agents, one or more --------------------------------------------------
    let mut again = false;
    loop {
        let first = ctx
            .store
            .list_agents()
            .map(|a| a.is_empty())
            .unwrap_or(true);
        if first {
            heading(t!("Your first agent"));
            eprintln!(
                "  {}",
                dim(t!("An agent is a name you can @mention, its own key, and how to run it."))
            );
        } else if !std::mem::replace(&mut again, true) {
            heading(t!("A new agent"));
        }
        eprintln!();

        let agent_name = create_agent(&ctx, &theme, &installed)?;

        // ---- next -----------------------------------------------------------------
        eprintln!();
        let choices = [
            t!("Open Axel and give @{name} something to do", name = agent_name),
            t!("Create another agent").to_string(),
        ];
        let pick = Select::with_theme(&theme)
            .with_prompt(t!("What next?"))
            .items(&choices)
            .default(0)
            .interact()?;
        if pick == 1 {
            again = true;
            heading(t!("Another agent"));
            eprintln!(
                "  {}",
                dim(t!("A builder and a reviewer on another model make a good pair."))
            );
            continue;
        }
        print_first_steps(&agent_name, rex);
        return Ok(Next::Open);
    }
}

/// Report Rex and git; true when we run inside a Rex session.
fn check_machine() -> bool {
    heading(t!("This machine"));
    let rex = axel_rex::detect().is_some();
    check(
        rex,
        "Rex",
        if rex {
            t!("you're in a Rex session").into()
        } else {
            dim(t!("not here: Axel runs inline (superlogical.com/rex)"))
        },
    );
    if rex {
        // Agents' questions reach every Rex app through a module in Rex's config.
        match super::ui::ensure_rex_module() {
            Ok(()) => check(true, t!("Rex questions"), t!("on every Rex app, Mac and iPhone").into()),
            Err(e) => check(false, t!("Rex questions"), dim(&t!("in a pane: {error}", error = format!("{e:#}")))),
        }
    }
    check(
        which("git").is_some(),
        "git",
        which("git")
            .map(|p| tilde(&p))
            .unwrap_or_else(|| t!("not found").into()),
    );
    rex
}

/// Every harness Axel knows that is on this machine, each reported with how
/// Axel would run it.  The rest show up only when there's none at all.
fn installed_harnesses() -> Vec<Harness> {
    let mut installed: Vec<Harness> = Vec::new();
    for h in &Harness::BUILTIN {
        let Some(found) = which(h.program()) else { continue };
        let adapter = adapter(h);
        let managed = adapter.as_deref().and_then(which).is_some();
        let how = match &adapter {
            _ if managed => format!("· {}", t!("managed")),
            // Its adapter is a separate install (Pi's, Amp's): name it.
            Some(a) if a != h.program() => format!("· {}", t!("in a pane until you install its ACP adapter, {adapter}", adapter = a)),
            _ => format!("· {}", t!("in a pane (no ACP adapter)")),
        };
        check(true, h.label(), format!("{}  {}", tilde(&found), dim(&how)));
        installed.push(h.clone());
    }
    installed
}

/// No harness on this machine: list them all, with how to install each.
fn explain_no_harness() {
    eprintln!();
    eprintln!(
        "  {}",
        style(t!("Axel runs your agents' own CLIs, and none is installed.")).color256(DANGER)
    );
    eprintln!(
        "  {}",
        dim(t!("Install one of them, log in once, then run `axel` again:"))
    );
    for h in &Harness::BUILTIN {
        eprintln!("    {:<13} {}", style(h.label()).bold(), dim(h.install()));
    }
}

/// Ask for one agent's name, harness, skills, colour and mode, create it, and
/// return its name.
fn create_agent(ctx: &Ctx, theme: &ColorfulTheme, installed: &[Harness]) -> Result<String> {
    assert!(!installed.is_empty(), "setup stops before asking when no harness is installed");
    let name = ask_name(ctx, theme)?;
    let harness = ask_harness(theme, installed)?;
    // Axel drives the harness's own CLI with its own login: without one,
    // the agent can't start (Pi, say, refuses every session).
    eprintln!(
        "  {}",
        dim(&t!("{harness} has to be logged in once: {how}.", harness = harness.label(), how = harness.login()))
    );

    let skills = pick_skills(theme)?;
    let color = ask_color(ctx, theme)?;
    let mode = ask_mode(theme)?;

    let agent = super::agent::create(
        ctx,
        &name,
        harness.clone(),
        None,
        skills,
        None,
        Some(color),
        mode,
    )
    .context(t!("creating the agent"))?;
    let key: String = agent.pubkey.chars().take(6).collect();
    eprintln!();
    eprintln!(
        "  {} {} {}",
        style("✓").color256(OK),
        style(&agent.name).color256(color_index(color)).bold(),
        dim(&format!("· {}", t!("key {key}… · attested by you", key = key)))
    );

    // Claude Code asks once per machine to accept bypass mode, which autopilot uses.
    if harness == Harness::Claude && mode == RuntimeMode::Autopilot && !claude_bypass_accepted()
    {
        offer_claude_bypass(theme)?;
    }
    Ok(agent.name)
}

fn ask_name(ctx: &Ctx, theme: &ColorfulTheme) -> Result<String> {
    let taken = |n: &str| ctx.store.agent_by_name(n).ok().flatten().is_some();
    let suggestion = NAMES
        .iter()
        .find(|n| !taken(n))
        .map(|n| n.to_string())
        .unwrap_or_default();
    let name: String = Input::with_theme(theme)
        .with_prompt(t!("Name"))
        .default(suggestion)
        .validate_with(|s: &String| -> Result<(), String> {
            let s = s.trim();
            if s.is_empty()
                || !s
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                Err(t!("letters, digits, - and _ only, so it works as @name").into())
            } else if ctx.store.agent_by_name(s).ok().flatten().is_some() {
                Err(t!("there's already an agent named {name}", name = s))
            } else {
                Ok(())
            }
        })
        .interact_text()?;
    Ok(name.trim().to_string())
}

fn ask_harness(theme: &ColorfulTheme, installed: &[Harness]) -> Result<Harness> {
    if let [only] = installed {
        eprintln!(
            "  {} {} {} {}",
            style("✓").color256(OK),
            style(t!("Harness")).bold(),
            dim("·"),
            style(only.label()).fg(YOU)
        );
        return Ok(only.clone());
    }
    let labels: Vec<String> = installed.iter().map(|h| h.label().to_string()).collect();
    let index = Select::with_theme(theme)
        .with_prompt(t!("Harness"))
        .items(&labels)
        .default(0)
        .interact()?;
    Ok(installed[index].clone())
}

/// The agent's colour; the default walks the palette from orange, one step
/// per agent already made.
fn ask_color(ctx: &Ctx, theme: &ColorfulTheme) -> Result<AgentColor> {
    let colors = AgentColor::ALL;
    let color_items: Vec<String> = colors
        .iter()
        .map(|c| format!("{} {c}", style("●").color256(color_index(*c))))
        .collect();
    let used = ctx.store.list_agents().map(|a| a.len()).unwrap_or(0);
    let default_color = colors
        .iter()
        .position(|c| *c == AgentColor::Orange)
        .unwrap_or(0);
    let index = Select::with_theme(theme)
        .with_prompt(t!("Colour"))
        .items(&color_items)
        .default((default_color + used) % colors.len())
        .interact()?;
    Ok(colors[index])
}

fn ask_mode(theme: &ColorfulTheme) -> Result<RuntimeMode> {
    let modes = [
        (
            RuntimeMode::Autopilot,
            "autopilot",
            t!("never asks; the guard still refuses irreversible commands"),
        ),
        (
            RuntimeMode::Edits,
            "edits",
            t!("file edits go through, anything else asks you"),
        ),
        (
            RuntimeMode::Supervised,
            "supervised",
            t!("asks before every action"),
        ),
        (RuntimeMode::Plan, "plan", t!("read-only: looks and proposes")),
    ];
    let mode_items: Vec<String> = modes
        .iter()
        .map(|(_, n, d)| format!("{n:<11} {d}"))
        .collect();
    let index = Select::with_theme(theme)
        .with_prompt(t!("Mode"))
        .items(&mode_items)
        .default(0)
        .interact()?;
    Ok(modes[index].0)
}

fn offer_claude_bypass(theme: &ColorfulTheme) -> Result<()> {
    eprintln!();
    let yes = Confirm::with_theme(theme)
        .with_prompt(t!("Skip Claude Code's one-time autopilot prompt?"))
        .default(true)
        .interact()?;
    if yes {
        match accept_claude_bypass() {
            Ok(path) => check(
                true,
                "Claude Code",
                dim(&t!("autopilot accepted in {path}", path = tilde(&path))),
            ),
            Err(e) => check(
                false,
                "Claude Code",
                t!("couldn't update its settings: {error}", error = format!("{e:#}")),
            ),
        }
    }
    Ok(())
}

fn print_first_steps(agent_name: &str, rex: bool) {
    eprintln!();
    eprintln!(
        "  {}",
        dim(t!("Type what you want done in the prompt and press Enter, e.g."))
    );
    eprintln!(
        "    {} {}",
        style("›").fg(ACCENT).bold(),
        style(t!("@{name} add a health check endpoint", name = agent_name)).fg(YOU)
    );
    if !rex {
        eprintln!(
            "  {}",
            dim(t!("Inside Rex, `axel` floats over your window; here it runs inline."))
        );
    }
    eprintln!();
}

/// A path with the home directory shown as `~`.
fn tilde(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    match dirs::home_dir().map(|h| h.display().to_string()) {
        Some(h) if s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

fn check(ok: bool, what: &str, detail: String) {
    let mark = if ok {
        style("✓").color256(OK)
    } else {
        style("✗").color256(DANGER)
    };
    // Padded by terminal columns before styling: the escapes take none.
    eprintln!("  {mark} {} {detail}", style(super::text::pad(what, 16)).bold());
}

/// The TUI's colour for an agent colour, as a 256-colour index.
fn color_index(c: AgentColor) -> u8 {
    match c {
        AgentColor::Purple => 5,
        AgentColor::Yellow => 3,
        AgentColor::Red => 1,
        AgentColor::Green => 2,
        AgentColor::Blue => 4,
        AgentColor::Gray => 7,
        AgentColor::Orange => 208,
    }
}

/// Skills from `~/.config/axel/skills` to give the agent.  A few chosen ones beat
/// all of them: each skill's description sits in every session's context.
fn pick_skills(theme: &ColorfulTheme) -> Result<Vec<String>> {
    let global = crate::paths::global_skills_dir();
    // An unreadable skills directory is the same as an empty one here: the
    // agent gets `*`, and launches report the directory's error themselves.
    let found = axel_config::find_all_skills(&[], Some(global.as_path())).unwrap_or_default();
    if found.is_empty() {
        eprintln!(
            "  {} {} {} {}",
            style("✓").color256(OK),
            style(t!("Skills")).bold(),
            dim("·"),
            dim(t!("none yet: all of them, once you add some"))
        );
        eprintln!("    {}", dim(&t!("skills live in {path}", path = tilde(&global))));
        return Ok(vec!["*".into()]);
    }
    let items: Vec<String> = found
        .values()
        .map(|s| match &s.description {
            Some(d) => format!(
                "{} {}",
                super::text::pad(&s.name, 20),
                d.chars().take(48).collect::<String>()
            ),
            None => s.name.clone(),
        })
        .collect();
    let picked = MultiSelect::with_theme(theme)
        .with_prompt(t!("Skills (Space picks, Enter when done)"))
        .items(&items)
        .interact()?;
    let names: Vec<String> = found
        .keys()
        .enumerate()
        .filter(|(i, _)| picked.contains(i))
        .map(|(_, k)| k.clone())
        .collect();
    Ok(if names.is_empty() {
        vec!["*".into()]
    } else {
        names
    })
}

fn claude_settings_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    Some(dir.join("settings.json"))
}

fn claude_bypass_accepted() -> bool {
    let Some(p) = claude_settings_path() else {
        return false;
    };
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("skipDangerousModePermissionPrompt")
                .and_then(|b| b.as_bool())
        })
        .unwrap_or(false)
}

fn accept_claude_bypass() -> Result<PathBuf> {
    let path = claude_settings_path().context(t!("no home directory"))?;
    let mut v: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).context(t!("reading Claude Code's settings.json"))?
        }
        _ => serde_json::json!({}),
    };
    let obj = v
        .as_object_mut()
        .context(t!("Claude Code's settings.json isn't an object"))?;
    obj.insert(
        "skipDangerousModePermissionPrompt".into(),
        serde_json::Value::Bool(true),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&v)? + "\n")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_shown_as_tilde() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(tilde(&home.join("Library/x")), "~/Library/x");
        assert_eq!(tilde(std::path::Path::new("/usr/bin/git")), "/usr/bin/git");
    }

    #[test]
    fn accepting_claude_bypass_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"model": "opus", "env": {"A": "1"}}"#,
        )
        .unwrap();
        // SAFETY: tests in this module don't read CLAUDE_CONFIG_DIR concurrently.
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", dir.path()) };
        assert!(!claude_bypass_accepted());
        accept_claude_bypass().unwrap();
        assert!(claude_bypass_accepted());
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["model"], "opus");
        assert_eq!(v["env"]["A"], "1");
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
    }
}
