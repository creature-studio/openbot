//! `sand bridge` — the SSH stdio ⇄ sandd UDS tunnel.
//!
//! ```text
//! host-agent ── framed binary ──▶ ssh stdin/stdout ──▶ sand bridge ──▶ sandd UDS
//! ```
//!
//! Responsibilities (deliberately minimal):
//!
//! * read frames from stdin, forward `Request` payloads to the sandd UDS,
//!   answer with `Response` frames carrying the same `request_id`;
//! * answer `Ping` frames itself with `Pong` **without touching sandd**, so the
//!   host-agent can measure latency of the whole link while sandd is busy;
//! * `StreamOpen`/`StreamData` are the one-way notification plane: the
//!   bridge's command thread pushes sandd events to stdout as unsolicited
//!   frames (with `request_id == 0`, since nobody asked for them);
//! * never listen on a network port, never spawn a shell, never write outside
//!   the socket path it is told to use.
//!
//! The UDS connection is opened once and reused for every request, so a single
//! SSH connection carries every RPC in the session.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use sand_protocol::frame::{
    read_frame, write_frame, FrameHeader, FrameKind, FramePayload,
};
use sand_protocol::status::current_machine_id;

/// Default socket path on a remote machine.
pub const DEFAULT_SOCKET: &str = "~/.cache/spark/run/sandd.sock";

/// sandd serves the framed (binary) protocol on a sibling socket. Requests are
/// relayed through it because exec/pty/screenshot can then travel as raw bytes;
/// every other method is delegated by sandd to the same JSON handlers.
pub fn binary_socket_for(socket: &std::path::Path) -> PathBuf {
    let name = socket
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "sandd.sock".to_string());
    let binary_name = name.replace("sandd.sock", "sandd-binary.sock");
    let binary_name = if binary_name == name && !name.contains("binary") {
        format!("{}.binary", name)
    } else {
        binary_name
    };
    socket.with_file_name(binary_name)
}

/// Outgoing frame plus its header, queued for the writer thread.
type Outgoing = (FrameHeader, Vec<u8>);

/// `sand bridge` runtime state shared between reader / writer / worker threads.
struct Bridge {
    /// sandd UDS connection, guarded because requests are serialized anyway
    /// (they share one socket).
    uds: Arc<Mutex<UnixStream>>,
    /// Queue of frames to write to stdout.
    out_tx: Sender<Outgoing>,
    /// Monotonic id for server-push streams.
    next_stream_id: AtomicU64,
    /// Set once stdin closes, so the event poller can exit and `run()` can join it.
    shutdown: std::sync::atomic::AtomicBool,
}

pub struct BridgeOptions {
    /// sandd control socket (`.../sandd.sock`).
    pub socket: PathBuf,
    /// Framed sandd socket. Defaults to the sibling `sandd-binary.sock`.
    pub binary_socket: Option<PathBuf>,
    /// Print connection details to stderr (useful when running the bridge by hand).
    pub verbose: bool,
}

impl BridgeOptions {
    /// Resolve the socket the RPC relay actually uses.
    pub fn effective_binary_socket(&self) -> PathBuf {
        self.binary_socket
            .clone()
            .unwrap_or_else(|| binary_socket_for(&self.socket))
    }
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            socket: expand_home(DEFAULT_SOCKET),
            binary_socket: None,
            verbose: false,
        }
    }
}

/// Expand a leading `~/` using `$HOME`.
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Run the bridge until stdin closes. Returns `Ok(())` on a clean shutdown.
pub fn run(options: BridgeOptions) -> io::Result<()> {
    let rpc_socket = options.effective_binary_socket();
    let uds = UnixStream::connect(&rpc_socket).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "cannot connect to sandd socket {} (control socket {}): {e}",
                rpc_socket.display(),
                options.socket.display()
            ),
        )
    })?;

    if options.verbose {
        let machine = current_machine_id().0;
        eprintln!(
            "[sand bridge] rpc {} | control {} | protocol v{} | machine {}",
            rpc_socket.display(),
            options.socket.display(),
            sand_protocol::frame::BRIDGE_PROTOCOL_VERSION,
            machine
        );
    }

    let (out_tx, out_rx) = mpsc::channel::<Outgoing>();
    let uds = Arc::new(Mutex::new(uds));

    let bridge = Arc::new(Bridge {
        uds,
        out_tx: out_tx.clone(),
        next_stream_id: AtomicU64::new(1),
        shutdown: std::sync::atomic::AtomicBool::new(false),
    });

    // Writer thread owns stdout: every frame goes through it, so no two
    // threads can interleave into a corrupt frame.
    let writer_handle = spawn_writer(std::io::stdout(), out_rx);

    // Event plane: forward sandd notifications as Event frames.
    let event_handle = spawn_event_poller(bridge.clone());

    // Main loop: read frames from stdin and serve them.
    let result = serve_stdin(bridge.clone());

    // stdin closed (host-agent went away or the SSH link dropped). The remote
    // sandd keeps running with all its runtimes; only this bridge exits.
    bridge.shutdown.store(true, Ordering::SeqCst);
    let _ = event_handle.join();
    drop(out_tx); // last sender → writer thread drains and exits
    let _ = writer_handle.join();
    result
}

