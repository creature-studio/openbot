//! Runtime event delivery.
//!
//! Two sources, one type ([`RuntimeEvent`]):
//!
//! * **local machine** — poll `ListEvents` over the UDS socket (sandd keeps a
//!   1000 event ring buffer, so a short disconnect loses nothing);
//! * **remote machine** — the bridge pushes `Event` frames as they happen, and
//!   [`EventSink`] buffers them for whoever subscribed.
//!
//! Either way the caller sees the same stream, and reconnecting resumes from the
//! last seen `event_id` — an agent session catches up instead of restarting
//! (architecture §十六).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::stream::BoxStream;
use futures::StreamExt;

use crate::runtime_transport::{spark_json, RawRpc, RuntimeEvent};

/// How often a polling transport asks the machine for new events.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How long to wait after a failed poll before trying again.
const RETRY_INTERVAL: Duration = Duration::from_millis(1000);

// ---------------------------------------------------------------------------
// Push sink (used by the SSH bridge reader loop)
// ---------------------------------------------------------------------------

/// Thread-safe buffer of pushed events, cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct EventSink {
    inner: Arc<Mutex<SinkState>>,
}

#[derive(Debug, Default)]
struct SinkState {
    events: VecDeque<RuntimeEvent>,
    last_id: u64,
    closed: bool,
}

