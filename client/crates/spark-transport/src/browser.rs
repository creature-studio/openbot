//! Remote browser helpers, shared by every transport.
//!
//! There is exactly one implementation of "open a URL / snapshot / click / fill
//! / screenshot": it speaks the sandd browser methods through [`RawRpc`]. A
//! runtime on the local machine and a runtime on an SSH machine therefore take
//! the identical code path — the machine boundary is the transport's problem,
//! never the browser panel's (architecture §十八/§二十).
//!
//! Screenshots arrive as **raw image bytes** on the framed socket, never as
//! base64, and carry a `frame_id` so the UI can drop stale frames instead of
//! queueing them (no VNC, §二十一).

use anyhow::{bail, Result};

use crate::runtime_transport::{
    spark_json, BrowserAction, BrowserFrame, BrowserRequest, BrowserResponse, BrowserTab, RawRpc,
};

/// Run one browser action on the runtime's machine.
pub async fn dispatch<T: RawRpc + ?Sized>(transport: &T, request: BrowserRequest) -> Result<BrowserResponse> {
    let (method, extra) = match &request.action {
        BrowserAction::Open { url } => ("BrowserOpen", format!(",\"url\":\"{}\"", spark_json::escape(url))),
        BrowserAction::Snapshot => ("BrowserSnapshot", String::new()),
        BrowserAction::Click { reference } => (
            "BrowserClick",
            format!(",\"ref\":\"{}\"", spark_json::escape(reference)),
        ),
        BrowserAction::Fill { reference, text } => (
            "BrowserFill",
            format!(
                ",\"ref\":\"{}\",\"text\":\"{}\"",
                spark_json::escape(reference),
                spark_json::escape(text)
            ),
        ),
        BrowserAction::Press { key } => ("BrowserPress", format!(",\"key\":\"{}\"", spark_json::escape(key))),
        BrowserAction::Screenshot { format, quality } => (
            "BrowserScreenshot",
            format!(",\"format\":\"{}\",\"quality\":{}", spark_json::escape(format), quality),
        ),
        BrowserAction::Tabs => ("BrowserTabs", String::new()),
        BrowserAction::Close => ("BrowserClose", String::new()),
    };

    let body = format!(
        "{{\"method\":\"{method}\",\"id\":\"{}\"{extra}}}",
        spark_json::escape(&request.runtime_id)
    );

    let response = transport.rpc_raw(body, None).await?;

    // Errors are reported as data, not as Err: the agent needs to see "the page
    // has no element @e12" as a tool result, not as a transport failure.
    if !spark_json::is_ok(&response.json) {
        let message = spark_json::error_of(&response.json).unwrap_or_else(|| response.json.clone());
        if spark_json::get_bool(&response.json, "unsupported").unwrap_or(false) {
            return Ok(BrowserResponse {
                error: Some(format!(
                    "{message} (the machine does not advertise the browser capability)"
                )),
                ..Default::default()
            });
        }
        return Ok(BrowserResponse {
            error: Some(message),
            ..Default::default()
        });
    }

    match request.action {
        BrowserAction::Screenshot { format, .. } => Ok(BrowserResponse {
            frame: Some(frame_from(&response.json, response.binary, &format)),
            ..Default::default()
        }),
        BrowserAction::Snapshot => Ok(BrowserResponse {
            snapshot: Some(
                spark_json::get_str(&response.json, "snapshot")
                    .or_else(|| spark_json::raw_field(&response.json, "data"))
                    .unwrap_or_default(),
            ),
            ..Default::default()
        }),
        BrowserAction::Tabs => Ok(BrowserResponse {
            tabs: Some(parse_tabs(&response.json)),
            ..Default::default()
        }),
        BrowserAction::Open { url } => {
            let opened = spark_json::get_str(&response.json, "url").unwrap_or(url);
            Ok(BrowserResponse {
                snapshot: Some(opened),
                ..Default::default()
            })
        }
        _ => Ok(BrowserResponse {
            ..Default::default()
        }),
    }
}

/// Build a frame record from a screenshot response.
pub fn frame_from(json: &str, binary: Vec<u8>, fallback_format: &str) -> BrowserFrame {
    BrowserFrame {
        frame_id: spark_json::get_u64(json, "frame_id").unwrap_or_else(sand_protocol::now_ms),
        width: spark_json::get_u64(json, "width").unwrap_or(1280) as u32,
        height: spark_json::get_u64(json, "height").unwrap_or(800) as u32,
        timestamp_ms: spark_json::get_u64(json, "at_ms").unwrap_or_else(sand_protocol::now_ms),
        data: binary,
        format: spark_json::get_str(json, "format").unwrap_or_else(|| fallback_format.to_string()),
    }
}

fn parse_tabs(json: &str) -> Vec<BrowserTab> {
    spark_json::get_object_array(json, "tabs")
        .iter()
        .map(|tab| BrowserTab {
            id: spark_json::get_str(tab, "id").unwrap_or_default(),
            url: spark_json::get_str(tab, "url").unwrap_or_default(),
            title: spark_json::get_str(tab, "title").unwrap_or_default(),
        })
        .collect()
}

/// Latest-wins frame buffer: keeps only the newest frame.
///
/// Screenshot streams can outpace the UI (a remote frame takes longer to draw
/// than to produce), so anything but the newest frame is dropped.
#[derive(Debug, Default)]
pub struct LatestFrame {
    latest: Option<BrowserFrame>,
}

impl LatestFrame {
    pub fn new() -> Self {
        Self { latest: None }
    }

