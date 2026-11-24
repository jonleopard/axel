//! A question an agent asks, put to the owner by Rex instead of a pane.
//!
//! Rex shows it in the command palette of every connected app (Mac, iPhone);
//! the first answer wins ([`axel_rex::prompt`]). This module turns a pending
//! question into a pick, waits, and sends the answer to the agent's host
//! exactly as the answer pane would. When Rex can't reach anyone, the caller
//! falls back to the pane.

use std::cell::Cell;
use std::time::{Duration, Instant};

use anyhow::Result;
use axel_i18n::t;
use axel_rex::prompt::{Answer, Item, Pick, Selection, Unanswered};

use crate::Deps;
use crate::glue;
use crate::model::{ChoiceOption, ItemBody, ItemKind, ItemStatus, ThreadItem};
use crate::thread::{SendCommand, SendReply, multi_answer_id};
use crate::util::now_secs;

/// How long Rex waits for the owner: its maximum. Until then the question
/// stays open on every device; the composer can still answer it meanwhile.
const TIMEOUT_SECONDS: u32 = axel_rex::prompt::TIMEOUT_SECONDS_MAX;
/// How many times a multi-select answer outside its bounds is asked again
/// before the question is left to the composer.
const BOUNDS_ATTEMPTS_MAX: u32 = 3;
/// While another prompt holds every app, how long to wait before asking again.
const BUSY_RETRY: Duration = Duration::from_secs(2);
/// How long to keep waiting behind other prompts: one Rex timeout.
const BUSY_WAIT_MAX: Duration = Duration::from_secs(TIMEOUT_SECONDS as u64);
/// How often the store is read to see whether the question was answered
/// elsewhere. Rex polls the stop check five times a second; a second is
/// quick enough for a person and keeps it to one read a second.
const PENDING_CHECK: Duration = Duration::from_secs(1);

/// Puts a pick to the owner and waits; the second argument says when to
/// stop waiting. In production [`axel_rex::Rex::ask`].
pub type AskFn<'a> = dyn Fn(&Pick, &dyn Fn() -> bool) -> Result<Answer> + 'a;

/// How putting a question through Rex ended.
#[derive(Debug, PartialEq, Eq)]
pub enum RexQuestion {
    /// The owner answered and the agent's host took it.
    Answered,
    /// The owner closed it, nobody answered in time, or it was answered
    /// elsewhere: the composer still has it.
    Left,
    /// Rex couldn't put it to anyone; show it another way.
    Unreachable(String),
}

/// Put question `item_id` of `thread_id` to the owner through `ask` (in
/// production [`axel_rex::Rex::ask`]) and send the answer to its session.
pub fn run_rex_question(
    deps: &Deps,
    thread_id: &str,
    item_id: &str,
    ask: &AskFn,
) -> Result<RexQuestion> {
    let owner_pk = glue::owner_pubkey(deps.keystore)?;
    let load = || -> Result<Option<(ThreadItem, String)>> {
        let data = glue::load_thread(deps.store, &owner_pk, thread_id, now_secs())?;
        let item = data.items.into_iter().find(|it| it.id == item_id && is_pending_question(it));
        Ok(item.map(|it| (it, data.title)))
    };
    let Some((item, thread_title)) = load()? else { return Ok(RexQuestion::Left) };
    let last_check = Cell::new(Instant::now());
    let is_answered_elsewhere = || {
        if last_check.get().elapsed() < PENDING_CHECK {
            return false;
        }
        last_check.set(Instant::now());
        // A read that fails keeps asking: the answer, if any, decides.
        matches!(load(), Ok(None))
    };
    let busy_since = Instant::now();
    let mut note: Option<String> = None;
    let mut bounds_attempts = 0u32;
    loop {
        let Some(pick) = question_pick(&item, &thread_title, note.as_deref()) else { return Ok(RexQuestion::Left) };
        let answer = match ask(&pick, &is_answered_elsewhere) {
            Ok(answer) => answer,
            Err(error) => return Ok(RexQuestion::Unreachable(format!("{error:#}"))),
        };
        match reply_for(&item, &answer) {
            Reply::Send(cmd) => return send(deps, &item, cmd),
            Reply::OutOfBounds(why) if bounds_attempts + 1 < BOUNDS_ATTEMPTS_MAX => {
                bounds_attempts += 1;
                note = Some(why);
            }
            Reply::OutOfBounds(_) => return Ok(RexQuestion::Left),
            Reply::Busy if busy_since.elapsed() < BUSY_WAIT_MAX => {
                std::thread::sleep(BUSY_RETRY);
                if load()?.is_none() {
                    return Ok(RexQuestion::Left);
                }
            }
            Reply::Busy | Reply::Leave => return Ok(RexQuestion::Left),
            Reply::Unreachable(why) => return Ok(RexQuestion::Unreachable(why)),
        }
    }
}

