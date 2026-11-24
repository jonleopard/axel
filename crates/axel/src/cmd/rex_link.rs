//! A composer's own connection to Rex (`rex.control.v1`), for as long as the
//! thread is open.
//!
//! It offers the thread's actions to every other Rex client: Rex's command
//! palette runs them through `axel.lua` (`client.perform`), on the Mac or the
//! iPhone, and so does `rex -C axel:<window> do axel.stop`. It also follows
//! the session's events, so an agent's pane closing ends its session at once
//! rather than at the next reconcile. Everything runs on a thread of its own;
//! the composer never waits on it.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axel_i18n::t;
use axel_rex::control::{ActionSpec, ClientInfo, Control, Event, PerformRequest};
use axel_store::{SessionFilter, SessionRuntime, SessionState};
use serde_json::{Value, json};

use super::Ctx;
use axel_tui::SendCommand;

/// Reconnects in a row before the link gives up; the composer works on
/// without it. A connection that lived [`STABLE_AFTER`] resets the count.
const RECONNECTS_MAX: u32 = 20;
/// The longest wait between reconnects, doubling from a second.
const RECONNECT_WAIT_MAX: Duration = Duration::from_secs(30);
/// A connection that lasted this long was healthy; its drop starts afresh.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// How long one wait for an event lasts before the loop goes round.
const EVENT_WAIT: Duration = Duration::from_secs(1);

/// The prefix of a composer's client label; the window id follows, so the
/// palette finds the composer of the window you are looking at.
pub const LABEL_PREFIX: &str = "axel:";

/// The actions a composer performs, closed set; their wire names are below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadAction {
    Describe,
    Stop,
    Controls,
    AgentLog,
    Focus,
}

impl ThreadAction {
    const ALL: [ThreadAction; 5] = [ThreadAction::Describe, ThreadAction::Stop, ThreadAction::Controls, ThreadAction::AgentLog, ThreadAction::Focus];

    fn name(self) -> &'static str {
        match self {
            ThreadAction::Describe => "axel.describe",
            ThreadAction::Stop => "axel.stop",
            ThreadAction::Controls => "axel.controls",
            ThreadAction::AgentLog => "axel.agent_log",
            ThreadAction::Focus => "axel.focus",
        }
    }

    fn parse(name: &str) -> Option<ThreadAction> {
        ThreadAction::ALL.into_iter().find(|a| a.name() == name)
    }

    fn spec(self) -> ActionSpec {
        let (title, description) = match self {
            ThreadAction::Describe => (t!("Axel: Which thread"), t!("Name the thread this composer writes to.")),
            ThreadAction::Stop => (t!("Axel: Stop the agent's turn"), t!("Cancel the turn an agent is running on this thread.")),
            ThreadAction::Controls => (t!("Axel: Thread controls"), t!("Open the thread's sessions, spend and controls.")),
            ThreadAction::AgentLog => (t!("Axel: Agent log"), t!("Show or hide the agent's session pane.")),
            ThreadAction::Focus => (t!("Axel: Focus the composer"), t!("Put the cursor in this thread's composer.")),
        };
        ActionSpec { name: self.name(), title: title.to_string(), description: description.to_string() }
    }
}

/// Start the link for the composer of `thread_id`, in this Rex terminal.
pub fn spawn(thread_id: String) {
    let Some(env) = axel_rex::detect() else { return };
    std::thread::spawn(move || run(&thread_id, &env));
}

fn run(thread_id: &str, env: &axel_rex::RexEnv) {
    // Its own context: the composer's database handle stays on its thread.
    let Ok(ctx) = Ctx::open() else { return };
    let Some(rex) = ctx.rex.as_ref() else { return };
    let window = rex
        .api_call("session.describe_block", json!({ "block_id": env.block }))
        .ok()
        .and_then(|d| d.pointer("/block/window_id").and_then(Value::as_str).map(String::from))
        .unwrap_or_default();
    let info = ClientInfo { label: format!("{LABEL_PREFIX}{window}"), name: "axel".into(), version: env!("CARGO_PKG_VERSION").into() };
    let mut token: Option<String> = None;
    let mut failures = 0u32;
    while failures < RECONNECTS_MAX {
        let started = Instant::now();
        // An error ends this connection; the loop opens the next.
        let _ = serve(&ctx, thread_id, env, &info, &mut token);
        failures = if started.elapsed() >= STABLE_AFTER { 1 } else { failures + 1 };
        let wait = Duration::from_secs(1u64 << failures.min(5)).min(RECONNECT_WAIT_MAX);
        std::thread::sleep(wait);
    }
}

