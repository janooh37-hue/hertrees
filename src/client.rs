//! Talk to the running Herdr server.
//!
//! Requests use newline-delimited JSON over the socket at
//! `HERDR_SOCKET_PATH`, falling back to the `herdr` CLI at `HERDR_BIN_PATH`
//! when the socket is unavailable.

use crate::host;
use crate::opener::herdr_bin;
use crate::pyjson::{self, Map, Value};
use crate::pystr;
use std::fmt;
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
use std::time::Duration;

/// What kind of failure, as the Python release told them apart by class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// HerdrError: a request failed, or no server could be reached.
    Herdr,
    /// OSError: the local filesystem or process table said no.
    Os,
}

/// A Herdr request failed, or no server could be reached; or, with
/// `Kind::Os`, a local operation failed along the way.
#[derive(Clone, Debug, PartialEq)]
pub struct HerdrError {
    pub message: String,
    pub kind: Kind,
}

impl fmt::Display for HerdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl HerdrError {
    pub fn new(message: impl Into<String>) -> HerdrError {
        HerdrError { message: message.into(), kind: Kind::Herdr }
    }

    pub fn os(error: &io::Error) -> HerdrError {
        HerdrError { message: host::os_error_str(error), kind: Kind::Os }
    }
}

impl From<io::Error> for HerdrError {
    fn from(error: io::Error) -> HerdrError {
        HerdrError::os(&error)
    }
}

pub type Result<T> = std::result::Result<T, HerdrError>;

/// What went wrong below the protocol: Python's OSError and ValueError.
enum Transport {
    Os(io::Error),
    Value,
    Herdr(HerdrError),
}

impl Transport {
    fn message(&self) -> String {
        match self {
            Transport::Os(error) => host::os_error_str(error),
            Transport::Value => "invalid JSON in herdr's response".into(),
            Transport::Herdr(error) => error.message.clone(),
        }
    }
}

fn env_path(name: &str) -> Option<String> {
    std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()).filter(|v| !v.is_empty())
}

pub fn dict(entries: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(*key, value.clone());
    }
    Value::Dict(map)
}

#[derive(Clone)]
pub struct HerdrClient {
    pub socket_path: Option<String>,
    pub bin_path: String,
    pub timeout: f64,
    pub socket_only: bool,
    socket_ok: bool,
}

impl HerdrClient {
    pub fn new(socket_path: Option<String>, timeout: f64, socket_only: bool) -> HerdrClient {
        let socket_path = socket_path.or_else(|| env_path("HERDR_SOCKET_PATH"));
        let mut client = HerdrClient { socket_path, bin_path: herdr_bin(), timeout, socket_only, socket_ok: false };
        client.socket_ok = client.socket_usable();
        client
    }

    pub fn from_env() -> HerdrClient {
        HerdrClient::new(None, 4.0, false)
    }

    // -- transports ---------------------------------------------------------

    fn socket_usable(&self) -> bool {
        match &self.socket_path {
            // On Windows the path is the marker file next to the named pipe.
            Some(path) => std::fs::metadata(path).is_ok(),
            None => false,
        }
    }

    fn socket_call(&self, method: &str, params: &Value) -> std::result::Result<Value, Transport> {
        let line = self.socket_line(method, params)?;
        HerdrClient::parse_line(&line)
    }

    /// One request over the socket: the raw response line, unparsed.
    fn socket_line(&self, method: &str, params: &Value) -> std::result::Result<Vec<u8>, Transport> {
        let request = dict(&[("id", Value::str("bar")), ("method", Value::str(method)), ("params", params.clone())]);
        let mut payload = pyjson::dumps(&request, false).into_bytes();
        payload.push(b'\n');
        let path = self.socket_path.as_deref().unwrap_or("");
        let line = if host::WINDOWS {
            host::pipe_request(&host::pipe_name(path), &payload, self.timeout).map_err(Transport::Os)?
        } else {
            unix_request(path, &payload, self.timeout).map_err(Transport::Os)?
        };
        Ok(line)
    }

    fn parse_line(line: &[u8]) -> std::result::Result<Value, Transport> {
        if line.is_empty() {
            return Err(Transport::Herdr(HerdrError::new("herdr closed the connection without a response")));
        }
        let text = String::from_utf8_lossy(line);
        pyjson::loads(&text).map_err(|_| Transport::Value)
    }