fn send(deps: &Deps, item: &ThreadItem, cmd: SendCommand) -> Result<RexQuestion> {
    match (deps.send)(&item.session_id, cmd)? {
        SendReply::Error { message } => Ok(RexQuestion::Unreachable(message)),
        _ => Ok(RexQuestion::Answered),
    }
}

fn is_pending_question(item: &ThreadItem) -> bool {
    item.kind == ItemKind::Question && item.status == Some(ItemStatus::Pending)
}

/// The pick for a pending question; `None` for anything else. `note` is
/// added to the title, as when an earlier answer chose too few or too many.
pub fn question_pick(item: &ThreadItem, thread_title: &str, note: Option<&str>) -> Option<Pick> {
    let ItemBody::Choice { prompt, options, multiple, .. } = &item.body else { return None };
    if options.is_empty() {
        return None;
    }
    let asker = item.agent.as_ref().map(|(name, _)| format!("@{name}")).unwrap_or_else(|| t!("an agent").to_string());
    let mut title = format!("{asker} · {prompt}");
    if let Some(note) = note {
        title = format!("{title} ({note})");
    }
    let items = options.iter().map(|o| Item { id: o.id.clone(), title: o.label.clone(), subtitle: None }).collect();
    let (selection, placeholder) = match multiple {
        Some((min, max)) => (Selection::Multiple, t!("Choose {min} to {max} · {thread}", min = min, max = max, thread = thread_title)),
        None => (Selection::Single { allow_other: true }, t!("Or type another answer · {thread}", thread = thread_title)),
    };
    Some(Pick { title, placeholder: Some(placeholder.to_string()), items, selection, timeout_seconds: TIMEOUT_SECONDS })
}

/// What to do with what Rex returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Send(SendCommand),
    /// A multi-select answer outside its bounds; ask again with this note.
    OutOfBounds(String),
    Busy,
    Leave,
    Unreachable(String),
}

