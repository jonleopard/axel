//! Spawning the adapter and wiring its stdio into the SDK as a line transport.
//!
//! The SDK's own `AcpAgent` cannot set the child's working directory and
//! gives no hook for raw frames, so the host spawns the adapter itself and
//! hands the SDK a [`Lines`] transport over the child's pipes.  Every line
//! that passes is appended to the [`FrameLog`].

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use agent_client_protocol::{Client, ConnectTo, Lines};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent_rpc::{LINE_LIMIT_BYTES, LineAccumulator, LineRead};
use crate::log::{Dir, FrameLog};

/// The longest stderr line kept from the adapter: the pane shows its first
/// 200 characters, so anything past a few pages is noise.
const STDERR_LINE_LIMIT_BYTES: usize = 64 * 1024;

/// Read one line of at most `limit_bytes` (see [`LineAccumulator`]).
pub(crate) async fn read_line_bounded<R: AsyncBufRead + Unpin>(reader: &mut R, limit_bytes: usize) -> io::Result<Option<LineRead>> {
    let mut line = LineAccumulator::new(limit_bytes);
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(line.finish());
        }
        let (consumed, complete) = line.feed(chunk);
        reader.consume(consumed);
        if complete {
            return Ok(line.finish());
        }
    }
}

/// Spawn `cmd` in `cwd` with extra `env`; return the child and a transport
/// speaking NDJSON over its stdin/stdout.  Stderr lines go to `stderr_tx`.
pub fn spawn(
    cmd: &[String],
    cwd: &Path,
    env: &[(String, String)],
    log: Arc<FrameLog>,
    stderr_tx: UnboundedSender<String>,
) -> io::Result<(Child, impl ConnectTo<Client>)> {
    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty adapter command"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group so killing the adapter also reaches wrappers (`npx …`).
    command.process_group(0);
    let mut child = command.spawn().map_err(|e| {
        io::Error::new(
            e.kind(),
            axel_i18n::t!("failed to spawn adapter `{program}`: {error}", program = program, error = e),
        )
    })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("no stderr"))?;

    forward_stderr(stderr, stderr_tx);
    let incoming = incoming_lines(stdout, log.clone());
    let outgoing = futures::sink::unfold(stdin, move |mut w, line: String| {
        let log = log.clone();
        async move {
            log.record(Dir::Out, &line);
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            w.write_all(&bytes).await?;
            w.flush().await?;
            Ok::<_, io::Error>(w)
        }
    });
    Ok((child, Lines::new(outgoing, incoming)))
}

/// The adapter's stderr, line by line, to `stderr_tx` (for the pane).
fn forward_stderr(stderr: tokio::process::ChildStderr, stderr_tx: UnboundedSender<String>) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        while let Ok(Some(read)) = read_line_bounded(&mut reader, STDERR_LINE_LIMIT_BYTES).await {
            // Stderr is for people: shown as well as it can be, never refused.
            let line = match read {
                LineRead::Line(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                LineRead::TooLong { length_bytes } => axel_i18n::t!("[a {length_bytes}-byte line, not shown]", length_bytes = length_bytes),
            };
            if stderr_tx.send(line).is_err() {
                break;
            }
        }
    });
}

/// The adapter's stdout as a stream of JSON lines, each one recorded in `log`.
fn incoming_lines(stdout: tokio::process::ChildStdout, log: Arc<FrameLog>) -> impl futures::Stream<Item = io::Result<String>> {
    futures::stream::unfold(BufReader::new(stdout), move |mut reader| {
        let log = log.clone();
        async move {
            loop {
                match read_line_bounded(&mut reader, LINE_LIMIT_BYTES).await {
                    Ok(Some(LineRead::Line(bytes))) => {
                        let line = String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
                        if let Ok(line) = &line {
                            log.record(Dir::In, line);
                        }
                        return Some((line, reader));
                    }
                    // Dropped, not passed on: a frame that size is no
                    // message the host could use. The log says it was there.
                    Ok(Some(LineRead::TooLong { length_bytes })) => {
                        log.record(Dir::In, &format!("[dropped a {length_bytes}-byte line: over the {LINE_LIMIT_BYTES}-byte limit]"));
                    }
                    Ok(None) => return None,
                    Err(e) => return Some((Err(e), reader)),
                }
            }
        }
    })
}