/// One connection's life: introduce, offer the actions, then follow events
/// until the connection fails.
fn serve(ctx: &Ctx, thread_id: &str, env: &axel_rex::RexEnv, info: &ClientInfo, token: &mut Option<String>) -> Result<()> {
    let mut control = Control::connect(env.server.as_deref(), info, token.as_deref())?;
    *token = control.reconnect_token().map(String::from);
    control.call("session.bootstrap", json!({ "event_scope": "session", "attach_session_ids": [env.session] }))?;
    let specs: Vec<ActionSpec> = ThreadAction::ALL.into_iter().map(ThreadAction::spec).collect();
    control.declare_actions(&specs)?;
    loop {
        match control.next_event(EVENT_WAIT)? {
            Some(Event::PerformRequested(request)) => {
                let outcome = perform(ctx, thread_id, env, &request).map_err(|e| format!("{e:#}"));
                control.reply(&request.call_id, outcome)?;
            }
            Some(Event::BlockClosed { block_id, .. }) if is_agent_pane(ctx, thread_id, &block_id) => {
                // The pane is gone, so is its harness: end its session now.
                super::reconcile::run(ctx)?;
            }
            Some(_) | None => {}
        }
    }
}

fn is_agent_pane(ctx: &Ctx, thread_id: &str, block_id: &str) -> bool {
    live_sessions(ctx, thread_id).is_ok_and(|sessions| sessions.iter().any(|s| s.rex_block_id.as_deref() == Some(block_id)))
}

/// The thread's live sessions, newest first.
fn live_sessions(ctx: &Ctx, thread_id: &str) -> Result<Vec<axel_store::Session>> {
    let states = vec![SessionState::Starting, SessionState::Running, SessionState::Blocked, SessionState::Idle];
    let mut sessions = ctx.store.list_sessions(SessionFilter { thread_id: Some(thread_id.into()), states: Some(states), ..Default::default() })?;
    sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
    Ok(sessions)
}

/// Carry out one action another client asked for.
fn perform(ctx: &Ctx, thread_id: &str, env: &axel_rex::RexEnv, request: &PerformRequest) -> Result<Value> {
    let rex = ctx.rex.as_ref().context(t!("not inside Rex"))?;
    let Some(action) = ThreadAction::parse(&request.action) else {
        bail!("{}", t!("Axel offers no action {action}", action = request.action));
    };
    match action {
        ThreadAction::Describe => {
            let thread = ctx.store.get_thread(thread_id)?.context(t!("the thread is gone"))?;
            Ok(json!({ "thread_id": thread.id, "title": thread.title }))
        }
        ThreadAction::Stop => {
            let working: Vec<_> = live_sessions(ctx, thread_id)?
                .into_iter()
                .filter(|s| s.state == SessionState::Running && s.runtime == SessionRuntime::Managed)
                .collect();
            if working.is_empty() {
                bail!("{}", t!("no agent is working on this thread"));
            }
            for session in &working {
                super::ui::send_to_session(ctx, &session.id, SendCommand::Cancel)?;
            }
            Ok(json!({ "stopped": working.len() }))
        }
        ThreadAction::Controls => {
            super::ui::open_controls(rex, thread_id);
            Ok(Value::Null)
        }
        ThreadAction::AgentLog => {
            let sessions = live_sessions(ctx, thread_id)?;
            let block = sessions.iter().find_map(|s| s.rex_block_id.clone()).context(t!("no agent on this thread has a pane"))?;
            super::ui::toggle_agent_pane(rex, &block, &env.block);
            Ok(Value::Null)
        }
        ThreadAction::Focus => {
            rex.api_call("session.focus_block", json!({ "block_id": env.block }))?;
            Ok(Value::Null)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_round_trips_by_name_and_has_a_dot() {
        for action in ThreadAction::ALL {
            assert_eq!(ThreadAction::parse(action.name()), Some(action));
            assert!(action.name().starts_with("axel."), "{}", action.name());
        }
        assert_eq!(ThreadAction::parse("axel.nope"), None);
    }
}