    fn cli(&self, args: &[String]) -> Result<Value> {
        let mut argv = vec![self.bin_path.clone()];
        argv.extend(args.iter().cloned());
        let joined = args.join(" ");
        let output = match crate::proc::run(&argv, self.timeout + 4.0) {
            Ok(output) => output,
            Err(crate::proc::RunError::Spawn(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(HerdrError::new("herdr binary not found (set HERDR_BIN_PATH)"));
            }
            Err(crate::proc::RunError::Spawn(error)) => return Err(HerdrError::os(&error)),
            Err(crate::proc::RunError::Timeout) => return Err(HerdrError::new(format!("herdr {} timed out", joined))),
        };
        let out = String::from_utf8_lossy(&output.stdout);
        let out = pystr::strip(&out);
        if out.is_empty() {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = pystr::strip(&err);
            if err.is_empty() {
                return Err(HerdrError::new(format!("herdr {} produced no output", joined)));
            }
            return Err(HerdrError::new(err));
        }
        let lines = pystr::splitlines(out);
        let last = lines.last().copied().unwrap_or("");
        pyjson::loads(last).map_err(|_| HerdrError::new(format!("herdr {} returned unparseable output", joined)))
    }

    pub fn unwrap(response: Value) -> Result<Map> {
        let Value::Dict(mut response) = response else {
            return Err(HerdrError::new("herdr returned an unexpected response shape"));
        };
        if let Some(error) = response.get("error") {
            if error.truthy() {
                let message = match error {
                    Value::Dict(map) => map.get("message").cloned().unwrap_or(Value::Null),
                    other => Value::str(pyjson::py_str(other)),
                };
                if message.truthy() {
                    return Err(HerdrError::new(pyjson::py_str(&message)));
                }
                return Err(HerdrError::new("herdr request failed"));
            }
        }
        match response.remove("result") {
            Some(Value::Dict(result)) => Ok(result),
            _ => Err(HerdrError::new("herdr returned an unexpected response shape")),
        }
    }

    /// Run a request over the socket, falling back to the CLI.
    pub fn call(&mut self, method: &str, params: Value, cli_args: &[&str]) -> Result<Map> {
        if self.socket_only {
            if self.socket_path.is_none() {
                return Err(HerdrError::new("background naming requires HERDR_SOCKET_PATH"));
            }
            return match self.socket_call(method, &params) {
                Ok(response) => HerdrClient::unwrap(response),
                Err(error) => Err(HerdrError::new(error.message())),
            };
        }
        if self.socket_ok {
            match self.socket_call(method, &params) {
                Ok(response) => return HerdrClient::unwrap(response),
                Err(Transport::Herdr(error)) => return Err(error),
                // Transport-level failure only: the server may have restarted
                // or handed off. Stop trusting the socket and use the CLI.
                Err(_) => self.socket_ok = false,
            }
        }
        let args: Vec<String> = cli_args.iter().map(|s| s.to_string()).collect();
        HerdrClient::unwrap(self.cli(&args)?)
    }
}

#[cfg(unix)]
fn unix_request(path: &str, payload: &[u8], timeout: f64) -> io::Result<Vec<u8>> {
    use std::os::unix::net::UnixStream;
    let mut conn = UnixStream::connect(path)?;
    let limit = Some(Duration::from_secs_f64(timeout.max(0.001)));
    conn.set_read_timeout(limit)?;
    conn.set_write_timeout(limit)?;
    conn.write_all(payload)?;
    // Read straight into the answer, in reads that grow with it: a big
    // snapshot arrives in a handful of reads instead of one per 64 KiB, each
    // of which would also have woken the sender.
    let mut buffered: Vec<u8> = Vec::new();
    let mut scanned = 0;
    loop {
        if buffered[scanned..].contains(&b'\n') {
            break;
        }
        scanned = buffered.len();
        let room = scanned.clamp(65536, 4 << 20);
        buffered.resize(scanned + room, 0);
        let count = match conn.read(&mut buffered[scanned..]) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                buffered.truncate(scanned);
                continue;
            }
            Err(error) => return Err(error),
        };
        buffered.truncate(scanned + count);
        if count == 0 {
            break;
        }
    }
    let end = buffered.iter().position(|&b| b == b'\n').unwrap_or(buffered.len());
    buffered.truncate(end);
    Ok(buffered)
}

