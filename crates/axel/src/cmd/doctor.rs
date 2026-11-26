//! `axel doctor`

use anyhow::Result;
use axel_i18n::t;

use super::Ctx;
use super::text::pad;

pub fn run() -> Result<()> {
    println!("axel {}", env!("CARGO_PKG_VERSION"));
    // Each line is a label and a value; the labels are padded by terminal
    // columns, so they line up in Chinese too.
    let row = |label: &str, value: &str| println!("{} {value}", pad(label, 10));
    row(t!("db:"), &axel_store::Store::default_path().display().to_string());
    match axel_rex::detect() {
        Some(env) => {
            row("rex:", &t!("session {session}  block {block}", session = env.session, block = env.block));
            let rex = axel_rex::Rex::new(env);
            match rex.host_id() {
                Ok(h) => row(t!("rex host:"), &h.to_string()),
                Err(e) => row(t!("rex host:"), &t!("unavailable ({error})", error = e)),
            }
        }
        None => row("rex:", t!("not detected (REX_BLOCK unset or session unavailable); launcher runs inline")),
    }
    let ctx = Ctx::open()?;
    let owner = ctx.owner()?;
    row(t!("owner:"), &owner.public().to_string());
    let env = ctx.store.local_environment()?;
    row(t!("env:"), &format!("{} ({})", env.label, env.id));
    let agents = ctx.store.list_agents()?;
    row(t!("agents:"), &agents.len().to_string());
    for a in &agents {
        let key = ctx.keystore.agent(&a.pubkey.parse()?)?.is_some();
        let key = if key { t!("key: ok") } else { t!("key: MISSING") };
        println!("  {} {:<9} {key}", pad(&a.name, 16), a.harness);
    }
    row(t!("pending:"), &ctx.store.pending_requests()?.len().to_string());
    let open = ctx.store.list_threads(axel_store::ThreadFilter { states: Some(vec![axel_store::ThreadState::Open]), ..Default::default() })?.len();
    let live = ctx.store.statuses()?.len();
    row(t!("threads:"), &t!("{open} open, {live} live sessions", open = open, live = live));
    for h in ["claude", "codex", "opencode", "rex", "git"] {
        let found = which(h).map(|p| p.display().to_string());
        row(&format!("{h}:"), found.as_deref().unwrap_or(t!("not found")));
    }
    Ok(())
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}
