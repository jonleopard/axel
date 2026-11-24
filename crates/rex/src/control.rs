//! Rex's native control connection, `rex.control.v1`: one WebSocket on the
//! server's Unix socket that carries method calls both ways and streams
//! events, instead of a `rex` process per call.
//!
//! Axel uses it from long-lived processes (a thread's composer) to receive
//! live events, such as an agent's pane closing, and to offer actions that
//! other clients run with `client.perform`: Rex's command palette, a key, or
//! `rex -C LABEL do ACTION`, from any device. Synchronous on purpose: one
//! connection per process, read on its own thread, no async runtime.

use std::collections::VecDeque;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

use crate::Error;

/// The revision Axel is written against (Rex 0.1.0-preview.22).
pub const API_REVISION: u64 = 19;
/// The oldest revision Axel can use: 17 brought prompts and client actions.
pub const API_MIN_REVISION: u64 = 17;
/// Rex closes a connection whose hello passes 512 bytes (llms.txt).
const HELLO_BYTES_MAX: usize = 512;
/// Rex answers within this, or it is gone. Calls Axel makes are quick ones;
/// waiting on a person goes through `Rex::ask`, not here.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the welcome may take after the WebSocket opens.
const WELCOME_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one socket read blocks, so deadlines are checked between reads.
const READ_POLL: Duration = Duration::from_millis(250);
/// Events that arrive during a call wait here. Past this the oldest are
/// dropped: a composer reads them every second, so a backlog this deep means
/// it stopped reading.
const EVENTS_QUEUED_MAX: usize = 1_024;

/// What Rex shows about this client in `rex client list`.
#[derive(Clone, Debug)]
pub struct ClientInfo {
    /// Other clients find Axel's by this label.
    pub label: String,
    pub name: String,
    pub version: String,
}

/// An action this client performs for other clients (`client.perform`).
#[derive(Clone, Debug)]
pub struct ActionSpec {
    /// Must contain a dot: names without one belong to Rex's config file.
    pub name: &'static str,
    pub title: String,
    pub description: String,
}

/// An event Axel acts on; the rest arrive as [`Event::Other`].
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Another client asks this one to perform one of its actions; answer
    /// with [`Control::reply`].
    PerformRequested(PerformRequest),
    /// A block closed, and the program in it with it.
    BlockClosed { session_id: String, block_id: String },
    /// Any other event, by the name `rex.on` uses.
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PerformRequest {
    pub call_id: String,
    pub action: String,
    pub args: Value,
    pub session_id: Option<String>,
    pub block_id: Option<String>,
    /// Who asked, as the server identified them (not what they claim).
    pub requester: String,
}

/// One open control connection.
pub struct Control {
    socket: WebSocket<UnixStream>,
    request_id_next: u64,
    events: VecDeque<Value>,
    events_dropped: u64,
    reconnect_token: Option<String>,
}

impl Control {
    /// Connect to `server` (a `unix://` address, as `REX_SERVER` holds it;
    /// `None` for this account's default server), introduce this client,
    /// and agree on an API revision. `reconnect_token`, from an earlier
    /// connection, continues the same client.
    pub fn connect(server: Option<&str>, info: &ClientInfo, reconnect_token: Option<&str>) -> Result<Control, Error> {
        let path = socket_path(server)?;
        let stream = UnixStream::connect(&path).map_err(|e| connection(format!("connect {}: {e}", path.display())))?;
        let mut request = "ws://localhost/api/control".into_client_request().map_err(|e| connection(e.to_string()))?;
        let protocol = "rex.control.v1".parse().expect("a valid header value");
        request.headers_mut().insert("Sec-WebSocket-Protocol", protocol);
        let (socket, _) = tungstenite::client(request, stream).map_err(|e| connection(format!("handshake: {e}")))?;
        socket.get_ref().set_read_timeout(Some(READ_POLL)).map_err(|e| connection(e.to_string()))?;
        let mut control = Control { socket, request_id_next: 1, events: VecDeque::new(), events_dropped: 0, reconnect_token: None };
        control.hello(info, reconnect_token)?;
        Ok(control)
    }

    /// The token that continues this client after its connection drops.
    pub fn reconnect_token(&self) -> Option<&str> {
        self.reconnect_token.as_deref()
    }