fn spawn_writer<W: Write + Send + 'static>(mut sink: W, rx: Receiver<Outgoing>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while let Ok((header, payload)) = rx.recv() {
            if let Err(e) = write_frame(&mut sink, &header, &payload) {
                eprintln!("[sand bridge] stdout write failed: {e}");
                break;
            }
        }
    })
}

fn queue(&self_out: &Sender<Outgoing>, header: FrameHeader, payload: Vec<u8>) {
    let _ = self_out.send((header, payload));
}

/// Serve requests until stdin reaches EOF.
fn serve_stdin(bridge: Arc<Bridge>) -> io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();

    loop {
        let (header, payload) = match read_frame(&mut stdin) {
            Ok(frame) => frame,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };

        match header.kind {
            FrameKind::Ping => {
                // Answered locally: this measures the SSH link, not sandd.
                let mut pong = header;
                pong.kind = FrameKind::Pong;
                queue(&bridge.out_tx, pong, Vec::new());
            }
            FrameKind::Request => {
                match forward_to_sandd(&bridge, &payload) {
                    Ok(response_payload) => {
                        let mut reply = header;
                        reply.kind = FrameKind::Response;
                        reply.stream_id = header.stream_id;
                        queue(&bridge.out_tx, reply, response_payload);
                    }
                    Err(e) => {
                        let mut reply = header;
                        reply.kind = FrameKind::Response;
                        let body = FramePayload::json(format!(
                            "{{\"ok\":false,\"error\":\"bridge: {}\"}}",
                            sand_protocol::frame::json::escape(&e.to_string())
                        ));
                        queue(&bridge.out_tx, reply, body.encode());
                    }
                }
            }
            FrameKind::StreamOpen | FrameKind::StreamData | FrameKind::StreamClose => {
                // The stream plane is server-push only (sandd → host-agent), so
                // a client-initiated stream frame is acknowledged as an error
                // rather than silently ignored.
                let mut reply = header;
                reply.kind = FrameKind::Response;
                let body = FramePayload::json(
                    "{\"ok\":false,\"error\":\"bridge: stream frames are push-only\"}",
                );
                queue(&bridge.out_tx, reply, body.encode());
            }
            FrameKind::Response | FrameKind::Pong | FrameKind::Event => {
                // Responses/events only ever travel the other way; ignore.
            }
        }
    }
}

/// Relay one `Request` payload to sandd and return the `Response` payload.
///
/// Request and response share sandd's binary wire format
/// (`[u32 json_len][json][binary]`), so this is a faithful byte relay:
///
/// * the request's `binary` section is streamed straight through (PTY input);
/// * the response's binary section is sized from the length fields sandd puts
///   in its JSON (`len`, or `stdout_len` + `stderr_len`), then handed back;
///
/// Requests and responses are serialized on one connection, which is why the
/// socket is behind a mutex.
fn forward_to_sandd(bridge: &Bridge, payload: &[u8]) -> io::Result<Vec<u8>> {
    let request = FramePayload::decode(payload)?;
    if request.json.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request payload has an empty json body",
        ));
    }

    let mut uds = bridge
        .uds
        .lock()
        .map_err(|_| io::Error::other("sandd connection poisoned"))?;

    let json_bytes = request.json.as_bytes();
    uds.write_all(&(json_bytes.len() as u32).to_be_bytes())?;
    uds.write_all(json_bytes)?;
    if !request.binary.is_empty() {
        // sandd sizes the inbound binary section from `binary_len` in the JSON,
        // which the caller put there — see `FramePayload::with_binary` users.
        uds.write_all(&request.binary)?;
    }
    uds.flush()?;

    let mut len_buf = [0u8; 4];
    uds.read_exact(&mut len_buf)?;
    let json_len = u32::from_be_bytes(len_buf) as usize;
    if json_len > sand_protocol::frame::MAX_PAYLOAD_LEN as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("sandd json length too large: {json_len}"),
        ));
    }

    let mut json_buf = vec![0u8; json_len];
    uds.read_exact(&mut json_buf)?;
    let response_json = String::from_utf8_lossy(&json_buf).to_string();

    let binary_len = response_binary_len(&response_json);
    let mut binary = Vec::new();
    if binary_len > 0 {
        binary.resize(binary_len, 0);
        uds.read_exact(&mut binary)?;
    }

    Ok(FramePayload {
        json: response_json,
        binary,
    }
    .encode())
}