#[cfg(not(unix))]
fn unix_request(_path: &str, _payload: &[u8], _timeout: f64) -> io::Result<Vec<u8>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "no Unix sockets here"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testutil::{self, TempDir};
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    type Requests = Arc<Mutex<Vec<Value>>>;

    /// A stand-in for the Herdr socket: one request per connection. The
    /// responder's None means "never answer" (the connection stays open).
    fn serve(path: &std::path::Path, responder: fn(&Value) -> Option<Value>) -> Requests {
        let listener = UnixListener::bind(path).expect("bind fake herdr socket");
        let requests: Requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            let mut stalled = Vec::new();
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let mut line = String::new();
                if BufReader::new(&conn).read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(request) = pyjson::loads(&line) else { continue };
                seen.lock().unwrap().push(request.clone());
                match responder(&request) {
                    Some(reply) => {
                        let _ = conn.write_all(format!("{}\n", pyjson::dumps(&reply, false)).as_bytes());
                    }
                    None => stalled.push(conn),
                }
            }
        });
        requests
    }

    fn client(socket: &std::path::Path, bin: &str) -> HerdrClient {
        let mut client = HerdrClient::new(Some(socket.to_string_lossy().into_owned()), 4.0, false);
        client.bin_path = bin.to_string();
        client
    }

    fn params(text: &str) -> Value {
        pyjson::loads(text).unwrap()
    }

    #[test]
    fn server_errors_become_herdr_errors() {
        fn failing(request: &Value) -> Option<Value> {
            Some(dict(&[("id", request.get("id").clone()), ("error", params(r#"{"code": "not_found", "message": "no such tab"}"#))]))
        }
        let _env = testutil::env_lock();
        let path = testutil::socket_path();
        serve(&path, failing);
        let mut client = client(&path, "/nonexistent");
        let error = client.call("tab.focus", dict(&[("tab_id", Value::str("w1:t9"))]), &["tab", "focus", "w1:t9"]).unwrap_err();
        assert_eq!(error.kind, Kind::Herdr);
        assert!(error.message.contains("no such tab"), "{}", error);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_socket_falls_back_to_the_cli() {
        let _env = testutil::env_lock();
        let dir = TempDir::new();
        let mut client = client(&dir.path().join("gone.sock"), "/nonexistent");
        assert!(!client.socket_ok);
        assert_eq!(client.call("session.snapshot", dict(&[]), &["api", "snapshot"]).unwrap_err().kind, Kind::Herdr);
    }

    #[test]
    fn dead_socket_falls_back_to_the_cli() {
        // The marker is there but nothing answers: a server that went away.
        let _env = testutil::env_lock();
        let dir = TempDir::new();
        let path = dir.path().join("herdr.sock");
        std::fs::write(&path, "").unwrap();
        let mut client = client(&path, "/nonexistent");
        assert!(client.socket_ok);
        let error = client.call("session.snapshot", dict(&[]), &["api", "snapshot"]).unwrap_err();
        assert_eq!(error.kind, Kind::Herdr);
        assert!(error.message.contains("not found"), "{}", error);
        assert!(!client.socket_ok);
    }

    #[test]
    fn silent_server_times_out_instead_of_hanging() {
        let _env = testutil::env_lock();
        let path = testutil::socket_path();
        serve(&path, |_| None);
        let mut client = client(&path, "/nonexistent");
        client.timeout = 0.3;
        let started = Instant::now();
        assert_eq!(client.call("session.snapshot", dict(&[]), &["api", "snapshot"]).unwrap_err().kind, Kind::Herdr);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(!client.socket_ok);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn socket_only_client_reports_the_timeout() {
        let _env = testutil::env_lock();
        let path = testutil::socket_path();
        serve(&path, |_| None);
        let mut client = HerdrClient::new(Some(path.to_string_lossy().into_owned()), 0.3, true);
        let error = client.call("session.snapshot", dict(&[]), &["api", "snapshot"]).unwrap_err();
        assert_eq!(error.kind, Kind::Herdr);
        assert!(error.message.contains("timed out"), "{}", error);
        let _ = std::fs::remove_file(&path);
    }

    const SNAPSHOT_JSON: &str =
        r#"{"id":"cli","result":{"type":"session_snapshot","snapshot":{"tabs":[],"agents":[],"workspaces":[]}}}"#;

    /// A herdr stand-in that prints `output` whatever it is asked.
    fn fake_herdr(dir: &TempDir, output: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.path().join("fake-herdr");
        std::fs::write(&bin, format!("#!/bin/sh\necho '{}'\n", output)).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin.to_string_lossy().into_owned()
    }

    #[test]
    fn snapshot_over_the_cli() {
        let _env = testutil::env_lock();
        let dir = TempDir::new();
        let bin = fake_herdr(&dir, SNAPSHOT_JSON);
        let mut client = client(&dir.path().join("absent.sock"), &bin);
        let result = client.call("session.snapshot", dict(&[]), &["api", "snapshot"]).unwrap();
        assert_eq!(result.get("snapshot").unwrap().get("tabs"), &Value::List(Vec::new()));
    }

    #[test]
    fn missing_binary_reports_clearly() {
        let _env = testutil::env_lock();
        let dir = TempDir::new();
        let mut client = client(&dir.path().join("absent.sock"), "/nonexistent/herdr");
        let error = client.call("session.snapshot", dict(&[]), &["api", "snapshot"]).unwrap_err();
        assert_eq!(error.kind, Kind::Herdr);
        assert!(error.message.contains("not found"), "{}", error);
    }

    #[test]
    fn unparseable_output_reports_clearly() {
        let _env = testutil::env_lock();
        let dir = TempDir::new();
        let bin = fake_herdr(&dir, "not json");
        let mut client = client(&dir.path().join("absent.sock"), &bin);
        assert_eq!(client.call("session.snapshot", dict(&[]), &["api", "snapshot"]).unwrap_err().kind, Kind::Herdr);
    }
}