    /// How many events were dropped because nobody read them in time.
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }

    fn hello(&mut self, info: &ClientInfo, reconnect_token: Option<&str>) -> Result<(), Error> {
        let mut hello = json!({
            "type": "hello",
            "api_revision": API_REVISION,
            "api_min_revision": API_MIN_REVISION,
            "client": {"kind": "agent", "name": info.name, "version": info.version, "label": info.label},
        });
        if let Some(token) = reconnect_token {
            hello["reconnect_token"] = json!(token);
        }
        let text = hello.to_string();
        if text.len() > HELLO_BYTES_MAX {
            return Err(Error::Invalid(format!("hello is {} bytes, over Rex's {HELLO_BYTES_MAX}", text.len())));
        }
        self.send(text)?;
        let deadline = Instant::now() + WELCOME_TIMEOUT;
        let welcome = self.read_until(deadline, |m| m["type"] == "welcome")?;
        if welcome["protocol"] != "rex.control.v1" {
            return Err(connection(format!("unexpected protocol {}", welcome["protocol"])));
        }
        let revision = welcome["api_revision"].as_u64().unwrap_or(0);
        if revision < API_MIN_REVISION {
            return Err(connection(format!("Rex speaks API revision {revision}; Axel needs {API_MIN_REVISION} or later")));
        }
        self.reconnect_token = welcome.pointer("/client/reconnect_token").and_then(Value::as_str).map(String::from);
        Ok(())
    }

    /// Call `method` and return its result. Events that arrive meanwhile
    /// are kept for [`Control::next_event`].
    pub fn call(&mut self, method: &str, payload: Value) -> Result<Value, Error> {
        assert!(payload.is_object(), "{method}: a payload is a JSON object");
        let id = self.request_id_next.to_string();
        self.request_id_next += 1;
        self.send(json!({"type": "request", "id": id, "method": method, "payload": payload}).to_string())?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        let response = self.read_until(deadline, |m| m["type"] == "response" && m["id"] == id.as_str())?;
        if let Some(error) = response.get("error").filter(|e| !e.is_null()) {
            let message = error["message"].as_str().unwrap_or("unknown error");
            let code = error["code"].as_str().unwrap_or("internal");
            return Err(Error::Rex { method: method.to_string(), stderr: format!("{code}: {message}") });
        }
        Ok(response.get("payload").cloned().unwrap_or(Value::Null))
    }

    /// Offer `actions` to other clients, replacing any offered before.
    pub fn declare_actions(&mut self, actions: &[ActionSpec]) -> Result<(), Error> {
        let declared: Vec<Value> = actions
            .iter()
            .map(|a| {
                assert!(a.name.contains('.'), "client action {} must contain a dot", a.name);
                json!({"name": a.name, "kind": "client", "title": a.title, "description": a.description, "category": "Axel"})
            })
            .collect();
        self.call("client.keymap", json!({"actions": declared, "server_actions": "none"})).map(|_| ())
    }

    /// Answer a [`PerformRequest`]: its result, or why it wasn't performed.
    pub fn reply(&mut self, call_id: &str, outcome: Result<Value, String>) -> Result<(), Error> {
        let payload = match outcome {
            Ok(result) => json!({"call_id": call_id, "result": result}),
            Err(message) => json!({"call_id": call_id, "error": {"code": "refused", "message": message}}),
        };
        self.call("client.reply", payload).map(|_| ())
    }

    /// The next event, waiting up to `wait` for one.
    pub fn next_event(&mut self, wait: Duration) -> Result<Option<Event>, Error> {
        if let Some(message) = self.events.pop_front() {
            return Ok(Some(parse_event(&message)));
        }
        let deadline = Instant::now() + wait;
        match self.read_until(deadline, |m| m["type"] == "event") {
            Ok(message) => Ok(Some(parse_event(&message))),
            Err(Error::Timeout) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Read until a message matches, queueing the events on the way.
    fn read_until(&mut self, deadline: Instant, matches: impl Fn(&Value) -> bool) -> Result<Value, Error> {
        while Instant::now() < deadline {
            let Some(message) = self.receive()? else { continue };
            if matches(&message) {
                return Ok(message);
            }
            if message["type"] == "event" {
                if self.events.len() == EVENTS_QUEUED_MAX {
                    self.events.pop_front();
                    self.events_dropped += 1;
                }
                self.events.push_back(message);
            }
            // Other types are skipped: a newer server may add some (llms.txt).
        }
        Err(Error::Timeout)
    }

    fn receive(&mut self) -> Result<Option<Value>, Error> {
        match self.socket.read() {
            Ok(Message::Text(text)) => serde_json::from_str(text.as_str())
                .map(Some)
                .map_err(|source| Error::Json { method: "control".into(), source }),
            Ok(Message::Close(frame)) => Err(connection(format!("closed by Rex: {frame:?}"))),
            // tungstenite answers pings itself; binary frames carry nothing here.
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(connection(format!("read: {e}"))),
        }
    }

    fn send(&mut self, text: String) -> Result<(), Error> {
        self.socket.send(Message::text(text)).map_err(|e| connection(format!("send: {e}")))
    }
}

fn connection(message: String) -> Error {
    Error::Connection(message)
}

/// An event message, `{"type": "event", "payload": {"type": NAME, "payload": FIELDS}}`.
pub fn parse_event(message: &Value) -> Event {
    let event = &message["payload"];
    let name = event["type"].as_str().unwrap_or("");
    let fields = &event["payload"];
    let text = |v: &Value| v.as_str().map(String::from);
    match name {
        "client_perform_requested" => match (text(&fields["call_id"]), text(&fields["action"])) {
            (Some(call_id), Some(action)) => Event::PerformRequested(PerformRequest {
                call_id,
                action,
                args: fields.get("args").cloned().unwrap_or(Value::Null),
                session_id: text(&fields["session_id"]),
                block_id: text(&fields["block_id"]),
                requester: text(&fields["requester"]["principal"]["display"]).unwrap_or_default(),
            }),
            _ => Event::Other(name.to_string()),
        },
        "session_event" => {
            let inner = &fields["event"];
            let inner_name = inner["type"].as_str().unwrap_or("");
            match (inner_name, text(&fields["session_id"]), text(&inner["payload"]["block_id"])) {
                ("block_closed", Some(session_id), Some(block_id)) => Event::BlockClosed { session_id, block_id },
                _ => Event::Other(inner_name.to_string()),
            }
        }
        other => Event::Other(other.to_string()),
    }
}

/// The socket a server address names. Only Unix sockets: Axel runs on the
/// machine its Rex server runs on.
fn socket_path(server: Option<&str>) -> Result<PathBuf, Error> {
    match server {
        Some(address) => {
            let path = address
                .strip_prefix("unix://")
                .ok_or_else(|| Error::Invalid(format!("the control connection needs a unix:// server, not {address}")))?;
            Ok(PathBuf::from(percent_decode(path)?))
        }
        None => {
            let home = std::env::var_os("HOME").ok_or_else(|| Error::Invalid("HOME is not set".into()))?;
            Ok(PathBuf::from(home).join("Library/Application Support/rex/server.sock"))
        }
    }
}

/// Undo a URL path's percent-encoding (`%20` for a space, as Rex writes it).
fn percent_decode(path: &str) -> Result<String, Error> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let hex = path.get(i + 1..i + 3).ok_or_else(|| Error::Invalid(format!("bad escape in {path}")))?;
        out.push(u8::from_str_radix(hex, 16).map_err(|_| Error::Invalid(format!("bad escape in {path}")))?);
        i += 3;
    }
    String::from_utf8(out).map_err(|_| Error::Invalid(format!("{path} is not UTF-8")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_decode_to_socket_paths() {
        let path = socket_path(Some("unix:///Users/me/Library/Application%20Support/rex/server.sock")).unwrap();
        assert_eq!(path, PathBuf::from("/Users/me/Library/Application Support/rex/server.sock"));
        assert!(socket_path(Some("https://rex.example.ts.net")).is_err());
        assert!(socket_path(Some("unix:///bad%2")).is_err());
        assert!(socket_path(Some("unix:///bad%zz")).is_err());
    }

    #[test]
    fn perform_requests_and_closed_blocks_parse() {
        let perform = json!({"type": "event", "payload": {"type": "client_perform_requested", "payload": {
            "call_id": "7", "action": "axel.stop", "session_id": "session:1",
            "requester": {"client_id": "client:2", "principal": {"display": "uid 501"}}}}});
        let Event::PerformRequested(request) = parse_event(&perform) else { panic!("not a perform request") };
        assert_eq!((request.call_id.as_str(), request.action.as_str(), request.requester.as_str()), ("7", "axel.stop", "uid 501"));
        assert_eq!(request.session_id.as_deref(), Some("session:1"));
        assert_eq!(request.args, Value::Null);
        let closed = json!({"type": "event", "payload": {"type": "session_event", "payload": {
            "session_id": "session:1", "event": {"type": "block_closed", "payload": {"block_id": "block:9"}}}}});
        assert_eq!(parse_event(&closed), Event::BlockClosed { session_id: "session:1".into(), block_id: "block:9".into() });
    }

    #[test]
    fn unknown_and_incomplete_events_are_other() {
        let joined = json!({"type": "event", "payload": {"type": "client_joined", "payload": {}}});
        assert_eq!(parse_event(&joined), Event::Other("client_joined".into()));
        let half = json!({"type": "event", "payload": {"type": "client_perform_requested", "payload": {"action": "x"}}});
        assert_eq!(parse_event(&half), Event::Other("client_perform_requested".into()));
        let bell = json!({"type": "event", "payload": {"type": "session_event", "payload": {
            "session_id": "session:1", "event": {"type": "block_event", "payload": {}}}}});
        assert_eq!(parse_event(&bell), Event::Other("block_event".into()));
    }
}