/// The answer as the composer would send it: an option, a set of options,
/// or typed text that declines the question and redirects the agent.
pub fn reply_for(item: &ThreadItem, answer: &Answer) -> Reply {
    let ItemBody::Choice { options, multiple, .. } = &item.body else {
        unreachable!("only questions are asked through Rex");
    };
    let answer_with = |option_id: String| Reply::Send(SendCommand::Answer { item_id: item.id.clone(), option_id });
    match answer {
        Answer::Ids(ids) => match multiple {
            None => {
                assert_eq!(ids.len(), 1, "axel_rex checks a single pick has one answer");
                answer_with(ids[0].clone())
            }
            Some((min, max)) => {
                let chosen: Vec<&ChoiceOption> = options.iter().filter(|o| ids.contains(&o.id)).collect();
                // Declining is exclusive of other choices and wins over them, as in the pane.
                if let Some(decline) = chosen.iter().find(|o| o.exclusive) {
                    return answer_with(decline.id.clone());
                }
                if chosen.len() < *min || chosen.len() > *max {
                    return Reply::OutOfBounds(t!("choose {min} to {max}", min = min, max = max).to_string());
                }
                let ids: Vec<&str> = chosen.iter().map(|o| o.id.as_str()).collect();
                answer_with(multi_answer_id(&ids))
            }
        },
        Answer::Text(text) if text.trim().is_empty() => Reply::Leave,
        Answer::Text(text) => Reply::Send(SendCommand::Prompt { text: text.clone(), attachments: Vec::new(), steer: true }),
        Answer::Unanswered(Unanswered::Busy) => Reply::Busy,
        Answer::Unanswered(why) if why.is_unreachable() => Reply::Unreachable(format!("{why:?}")),
        Answer::Unanswered(_) => Reply::Leave,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(multiple: Option<(usize, usize)>) -> ThreadItem {
        let option = |id: &str, label: &str, exclusive: bool| ChoiceOption { id: id.into(), label: label.into(), exclusive };
        ThreadItem {
            id: "q1".into(),
            session_id: "s1".into(),
            agent: Some(("nova".into(), None)),
            author: None,
            seq: 1,
            turn: 1,
            at_ms: 0,
            kind: ItemKind::Question,
            status: Some(ItemStatus::Pending),
            body: ItemBody::Choice {
                prompt: "Which database?".into(),
                options: vec![option("answer:0", "Postgres", false), option("answer:1", "SQLite", false), option("answer:none", "None of these", true)],
                multiple,
                chosen: None,
            },
            attachments: Vec::new(),
            relay_to: None,
            steering: false,
        }
    }

    fn answer_id(reply: Reply) -> String {
        match reply {
            Reply::Send(SendCommand::Answer { item_id, option_id }) => {
                assert_eq!(item_id, "q1");
                option_id
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_single_question_is_a_pick_that_takes_typed_text() {
        let pick = question_pick(&question(None), "Storage", None).unwrap();
        assert_eq!(pick.title, "@nova · Which database?");
        assert_eq!(pick.selection, Selection::Single { allow_other: true });
        assert_eq!(pick.items.len(), 3);
        assert!(pick.placeholder.unwrap().contains("Storage"));
        let multiple = question_pick(&question(Some((1, 2))), "Storage", Some("choose 1 to 2")).unwrap();
        assert_eq!(multiple.selection, Selection::Multiple);
        assert!(multiple.title.ends_with("(choose 1 to 2)"), "{}", multiple.title);
    }

    #[test]
    fn answers_become_what_the_pane_would_send() {
        let single = question(None);
        assert_eq!(answer_id(reply_for(&single, &Answer::Ids(vec!["answer:1".into()]))), "answer:1");
        let typed = reply_for(&single, &Answer::Text("MySQL".into()));
        assert_eq!(typed, Reply::Send(SendCommand::Prompt { text: "MySQL".into(), attachments: vec![], steer: true }));
        assert_eq!(reply_for(&single, &Answer::Text("  ".into())), Reply::Leave);
        let multiple = question(Some((1, 2)));
        let both = Answer::Ids(vec!["answer:0".into(), "answer:1".into()]);
        assert_eq!(answer_id(reply_for(&multiple, &both)), r#"answer:multi:["answer:0","answer:1"]"#);
    }

    #[test]
    fn declining_wins_and_bounds_are_enforced() {
        let multiple = question(Some((1, 2)));
        let with_decline = Answer::Ids(vec!["answer:0".into(), "answer:none".into()]);
        assert_eq!(answer_id(reply_for(&multiple, &with_decline)), "answer:none");
        assert!(matches!(reply_for(&multiple, &Answer::Ids(vec![])), Reply::OutOfBounds(_)));
        let three = question(Some((1, 1)));
        let two = Answer::Ids(vec!["answer:0".into(), "answer:1".into()]);
        assert!(matches!(reply_for(&three, &two), Reply::OutOfBounds(_)));
    }

    /// A store holding one pending multi-select question, with its owner key.
    struct Fixture {
        store: axel_store::Store,
        keystore: axel_identity::Keystore,
        thread_id: String,
        item_id: String,
        directory: std::path::PathBuf,
    }

    fn fixture(name: &str) -> Fixture {
        use axel_store::{Harness, NewItem, NewSession, RuntimeMode, SessionRuntime};
        let directory = std::env::temp_dir().join(format!("axel-rex-question-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let keystore = axel_identity::Keystore::open_at(directory.join("keys"), false).unwrap();
        let owner = keystore.owner().unwrap();
        let store = axel_store::Store::open_memory().unwrap();
        let thread = store.create_thread(&owner, "Storage", "Storage", None).unwrap();
        let environment = store.local_environment().unwrap();
        let session = store
            .start_session(&owner, &NewSession {
                thread_id: thread.id.clone(), agent_pubkey: owner.public().to_string(),
                environment_id: environment.id.clone(), environment_label: environment.label.clone(),
                project_repo_key: None, harness: Harness::Claude, runtime_mode: RuntimeMode::default(),
                resumed_from: None, runtime: SessionRuntime::Managed,
            })
            .unwrap();
        let content = serde_json::json!({"title": "Which databases?", "multi_select": {"min": 1, "max": 2}, "options": [
            {"option_id": "answer:0", "name": "Postgres", "kind": "allow_once"},
            {"option_id": "answer:1", "name": "SQLite", "kind": "allow_once"},
            {"option_id": "answer:none", "name": "None of these", "kind": "reject_once"}]});
        let item = store
            .push_item(&NewItem {
                session_id: session.id.clone(), thread_id: thread.id.clone(), turn: 1,
                kind: axel_store::ItemKind::Question, status: Some(axel_store::ItemStatus::Pending),
                ref_id: None, content,
            })
            .unwrap();
        Fixture { store, keystore, thread_id: thread.id, item_id: item.id, directory }
    }

    /// Run the question with `ask`, recording what reaches the host.
    fn run(f: &Fixture, ask: &AskFn) -> (RexQuestion, Vec<SendCommand>) {
        let sent = std::cell::RefCell::new(Vec::new());
        let send = |_: &str, cmd: SendCommand| -> Result<SendReply> {
            sent.borrow_mut().push(cmd);
            Ok(SendReply::Ok)
        };
        let summon = |_: &str, _: &str, _: &str, _: &[std::path::PathBuf]| -> Result<String> { anyhow::bail!("not in tests") };
        let runtime_label = |_: &axel_store::Harness| String::new();
        let deps = Deps {
            store: &f.store, keystore: &f.keystore, cwd: f.directory.clone(), harnesses: vec![],
            send: &send, summon: &summon, runtime_label: &runtime_label, open_thread: None, focus_peer: None,
            fit_prompt: None, manage_thread: None, open_controls: None, open_terminal: None,
        };
        let outcome = run_rex_question(&deps, &f.thread_id, &f.item_id, ask).unwrap();
        (outcome, sent.into_inner())
    }

    #[test]
    fn a_stored_question_goes_through_rex_and_its_answer_to_the_host() {
        let f = fixture("answer");
        let asked = std::cell::Cell::new(0u32);
        let ask = |pick: &Pick, _: &dyn Fn() -> bool| -> Result<Answer> {
            asked.set(asked.get() + 1);
            assert_eq!(pick.selection, Selection::Multiple);
            // The first answer picks nothing, outside 1 to 2: asked again with the bounds.
            if asked.get() == 1 {
                return Ok(Answer::Ids(vec![]));
            }
            assert!(pick.title.contains("Which databases?") && pick.title.ends_with(')'), "{}", pick.title);
            Ok(Answer::Ids(vec!["answer:0".into(), "answer:1".into()]))
        };
        let (outcome, sent) = run(&f, &ask);
        assert_eq!(outcome, RexQuestion::Answered);
        assert_eq!(asked.get(), 2);
        let expected = SendCommand::Answer { item_id: f.item_id.clone(), option_id: r#"answer:multi:["answer:0","answer:1"]"#.into() };
        assert_eq!(sent, vec![expected]);
        std::fs::remove_dir_all(&f.directory).unwrap();
    }

    #[test]
    fn a_question_answered_elsewhere_withdraws_the_prompt() {
        let f = fixture("withdraw");
        let ask = |_: &Pick, should_stop: &dyn Fn() -> bool| -> Result<Answer> {
            f.store.update_item(&f.item_id, Some(axel_store::ItemStatus::Completed), None).unwrap();
            let deadline = Instant::now() + PENDING_CHECK * 3;
            while Instant::now() < deadline {
                if should_stop() {
                    return Ok(Answer::Unanswered(Unanswered::Withdrawn));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("the stop check never noticed the answer");
        };
        let (outcome, sent) = run(&f, &ask);
        assert_eq!(outcome, RexQuestion::Left);
        assert!(sent.is_empty());
        std::fs::remove_dir_all(&f.directory).unwrap();
    }

    #[test]
    fn rex_out_of_reach_hands_the_question_back() {
        let f = fixture("unreachable");
        let ask = |_: &Pick, _: &dyn Fn() -> bool| -> Result<Answer> { Ok(Answer::Unanswered(Unanswered::NotInstalled)) };
        let (outcome, sent) = run(&f, &ask);
        assert!(matches!(outcome, RexQuestion::Unreachable(_)), "{outcome:?}");
        assert!(sent.is_empty());
        std::fs::remove_dir_all(&f.directory).unwrap();
    }

    #[test]
    fn unanswered_questions_stay_or_fall_back() {
        let single = question(None);
        let unanswered = |u: Unanswered| reply_for(&single, &Answer::Unanswered(u));
        assert_eq!(unanswered(Unanswered::Cancelled), Reply::Leave);
        assert_eq!(unanswered(Unanswered::Timeout), Reply::Leave);
        assert_eq!(unanswered(Unanswered::Withdrawn), Reply::Leave);
        assert_eq!(unanswered(Unanswered::Busy), Reply::Busy);
        assert!(matches!(unanswered(Unanswered::NoClient), Reply::Unreachable(_)));
        assert!(matches!(unanswered(Unanswered::NotInstalled), Reply::Unreachable(_)));
    }
}