    /// Store a frame, dropping it when it is older than what we already have.
    pub fn push(&mut self, frame: BrowserFrame) -> bool {
        let accepted = self
            .latest
            .as_ref()
            .map(|current| frame.frame_id >= current.frame_id)
            .unwrap_or(true);
        if accepted {
            self.latest = Some(frame);
        }
        accepted
    }

    pub fn take(&mut self) -> Option<BrowserFrame> {
        self.latest.take()
    }

    pub fn peek(&self) -> Option<&BrowserFrame> {
        self.latest.as_ref()
    }
}

/// Convenience used by the agent tools and the UI: one screenshot call.
pub async fn screenshot<T: RawRpc + ?Sized>(transport: &T, runtime_id: &str) -> Result<BrowserFrame> {
    let response = dispatch(
        transport,
        BrowserRequest {
            runtime_id: runtime_id.to_string(),
            action: BrowserAction::Screenshot {
                format: "png".to_string(),
                quality: 80,
            },
        },
    )
    .await?;
    if let Some(error) = response.error {
        bail!("{error}");
    }
    response
        .frame
        .ok_or_else(|| anyhow::anyhow!("browser returned no frame"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_transport::RpcPayload;
    use std::sync::Mutex;

    /// Records the requests a transport would send.
    struct RecordingTransport {
        response: RpcPayload,
        sent: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl RawRpc for RecordingTransport {
        async fn rpc_raw(&self, method_json: String, _binary: Option<Vec<u8>>) -> Result<RpcPayload> {
            self.sent.lock().unwrap().push(method_json);
            Ok(self.response.clone())
        }
    }

    fn transport(response: RpcPayload) -> RecordingTransport {
        RecordingTransport {
            response,
            sent: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn click_fill_and_open_use_sandd_method_names() {
        let t = transport(RpcPayload::json("{\"ok\":true}"));
        for action in [
            BrowserAction::Open {
                url: "http://127.0.0.1:8000".into(),
            },
            BrowserAction::Click {
                reference: "@e1".into(),
            },
            BrowserAction::Fill {
                reference: "@e2".into(),
                text: "hello \"world\"".into(),
            },
            BrowserAction::Press { key: "Enter".into() },
            BrowserAction::Tabs,
            BrowserAction::Close,
        ] {
            let _ = dispatch(
                &t,
                BrowserRequest {
                    runtime_id: "rt-1".into(),
                    action,
                },
            )
            .await
            .unwrap();
        }
        let sent = t.sent.lock().unwrap();
        assert_eq!(sent.len(), 6);
        assert!(sent[0].contains("\"method\":\"BrowserOpen\""));
        assert!(sent[0].contains("http://127.0.0.1:8000"));
        assert!(sent[1].contains("\"method\":\"BrowserClick\""));
        assert!(sent[2].contains("hello \\\"world\\\""), "unicode must be escaped");
        assert!(sent[3].contains("\"method\":\"BrowserPress\""));
        assert!(sent[4].contains("\"method\":\"BrowserTabs\""));
        assert!(sent[5].contains("\"method\":\"BrowserClose\""));
        for body in sent.iter() {
            assert!(body.contains("\"id\":\"rt-1\""));
        }
    }

    #[tokio::test]
    async fn screenshot_returns_raw_bytes_not_base64() {
        let t = transport(RpcPayload::with_binary(
            "{\"ok\":true,\"len\":4,\"format\":\"png\",\"frame_id\":9,\"width\":800,\"height\":600}",
            vec![0x89, b'P', b'N', b'G'],
        ));
        let frame = screenshot(&t, "rt-1").await.unwrap();
        assert_eq!(frame.data, vec![0x89, b'P', b'N', b'G']);
        assert_eq!(frame.frame_id, 9);
        assert_eq!(frame.format, "png");
        assert_eq!(frame.width, 800);
    }

    #[tokio::test]
    async fn unsupported_browser_is_reported_not_panicked() {
        let t = transport(RpcPayload::json(
            "{\"ok\":false,\"unsupported\":true,\"error\":\"browser worker not available on this machine\"}",
        ));
        let response = dispatch(
            &t,
            BrowserRequest {
                runtime_id: "rt-1".into(),
                action: BrowserAction::Screenshot {
                    format: "png".into(),
                    quality: 80,
                },
            },
        )
        .await
        .unwrap();
        let error = response.error.unwrap();
        assert!(error.contains("not available"));
        assert!(error.contains("capability"));
    }

    #[tokio::test]
    async fn tool_errors_are_data_not_transport_failures() {
        let t = transport(RpcPayload::json("{\"ok\":false,\"error\":\"no element @e7\"}"));
        let response = dispatch(
            &t,
            BrowserRequest {
                runtime_id: "rt-1".into(),
                action: BrowserAction::Click {
                    reference: "@e7".into(),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(response.error.as_deref(), Some("no element @e7"));
    }

    #[test]
    fn drops_stale_frames() {
        let mut buffer = LatestFrame::new();
        assert!(buffer.push(BrowserFrame {
            frame_id: 5,
            ..Default::default()
        }));
        assert!(!buffer.push(BrowserFrame {
            frame_id: 4,
            ..Default::default()
        }));
        assert_eq!(buffer.peek().unwrap().frame_id, 5);
        assert!(buffer.push(BrowserFrame {
            frame_id: 6,
            ..Default::default()
        }));
        assert_eq!(buffer.take().unwrap().frame_id, 6);
        assert!(buffer.take().is_none());
    }

    #[test]
    fn parses_tabs() {
        let tabs = parse_tabs(
            "{\"ok\":true,\"tabs\":[{\"id\":\"t1\",\"url\":\"http://a\",\"title\":\"A\"},{\"id\":\"t2\",\"url\":\"http://b\",\"title\":\"B\"}]}",
        );
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[1].title, "B");
    }
}
