//! Undo a turn: take its change back out of the working tree, keeping the
//! work done since.
//!
//! The reverse of the turn's `before..after` patch, limited to the files its
//! edit tools touched (every changed file when none is marked authored), is
//! applied to the working tree only; the index and HEAD are untouched.  A
//! snapshot of the tree is kept first under
//! `refs/axel/checkpoints/<thread>/undo/<n>`, so an undo can be undone.  When
//! a hunk no longer applies, because later work overlaps it, nothing is
//! written: the conflicting files are listed, and `restore` rewrites the
//! turn's files to their `before` content instead.  A hunk needs one line
//! of context on each side to apply (`-C1`), so edits near, but not on,
//! the turn's lines don't count as overlap.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axel_i18n::t;
use axel_acp::checkpoint::{self, REFS};
use axel_store::{ChangeStatus, FileChange, TurnChanges};

use super::Ctx;

/// What an undo did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undone {
    /// Files put back, repository-relative.
    pub files: Vec<String>,
    /// The snapshot of the tree as it was just before, for undoing the undo.
    pub snapshot_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoResult {
    Done(Undone),
    /// Later work overlaps these files; nothing was written.
    Conflicts(Vec<String>),
}

/// The files an undo touches: the turn's authored files, or all of them
/// when the turn marked none (a pane Codex session, or shell-only changes).
pub fn files_of(c: &TurnChanges) -> Vec<&FileChange> {
    let authored: Vec<&FileChange> = c.files.iter().filter(|f| f.authored).collect();
    if authored.is_empty() { c.files.iter().collect() } else { authored }
}

/// Why `session_id` can't be undone right now: its agent is mid-turn, so it
/// may be editing the same files.
pub fn blocked_reason(ctx: &Ctx, session_id: &str) -> Result<Option<String>> {
    use axel_store::{SessionState, StatusState};
    let Some(s) = ctx.store.get_session(session_id)? else { return Ok(None) };
    if s.ended_at.is_some() {
        return Ok(None);
    }
    let mid_turn = matches!(s.state, SessionState::Running | SessionState::Blocked | SessionState::Paused)
        || ctx.store.status(session_id)?.is_some_and(|st| matches!(st.state, StatusState::Working | StatusState::Blocked));
    Ok(mid_turn.then(|| t!("the agent is mid-turn; wait for it to finish").to_string()))
}

