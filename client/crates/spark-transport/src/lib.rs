//! spark-transport: GPUI-agnostic transport layer.
//!
//! Responsibilities:
//! - Connect to host-agent via HTTPS/WebSocket
//! - Authenticate
//! - Fetch snapshots (bots, tasks, workbenches)
//! - Subscribe to real-time events (SSE or WS)
//! - Send commands (create task, send message, approve, stop, etc.)
//! - Reconnect with backoff
//!
//! This crate does NOT depend on GPUI. Any client (CLI, mobile, web, tests)
//! can use it. The GPUI-specific stores in spark-ui consume TransportEvent
//! and update Entity<T> state.

pub mod api;
pub mod event;
pub mod transport;
pub mod reconnect;
pub mod runtime_transport;

pub use api::ApiClient;
pub use event::TransportEvent;
pub use transport::{Transport, TransportCommand};
pub use reconnect::ReconnectConfig;
pub use runtime_transport::{
    RuntimeTransport, LocalTransport, SshTransport,
    CreateRuntimeRequest, SandStatus, RuntimeInfo,
    FsRequest, FsResponse, BrowserRequest, BrowserResponse,
    ComputerRequest, ComputerResponse,
    PtyOpenRequest, PtyWriteRequest, PtyResizeRequest, PtySignalRequest,
    PtyReadResponse, PtyCursor, PtyReadRequest,
    RuntimeEvent, FrameHeader, FrameKind,
    BridgeConfig, BridgeHandshake, BRIDGE_PROTOCOL_VERSION,
    handshake_via_bridge, write_frame, read_frame,
};
