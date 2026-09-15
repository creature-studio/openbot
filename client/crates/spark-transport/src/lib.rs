//! spark-transport: GPUI-agnostic transport layer.
//!
//! Two layers live here, and they are deliberately separate:
//!
//! 1. **Client ↔ host-agent** ([`transport::Transport`], [`event::TransportEvent`]):
//!    the UI's command channel and event stream. The GPUI client never speaks
//!    SSH itself.
//! 2. **host-agent ↔ machine** ([`runtime_transport::RuntimeTransport`],
//!    [`LocalTransport`], [`SshTransport`]): one trait, two implementations, so
//!    the agent stack cannot tell a local runtime from a remote one.
//!
//! ```text
//! GPUI ──TransportCommand──▶ host-agent
//!                                 │  MachineManager.transport(machine_id)
//!                                 ▼
//!                        RuntimeTransport ──▶ LocalTransport (UDS)
//!                                        └──▶ SshTransport  (ssh + framed bridge)
//! ```
//!
//! This crate does NOT depend on GPUI. Any client (CLI, tests, mobile) can use
//! it.

pub mod api;
pub mod browser;
pub mod computer;
pub mod event;
pub mod events;
pub mod hash;
pub mod local;
pub mod reconnect;
pub mod runtime_transport;
pub mod ssh;
pub mod transport;
pub mod wire;

pub use api::ApiClient;
pub use event::{BootstrapSummary, MachineTestOutcome, TransportEvent};
pub use transport::{MachineDraft, Transport, TransportCommand};

// ---------------------------------------------------------------------------
// Machine transport surface — what host-agent and the tests import
// ---------------------------------------------------------------------------

pub use runtime_transport::{
    fs_request_binary, fs_request_json, parse_handshake_json, spark_json, BoxStream, BrowserAction,
    BrowserFrame, BrowserRequest, BrowserResponse, BrowserTab, CgroupConfig, ComputerAction,
    ComputerRequest, ComputerResponse, CreateRuntimeRequest, ExecRequest, ExecResult,
    FileTransferRequest, FsRequest, FsResponse, HandshakeResponse, PtyOpenRequest,
    PtyReadRequest, PtyReadResponse, PtyResizeRequest, PtySignalRequest, PtyWriteRequest, RawRpc,
    RpcPayload, RuntimeEvent, RuntimeInfo, RuntimeTransport, SandStatus, TransportStatus,
};

// Connection lifecycle: the pieces MachineManager needs to make retry decisions.
pub use local::{default_socket_dir, LocalTransport};
pub use reconnect::{describe_wait, plan, ReconnectConfig, ReconnectOutcome};
pub use ssh::bootstrap::{BootstrapReport, Platform};
pub use ssh::hostkey::{HostKeyIssue, Severity as HostKeySeverity};
pub use ssh::{ConnectError, SshTransport};

// Event delivery.
pub use events::{
    fetch_events, list_events_body, poll_events, poll_events_with, subscribe_sink, EventSink,
};

// Framing: defined once in sand-protocol and re-exported so callers do not have
// to depend on the sand workspace directly. `read_frame`/`write_frame` here are
// the async versions used by the SSH bridge.
pub use sand_protocol::frame::{
    FrameHeader, FrameKind, FramePayload, BRIDGE_PROTOCOL_VERSION, HEADER_LEN, MAX_PAYLOAD_LEN,
};
pub use wire::{decode_payload, encode_payload, read_frame, write_frame};

/// Compatibility alias: `PtyCursor` used to be a distinct type, but a PTY cursor
/// is just a byte offset, so it is a `u64` now.
pub type PtyCursor = u64;