/// Reverse the turn's change in `dir`'s working tree (see the module doc).
pub fn undo(dir: &Path, thread_id: &str, c: &TurnChanges) -> Result<UndoResult> {
    let root = root_of(dir)?;
    let paths: Vec<String> = files_of(c).iter().flat_map(|f| [Some(f.path.clone()), f.old_path.clone()]).flatten().collect();
    if paths.is_empty() || c.before == c.after {
        return Ok(UndoResult::Done(Undone { files: vec![], snapshot_ref: String::new() }));
    }
    let patch = git_out(&root, &[&["diff", "--binary", "-M", "--no-ext-diff", &c.before, &c.after, "--"][..], &paths.iter().map(String::as_str).collect::<Vec<_>>()[..]].concat(), b"")?;
    if patch.is_empty() {
        return Ok(UndoResult::Done(Undone { files: vec![], snapshot_ref: String::new() }));
    }
    let check = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["apply", "-R", "--check", "--binary", "-C1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git apply --check")?;
    let out = feed(check, &patch)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut conflicts = conflicting_paths(&stderr);
        if conflicts.is_empty() {
            conflicts = paths.clone();
        }
        return Ok(UndoResult::Conflicts(conflicts));
    }
    let snapshot_ref = undo_snapshot(&root, thread_id)?;
    let apply = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["apply", "-R", "--binary", "-C1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git apply")?;
    let out = feed(apply, &patch)?;
    if !out.status.success() {
        bail!("git apply -R: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(UndoResult::Done(Undone { files: files_of(c).iter().map(|f| f.path.clone()).collect(), snapshot_ref }))
}

/// Rewrite the turn's files to their `before` content, whatever happened
/// since: a file the turn added goes; a renamed file goes back to its old
/// name.  A snapshot is kept first, as for [`undo`].
pub fn restore(dir: &Path, thread_id: &str, c: &TurnChanges) -> Result<Undone> {
    let root = root_of(dir)?;
    let snapshot_ref = undo_snapshot(&root, thread_id)?;
    let mut files = Vec::new();
    for f in files_of(c) {
        let full = root.join(&f.path);
        match f.status {
            ChangeStatus::Added => remove_if_present(&full)?,
            ChangeStatus::Renamed => {
                remove_if_present(&full)?;
                let old = f.old_path.clone().unwrap_or_else(|| f.path.clone());
                write_from(&root, &c.before, &old)?;
            }
            ChangeStatus::Modified | ChangeStatus::Deleted => write_from(&root, &c.before, &f.path)?,
        }
        files.push(f.path.clone());
    }
    Ok(Undone { files, snapshot_ref })
}

/// Remove a file the turn added; one already gone is what we wanted.
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// `path` as it is in `commit`, written into the working tree.
fn write_from(root: &Path, commit: &str, path: &str) -> Result<()> {
    let out = Command::new("git").arg("-C").arg(root).args(["show", &format!("{commit}:{path}")]).output().context("running git show")?;
    if !out.status.success() {
        bail!("git show {commit}:{path}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&full, &out.stdout).with_context(|| format!("writing {}", full.display()))?;
    Ok(())
}

/// Snapshot the tree under the thread's next undo ref.
fn undo_snapshot(root: &Path, thread_id: &str) -> Result<String> {
    let prefix = format!("{REFS}/{thread_id}/undo/");
    let existing = git_out(root, &["for-each-ref", "--format=%(refname)", &prefix], b"")?;
    let n = String::from_utf8_lossy(&existing).lines().filter_map(|l| l.strip_prefix(&prefix)?.parse::<u64>().ok()).max().unwrap_or(0) + 1;
    let name = format!("{prefix}{n}");
    checkpoint::capture(root, &name, Duration::from_secs(60), None)?.context("nothing to snapshot: not a git repository")?;
    Ok(name)
}

/// The paths `git apply --check` complained about.
pub fn conflicting_paths(stderr: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in stderr.lines() {
        let Some(rest) = line.strip_prefix("error: ") else { continue };
        let path = if let Some(r) = rest.strip_prefix("patch failed: ") {
            r.rsplit_once(':').map(|(p, _)| p).unwrap_or(r).to_string()
        } else if let Some((p, what)) = rest.split_once(": ")
            && (what.starts_with("patch does not apply") || what.contains("in working directory") || what.starts_with("No such file"))
        {
            p.to_string()
        } else {
            continue;
        };
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Tell the agent, so it doesn't build on code that's gone.  A note on the
/// thread, to the session's agent, signed by whoever runs this.
pub fn notify(ctx: &Ctx, thread_id: &str, session_id: &str, turn: i64, files: &[String], restored: bool) -> Result<()> {
    let session = ctx.store.get_session(session_id)?.context("no such session")?;
    let how = if restored { "restored to how they were before it" } else { "back to how they were before it" };
    let list = match files.len() {
        0 => return Ok(()),
        1 => files[0].clone(),
        2 => format!("{} and {}", files[0], files[1]),
        n => format!("{} and {} more", files[0], n - 1),
    };
    let text = format!("The owner undid your turn {turn}: {list} {how}. Don't redo that change unless asked.");
    // The message is tagged with the thread through the environment `axel send` reads.
    // SAFETY: single-threaded CLI; set before the send reads it.
    unsafe { std::env::set_var("AXEL_THREAD", thread_id) };
    super::inbox::send_message(ctx, std::slice::from_ref(&session.agent_pubkey), &[], &text, axel_mcp::MessageKind::Fyi)?;
    Ok(())
}

/// `axel thread undo THREAD [--session S] [--turn N] [--restore] [--yes]`.
pub fn run(thread: &str, session: Option<&str>, turn: Option<i64>, restore_files: bool, yes: bool) -> Result<()> {
    let ctx = Ctx::open()?;
    let t = super::thread::resolve(&ctx, thread)?;
    let p = super::diff::pick(&ctx, &t.id, session, turn)?;
    let ordinal = p.turn.summary.ordinal;
    if let Some(why) = blocked_reason(&ctx, &p.turn.session_id)? {
        bail!("{}", t!("can't undo turn {n} of {agent}: {why}", n = ordinal, agent = p.agent, why = why));
    }
    let files: Vec<String> = files_of(&p.changes).iter().map(|f| f.path.clone()).collect();
    if files.is_empty() {
        println!("{}", t!("turn {n} of {agent} changed no files", n = ordinal, agent = p.agent));
        return Ok(());
    }
    let dir = super::links::thread_dir(&ctx, &t.id).context(t!("this machine has no checkout of the thread's project"))?;
    if !yes {
        use std::io::Write;
        let (n, list) = (files.len(), files.join(", "));
        let question = match (restore_files, n) {
            (true, 1) => t!("restore the files of turn {turn} of {agent}: 1 file ({files})?", turn = ordinal, agent = p.agent, files = list),
            (true, _) => t!("restore the files of turn {turn} of {agent}: {n} files ({files})?", turn = ordinal, agent = p.agent, n = n, files = list),
            (false, 1) => t!("undo turn {turn} of {agent}: 1 file ({files})?", turn = ordinal, agent = p.agent, files = list),
            (false, _) => t!("undo turn {turn} of {agent}: {n} files ({files})?", turn = ordinal, agent = p.agent, n = n, files = list),
        };
        // The answer stays y or yes, as the [y/N] hint says, in every language.
        print!("{question} [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("{}", t!("left as is"));
            return Ok(());
        }
    }
    let (done, restored) = if restore_files {
        (restore(&dir, &t.id, &p.changes)?, true)
    } else {
        match undo(&dir, &t.id, &p.changes)? {
            UndoResult::Done(d) => (d, false),
            UndoResult::Conflicts(files) => {
                bail!(
                    "{}",
                    t!(
                        "later work overlaps {files}: nothing changed. `axel thread undo {thread} --turn {n} --restore` rewrites the turn's files to how they were before it",
                        files = files.join(", "),
                        thread = &t.id[..12.min(t.id.len())],
                        n = ordinal
                    )
                );
            }
        }
    };
    let (files, snapshot) = (done.files.join(", "), &done.snapshot_ref);
    let line = if restored {
        t!("restored: {files} (snapshot of the tree before: {snapshot})", files = files, snapshot = snapshot)
    } else {
        t!("undone: {files} (snapshot of the tree before: {snapshot})", files = files, snapshot = snapshot)
    };
    println!("{line}");
    if let Err(e) = notify(&ctx, &t.id, &p.turn.session_id, ordinal, &done.files, restored) {
        eprintln!("axel: {}", t!("telling the agent: {error}", error = format!("{e:#}")));
    }
    Ok(())
}

fn root_of(dir: &Path) -> Result<PathBuf> {
    let out = git_out(dir, &["rev-parse", "--show-toplevel"], b"")?;
    Ok(PathBuf::from(String::from_utf8_lossy(&out).trim()))
}

fn git_out(dir: &Path, args: &[&str], stdin: &[u8]) -> Result<Vec<u8>> {
    let child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(if stdin.is_empty() { Stdio::null() } else { Stdio::piped() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git")?;
    let out = feed(child, stdin)?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// How long one git command may take.
const GIT_TIMEOUT_SECS: u64 = 60;
/// The most output one git command may give: a turn's binary patch, which
/// a turn that rewrote large files makes big, but never this big.
const GIT_OUTPUT_BYTES_MAX: u64 = 256 * 1024 * 1024;
/// How often a running git is checked for having finished.
const GIT_POLL_MS: u64 = 10;

/// Write `input` to the child's stdin and collect its output, within
/// [`GIT_TIMEOUT_SECS`].  Its output is read as it comes, so a command that
/// writes more than a pipe holds doesn't stall until the deadline.
fn feed(mut child: std::process::Child, input: &[u8]) -> Result<std::process::Output> {
    if let Some(mut stdin) = child.stdin.take() {
        let data = input.to_vec();
        std::thread::spawn(move || {
            use std::io::Write;
            // A git that exits without reading all of it says why in its status.
            let _ = stdin.write_all(&data);
        });
    }
    let stdout = child.stdout.take().map(|out| std::thread::spawn(move || read_bounded(out)));
    let stderr = child.stderr.take().map(|err| std::thread::spawn(move || read_bounded(err)));
    let deadline = Instant::now() + Duration::from_secs(GIT_TIMEOUT_SECS);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            // Already failing: a kill that doesn't land leaves a git that ends on its own.
            let _ = child.kill();
            let _ = child.wait();
            bail!("git took longer than {GIT_TIMEOUT_SECS} seconds");
        }
        std::thread::sleep(Duration::from_millis(GIT_POLL_MS));
    };
    let collect = |reader: Option<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>| -> Result<Vec<u8>> {
        match reader {
            Some(r) => r.join().map_err(|_| anyhow::anyhow!("reading git's output panicked"))?.map_err(Into::into),
            None => Ok(Vec::new()),
        }
    };
    let stdout = collect(stdout)?;
    if u64::try_from(stdout.len()).unwrap_or(u64::MAX) > GIT_OUTPUT_BYTES_MAX {
        bail!("git gave more than {GIT_OUTPUT_BYTES_MAX} bytes of output");
    }
    Ok(std::process::Output { status, stdout, stderr: collect(stderr)? })
}

/// Read a pipe to its end, keeping at most one byte past
/// [`GIT_OUTPUT_BYTES_MAX`] (so the caller can tell it was cut) and draining
/// the rest, so the writer never blocks on it.
fn read_bounded(mut pipe: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    (&mut pipe).take(GIT_OUTPUT_BYTES_MAX + 1).read_to_end(&mut buf)?;
    std::io::copy(&mut pipe, &mut std::io::sink())?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn git(dir: &Path, args: &[&str]) -> String {
        String::from_utf8_lossy(&git_out(dir, args, b"").unwrap()).trim().to_string()
    }

    #[test]
    fn a_command_with_more_output_than_a_pipe_holds_finishes_at_once() {
        let child = Command::new("/bin/sh")
            .args(["-c", "head -c 1000000 /dev/zero"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let out = feed(child, b"").unwrap();
        assert_eq!(out.stdout.len(), 1_000_000);
        assert!(started.elapsed() < Duration::from_secs(10), "read as it comes, not at the deadline");
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git(d.path(), &args);
        }
        std::fs::write(d.path().join("a.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        std::fs::write(d.path().join("c.txt"), "keep me\n").unwrap();
        std::fs::write(d.path().join("other.rs"), "x\n").unwrap();
        git(d.path(), &["add", "."]);
        git(d.path(), &["commit", "--quiet", "-m", "base"]);
        d
    }

    /// A turn that edits a.rs, adds b.rs and deletes c.txt, as its checkpoints record it.
    fn turn(d: &Path) -> TurnChanges {
        let far = Instant::now() + Duration::from_secs(60);
        let before = checkpoint::take(d, far).unwrap().unwrap();
        std::fs::write(d.join("a.rs"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
        std::fs::write(d.join("b.rs"), "new\n").unwrap();
        std::fs::remove_file(d.join("c.txt")).unwrap();
        let after = checkpoint::take(d, far).unwrap().unwrap();
        let mut c = checkpoint::changes(d, &before, &after, &[d.join("a.rs"), d.join("b.rs"), d.join("c.txt")]).unwrap();
        c.files.iter_mut().for_each(|f| f.authored = true);
        c
    }

    #[test]
    fn undo_reverses_the_turns_files_and_keeps_later_work() {
        let d = repo();
        let c = turn(d.path());
        // Later work elsewhere, and a non-overlapping edit in a.rs.
        std::fs::write(d.path().join("other.rs"), "y\n").unwrap();
        std::fs::write(d.path().join("a.rs"), "one\nTWO\nthree\nfour\nFIVE\n").unwrap();
        let UndoResult::Done(done) = undo(d.path(), "T1", &c).unwrap() else { panic!("applies cleanly") };
        assert_eq!(done.files, vec!["a.rs", "b.rs", "c.txt"]);
        assert_eq!(done.snapshot_ref, "refs/axel/checkpoints/T1/undo/1");
        assert_eq!(std::fs::read_to_string(d.path().join("a.rs")).unwrap(), "one\ntwo\nthree\nfour\nFIVE\n", "the turn's hunk is gone, the later one stays");
        assert!(!d.path().join("b.rs").exists());
        assert_eq!(std::fs::read_to_string(d.path().join("c.txt")).unwrap(), "keep me\n");
        assert_eq!(std::fs::read_to_string(d.path().join("other.rs")).unwrap(), "y\n");
        assert!(git(d.path(), &["diff", "--cached", "--name-only"]).is_empty(), "index untouched");
        // The snapshot holds the tree as it was before the undo.
        let snap = git(d.path(), &["show", "refs/axel/checkpoints/T1/undo/1:b.rs"]);
        assert_eq!(snap, "new");
        // Undoing again: the second undo gets the next number.
        let c2 = turn(d.path());
        let UndoResult::Done(done2) = undo(d.path(), "T1", &c2).unwrap() else { panic!() };
        assert_eq!(done2.snapshot_ref, "refs/axel/checkpoints/T1/undo/2");
    }

    #[test]
    fn overlapping_work_stops_undo_and_restore_takes_over() {
        let d = repo();
        let c = turn(d.path());
        std::fs::write(d.path().join("a.rs"), "one\nTWO!\nthree\nfour\nfive\n").unwrap();
        let UndoResult::Conflicts(files) = undo(d.path(), "T1", &c).unwrap() else { panic!("the edited hunk conflicts") };
        assert_eq!(files, vec!["a.rs"]);
        assert!(d.path().join("b.rs").exists(), "nothing written");
        assert!(git(d.path(), &["for-each-ref", "refs/axel/checkpoints/T1/undo"]).is_empty(), "no snapshot for a refused undo");
        let done = restore(d.path(), "T1", &c).unwrap();
        assert_eq!(done.snapshot_ref, "refs/axel/checkpoints/T1/undo/1");
        assert_eq!(std::fs::read_to_string(d.path().join("a.rs")).unwrap(), "one\ntwo\nthree\nfour\nfive\n");
        assert!(!d.path().join("b.rs").exists());
        assert_eq!(std::fs::read_to_string(d.path().join("c.txt")).unwrap(), "keep me\n");
    }

    #[test]
    fn only_authored_files_are_touched_unless_none_is() {
        let mut c = TurnChanges::default();
        c.files.push(FileChange { path: "a".into(), authored: true, ..Default::default() });
        c.files.push(FileChange { path: "b".into(), authored: false, ..Default::default() });
        assert_eq!(files_of(&c).iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["a"]);
        c.files[0].authored = false;
        assert_eq!(files_of(&c).len(), 2);
        assert_eq!(
            conflicting_paths("error: patch failed: src/a.rs:12\nerror: src/a.rs: patch does not apply\nerror: b.rs: already exists in working directory\n"),
            vec!["src/a.rs", "b.rs"]
        );
    }
}