impl EventSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a pushed event. Bounded so a client that never polls cannot grow
    /// memory without limit; the oldest events fall off first.
    pub fn push(&mut self, event: RuntimeEvent) {
        if let Ok(mut state) = self.inner.lock() {
            state.events.push_back(event);
            while state.events.len() > 4096 {
                state.events.pop_front();
            }
        }
    }

    /// Put an event back at the front (used to requeue a batch tail).
    pub fn push_front(&mut self, event: RuntimeEvent) {
        if let Ok(mut state) = self.inner.lock() {
            state.events.push_front(event);
        }
    }

    pub fn close(&mut self) {
        if let Ok(mut state) = self.inner.lock() {
            state.closed = true;
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.lock().map(|s| s.closed).unwrap_or(true)
    }

    /// Remove and return buffered events, optionally limited to one runtime.
    pub fn drain(&mut self, runtime_id: Option<&str>) -> Vec<RuntimeEvent> {
        let mut out = Vec::new();
        if let Ok(mut state) = self.inner.lock() {
            let mut keep = VecDeque::with_capacity(state.events.len());
            while let Some(event) = state.events.pop_front() {
                match runtime_id {
                    Some(id) if event.runtime_id != id => keep.push_back(event),
                    _ => out.push(event),
                }
            }
            state.events = keep;
        }
        out
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|s| s.events.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Advance the high-water mark of event ids seen on this bridge.
    pub fn set_last_id(&mut self, id: u64) {
        if let Ok(mut state) = self.inner.lock() {
            state.last_id = state.last_id.max(id);
        }
    }

    pub fn last_id(&self) -> u64 {
        self.inner.lock().map(|s| s.last_id).unwrap_or(0)
    }
}

/// Stream over a push sink (SSH transport).
pub fn subscribe_sink(sink: &EventSink, runtime_id: Option<&str>) -> BoxStream<'static, RuntimeEvent> {
    let sink = sink.clone();
    let filter = runtime_id.map(|s| s.to_string());
    let (tx, rx) = futures::channel::mpsc::unbounded::<RuntimeEvent>();

    // A small pump task rather than a generator: the sink is a shared buffer,
    // not a channel, so somebody has to move events out of it.
    tokio::spawn(async move {
        loop {
            let mut sink = sink.clone();
            let pending = sink.drain(filter.as_deref());
            if pending.is_empty() {
                if sink.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            for event in pending {
                if tx.unbounded_send(event).is_err() {
                    return; // subscriber dropped
                }
            }
        }
    });

    Box::pin(rx)
}

// ---------------------------------------------------------------------------
// Polling (local machine, and reconnect catch-up on any machine)
// ---------------------------------------------------------------------------

/// Parse a `ListEvents` response into events plus the highest id seen.
pub fn parse_events(json: &str) -> (Vec<RuntimeEvent>, u64) {
    let last_id = spark_json::get_u64(json, "last_event_id").unwrap_or(0);
    let events = spark_json::get_object_array(json, "events")
        .iter()
        .map(|obj| RuntimeEvent {
            runtime_id: spark_json::get_str(obj, "runtime_id").unwrap_or_default(),
            kind: spark_json::get_str(obj, "kind").unwrap_or_default(),
            at_ms: spark_json::get_u64(obj, "at_ms").unwrap_or_else(sand_protocol::now_ms),
            machine_id: None,
        })
        .collect();
    (events, last_id)
}

/// Build the `ListEvents` request body.
pub fn list_events_body(runtime_id: Option<&str>, since: u64) -> String {
    match runtime_id {
        Some(id) => format!(
            "{{\"method\":\"ListEvents\",\"id\":\"{}\",\"since\":{since}}}",
            spark_json::escape(id)
        ),
        None => format!("{{\"method\":\"ListEvents\",\"since\":{since}}}"),
    }
}

/// Poll any async RPC source and yield events as they appear.
///
/// Each poll advances the cursor, so events are delivered exactly once even
/// though the underlying API is a ring buffer with no acknowledgement.
pub fn poll_events_with<F, Fut>(
    fetch: F,
    runtime_id: Option<String>,
    since: u64,
) -> BoxStream<'static, RuntimeEvent>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String>> + Send + 'static,
{
    let fetch = Arc::new(fetch);
    let stream = futures::stream::unfold(
        (fetch, runtime_id, since, VecDeque::<RuntimeEvent>::new()),
        |(fetch, runtime_id, cursor, mut queued)| async move {
            loop {
                if let Some(event) = queued.pop_front() {
                    return Some((event, (fetch, runtime_id, cursor, queued)));
                }

                let body = list_events_body(runtime_id.as_deref(), cursor);
                match fetch(body).await {
                    Ok(json) => {
                        let (events, last_id) = parse_events(&json);
                        let mut next_cursor = cursor.max(last_id);
                        if next_cursor == cursor && !events.is_empty() {
                            // Defensive: a machine that does not implement ids
                            // still must not replay forever.
                            next_cursor = cursor + events.len() as u64;
                        }
                        queued = events.into();
                        if queued.is_empty() {
                            tokio::time::sleep(POLL_INTERVAL).await;
                        }
                        return match queued.pop_front() {
                            Some(event) => {
                                Some((event, (fetch, runtime_id, next_cursor, queued)))
                            }
                            None => {
                                let _ = next_cursor;
                                let _ = &mut queued;
                                continue;
                            }
                        };
                    }
                    Err(_) => {
                        // Connection lost. Keep the cursor: when the machine
                        // comes back, the events emitted meanwhile are still in
                        // its ring buffer.
                        tokio::time::sleep(RETRY_INTERVAL).await;
                    }
                }
            }
        },
    );
    Box::pin(stream)
}

/// Convenience for a `RawRpc` transport that is cheap to share.
pub fn poll_events<T>(transport: Arc<T>, runtime_id: Option<String>, since: u64) -> BoxStream<'static, RuntimeEvent>
where
    T: RawRpc + 'static,
{
    poll_events_with(
        move |body| {
            let transport = transport.clone();
            async move { Ok(transport.rpc_raw(body, None).await?.json) }
        },
        runtime_id,
        since,
    )
}

/// Fetch events once — used when reconnecting to catch up.
pub async fn fetch_events<T: RawRpc + ?Sized>(
    transport: &T,
    runtime_id: Option<&str>,
    since: u64,
) -> Result<(Vec<RuntimeEvent>, u64)> {
    let response = transport
        .rpc_raw(list_events_body(runtime_id, since), None)
        .await?;
    Ok(parse_events(&response.json))
}

/// Drain a stream until it is quiet, returning everything collected.
pub async fn collect_now(
    stream: &mut BoxStream<'static, RuntimeEvent>,
    quiesce: Duration,
) -> Vec<RuntimeEvent> {
    let mut out = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(quiesce, stream.next()).await {
        out.push(event);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_transport::RpcPayload;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn parses_event_pages() {
        let json = "{\"ok\":true,\"last_event_id\":42,\"events\":[{\"event_id\":41,\"runtime_id\":\"rt-1\",\"kind\":\"exec.started\",\"at_ms\":1000},{\"event_id\":42,\"runtime_id\":\"rt-2\",\"kind\":\"pty.opened\",\"at_ms\":1001}]}";
        let (events, last_id) = parse_events(json);
        assert_eq!(last_id, 42);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].runtime_id, "rt-1");
        assert_eq!(events[1].kind, "pty.opened");
        assert_eq!(events[1].at_ms, 1001);
    }

    #[test]
    fn request_body_carries_the_cursor() {
        assert_eq!(
            list_events_body(Some("rt-1"), 9),
            "{\"method\":\"ListEvents\",\"id\":\"rt-1\",\"since\":9}"
        );
        assert_eq!(
            list_events_body(None, 0),
            "{\"method\":\"ListEvents\",\"since\":0}"
        );
    }

    #[test]
    fn sink_filters_and_bounds() {
        let mut sink = EventSink::new();
        sink.push(RuntimeEvent {
            runtime_id: "rt-1".into(),
            kind: "exec.started".into(),
            at_ms: 1,
            machine_id: None,
        });
        sink.push(RuntimeEvent {
            runtime_id: "rt-2".into(),
            kind: "pty.opened".into(),
            at_ms: 2,
            machine_id: None,
        });
        assert_eq!(sink.len(), 2);
        let only_rt1 = sink.drain(Some("rt-1"));
        assert_eq!(only_rt1.len(), 1);
        assert_eq!(only_rt1[0].kind, "exec.started");
        // The other runtime's event is still queued.
        assert_eq!(sink.len(), 1);
        assert_eq!(sink.drain(None).len(), 1);
    }

    #[test]
    fn sink_tracks_high_water_mark() {
        let mut sink = EventSink::new();
        sink.set_last_id(10);
        sink.set_last_id(4); // must not go backwards
        assert_eq!(sink.last_id(), 10);
        sink.set_last_id(11);
        assert_eq!(sink.last_id(), 11);
    }

    struct PagedTransport {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl RawRpc for PagedTransport {
        async fn rpc_raw(&self, method_json: String, _binary: Option<Vec<u8>>) -> Result<RpcPayload> {
            assert!(method_json.contains("ListEvents"));
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                Ok(RpcPayload::json(
                    "{\"ok\":true,\"last_event_id\":3,\"events\":[{\"event_id\":1,\"runtime_id\":\"rt-1\",\"kind\":\"exec.started\",\"at_ms\":1},{\"event_id\":2,\"runtime_id\":\"rt-1\",\"kind\":\"exec.output\",\"at_ms\":2},{\"event_id\":3,\"runtime_id\":\"rt-1\",\"kind\":\"exec.finished\",\"at_ms\":3}]}",
                ))
            } else {
                Ok(RpcPayload::json("{\"ok\":true,\"last_event_id\":3,\"events\":[]}"))
            }
        }
    }

    #[tokio::test]
    async fn poll_stream_delivers_every_event_exactly_once() {
        let transport = Arc::new(PagedTransport {
            calls: AtomicUsize::new(0),
        });
        let mut stream = poll_events(transport, Some("rt-1".into()), 0);
        let mut seen = Vec::new();
        for _ in 0..3 {
            let event = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .expect("timed out")
                .expect("stream ended");
            seen.push(event.kind);
        }
        assert_eq!(
            seen,
            vec!["exec.started", "exec.output", "exec.finished"]
        );
        // Nothing new: the cursor advanced past the whole page.
        let next = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
        assert!(next.is_err(), "must not re-emit events");
    }

    #[tokio::test]
    async fn fetch_events_advances_cursor() {
        let transport = PagedTransport {
            calls: AtomicUsize::new(0),
        };
        let (events, last_id) = fetch_events(&transport, Some("rt-1"), 0).await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(last_id, 3);
    }

    #[tokio::test]
    async fn sink_stream_forwards_pushed_events() {
        let mut sink = EventSink::new();
        sink.push(RuntimeEvent {
            runtime_id: "rt-1".into(),
            kind: "browser.frame".into(),
            at_ms: 5,
            machine_id: None,
        });
        let mut stream = subscribe_sink(&sink, None);
        let event = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("timed out")
            .expect("stream ended");
        assert_eq!(event.kind, "browser.frame");
        sink.close();
    }
}