/// How many raw bytes follow the JSON of a sandd response.
///
/// sandd declares this with `len` (single binary blob: pty output, screenshot)
/// or `stdout_len` + `stderr_len` (exec). Anything else has no binary tail.
fn response_binary_len(json: &str) -> usize {
    let get = |field: &str| sand_protocol::frame::json::get_u64(json, field).unwrap_or(0) as usize;
    if !sand_protocol::frame::json::is_ok(json) {
        return 0;
    }
    if json.contains("\"stdout_len\"") || json.contains("\"stderr_len\"") {
        return get("stdout_len") + get("stderr_len");
    }
    if json.contains("\"len\"") {
        return get("len");
    }
    0
}

/// Poll sandd's event log and push new entries to the host-agent as `Event`
/// frames. Events are advisory: any failure just stops the poller without
/// affecting RPC.
fn spawn_event_poller(bridge: Arc<Bridge>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut last_seen: u64 = 0;
        loop {
            if bridge.shutdown.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(500));
            if bridge.shutdown.load(Ordering::SeqCst) {
                break;
            }
            let request = "{\"method\":\"ListEvents\"}";
            let payload = FramePayload::json(request).encode();
            let response = match forward_to_sandd(&bridge, &payload) {
                Ok(bytes) => bytes,
                Err(_) => break,
            };
            let decoded = match FramePayload::decode(&response) {
                Ok(decoded) => decoded,
                Err(_) => continue,
            };

            let events = match decoded.json.find("\"events\":[") {
                Some(start) => {
                    let rest = &decoded.json[start + "\"events\":[".len()..];
                    match rest.rfind(']') {
                        Some(end) => &rest[..end],
                        None => continue,
                    }
                }
                None => continue,
            };

            for raw in events.split("},{") {
                let at_ms = sand_protocol::frame::json::get_u64(raw, "at_ms").unwrap_or(0);
                if at_ms <= last_seen {
                    continue;
                }
                last_seen = at_ms;
                let stream_id = bridge.next_stream_id.fetch_add(1, Ordering::Relaxed);
                let mut header = FrameHeader::new(FrameKind::Event, 0, stream_id, 0);
                header.payload_len = 0;
                let body = FramePayload::json(raw.trim_matches([',', '{', '}']).to_string());
                queue(&bridge.out_tx, header, body.encode());
            }
        }
    })
}

/// Parse `--socket <path>` / `--socket=<path>` / `-v` from the sand CLI.
pub fn options_from_args<I: IntoIterator<Item = String>>(args: I) -> Result<BridgeOptions, String> {
    let mut options = BridgeOptions::default();
    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--socket" | "-s" => {
                let value = iter.next().ok_or("--socket requires a path")?;
                options.socket = expand_home(&value);
            }
            "-v" | "--verbose" => options.verbose = true,
            other => {
                if let Some(value) = other.strip_prefix("--socket=") {
                    options.socket = expand_home(value);
                } else if !other.starts_with('-') {
                    options.socket = expand_home(other);
                } else {
                    return Err(format!("unknown bridge option: {other}"));
                }
            }
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_socket_argument() {
        let opts = options_from_args(["--socket".to_string(), "/tmp/x.sock".to_string()]).unwrap();
        assert_eq!(opts.socket, PathBuf::from("/tmp/x.sock"));

        let opts = options_from_args(["--socket=/tmp/y.sock".to_string()]).unwrap();
        assert_eq!(opts.socket, PathBuf::from("/tmp/y.sock"));

        let opts = options_from_args(["/tmp/z.sock".to_string(), "-v".to_string()]).unwrap();
        assert_eq!(opts.socket, PathBuf::from("/tmp/z.sock"));
        assert!(opts.verbose);

        assert!(options_from_args(["--nope".to_string()]).is_err());
    }

    #[test]
    fn derives_binary_socket() {
        assert_eq!(
            binary_socket_for(std::path::Path::new("/home/dev/.cache/spark/run/sandd.sock")),
            PathBuf::from("/home/dev/.cache/spark/run/sandd-binary.sock")
        );
        let opts = BridgeOptions::default();
        assert!(opts.effective_binary_socket().ends_with("sandd-binary.sock"));
        let explicit = BridgeOptions {
            socket: PathBuf::from("/tmp/a.sock"),
            binary_socket: Some(PathBuf::from("/tmp/custom.sock")),
            verbose: false,
        };
        assert_eq!(
            explicit.effective_binary_socket(),
            PathBuf::from("/tmp/custom.sock")
        );
    }

    #[test]
    fn sizes_binary_tail_from_json() {
        assert_eq!(
            response_binary_len("{\"ok\":true,\"stdout_len\":10,\"stderr_len\":4,\"exit_code\":0}"),
            14
        );
        assert_eq!(
            response_binary_len("{\"ok\":true,\"len\":1234}"),
            1234
        );
        assert_eq!(response_binary_len("{\"ok\":true}"), 0);
        assert_eq!(response_binary_len("{\"ok\":false,\"error\":\"x\",\"len\":9}"), 0);
    }

    #[test]
    fn expands_home() {
        std::env::set_var("HOME", "/home/tester");
        assert_eq!(
            expand_home("~/x/y"),
            PathBuf::from("/home/tester/x/y")
        );
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
    }
}
