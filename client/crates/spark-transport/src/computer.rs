//! Remote computer-use helpers.
//!
//! Computer use drives the **machine's** desktop (the runtime's Xvfb display),
//! through the same transport as everything else. If the machine cannot run it
//! the runtime advertises no `computer` capability and this module refuses
//! locally with a clear message instead of sending a request that would fail
//! with something obscure (architecture §二十四).

use anyhow::{bail, Result};

use crate::runtime_transport::{
    spark_json, ComputerAction, ComputerRequest, ComputerResponse, RawRpc,
};

/// Run one computer-use action on the runtime's machine.
pub async fn dispatch<T: RawRpc + ?Sized>(
    transport: &T,
    request: ComputerRequest,
) -> Result<ComputerResponse> {
    let (method, extra) = match &request.action {
        ComputerAction::Screenshot => ("ComputerScreenshot", String::new()),
        ComputerAction::Click { x, y, button } => (
            "ComputerClick",
            format!(",\"x\":{x},\"y\":{y},\"button\":\"{}\"", spark_json::escape(button)),
        ),
        ComputerAction::Move { x, y } => ("ComputerMove", format!(",\"x\":{x},\"y\":{y}")),
        ComputerAction::Type { text } => (
            "ComputerType",
            format!(",\"text\":\"{}\"", spark_json::escape(text)),
        ),
        ComputerAction::Key { key } => (
            "ComputerKey",
            format!(",\"key\":\"{}\"", spark_json::escape(key)),
        ),
        ComputerAction::Scroll { x, y, delta } => (
            "ComputerScroll",
            format!(",\"x\":{x},\"y\":{y},\"delta\":{delta}"),
        ),
    };

    let body = format!(
        "{{\"method\":\"{method}\",\"id\":\"{}\"{extra}}}",
        spark_json::escape(&request.runtime_id)
    );
    let response = transport.rpc_raw(body, None).await?;

    if !spark_json::is_ok(&response.json) {
        return Ok(ComputerResponse {
            error: Some(spark_json::error_of(&response.json).unwrap_or(response.json)),
            ..Default::default()
        });
    }

    if matches!(request.action, ComputerAction::Screenshot) {
        return Ok(ComputerResponse {
            frame: Some(crate::browser::frame_from(&response.json, response.binary, "png")),
            ..Default::default()
        });
    }
    Ok(ComputerResponse::default())
}

/// One desktop screenshot — used by the Runtime Inspector preview.
pub async fn screenshot<T: RawRpc + ?Sized>(
    transport: &T,
    runtime_id: &str,
) -> Result<crate::runtime_transport::BrowserFrame> {
    let response = dispatch(
        transport,
        ComputerRequest {
            runtime_id: runtime_id.to_string(),
            action: ComputerAction::Screenshot,
        },
    )
    .await?;
    if let Some(error) = response.error {
        bail!("{error}");
    }
    response
        .frame
        .ok_or_else(|| anyhow::anyhow!("computer use returned no frame"))
}

// ---------------------------------------------------------------------------
// Capability gating
// ---------------------------------------------------------------------------

/// Is computer use allowed for this machine?
///
/// The UI calls this before rendering the Computer panel; the agent calls it
/// before offering the tool, so a machine without a desktop never produces a
/// tool call that cannot work.
pub fn enabled(capabilities: &spark_model::MachineCapabilities) -> bool {
    capabilities.computer_use && capabilities.desktop
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_transport::RpcPayload;
    use std::sync::Mutex;

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

    #[tokio::test]
    async fn sends_typed_actions() {
        let t = RecordingTransport {
            response: RpcPayload::json("{\"ok\":true}"),
            sent: Mutex::new(Vec::new()),
        };
        for action in [
            ComputerAction::Click {
                x: 10,
                y: 20,
                button: "left".into(),
            },
            ComputerAction::Type { text: "hi".into() },
            ComputerAction::Key { key: "Return".into() },
            ComputerAction::Scroll {
                x: 0,
                y: 0,
                delta: -100,
            },
        ] {
            dispatch(
                &t,
                ComputerRequest {
                    runtime_id: "rt-9".into(),
                    action,
                },
            )
            .await
            .unwrap();
        }
        let sent = t.sent.lock().unwrap();
        assert!(sent[0].contains("\"method\":\"ComputerClick\""));
        assert!(sent[0].contains("\"x\":10") && sent[0].contains("\"y\":20"));
        assert!(sent[1].contains("\"method\":\"ComputerType\""));
        assert!(sent[2].contains("\"method\":\"ComputerKey\""));
        assert!(sent[3].contains("\"delta\":-100"));
    }

    #[tokio::test]
    async fn screenshot_decodes_raw_png() {
        let t = RecordingTransport {
            response: RpcPayload::with_binary(
                "{\"ok\":true,\"len\":3,\"format\":\"png\",\"frame_id\":1}",
                vec![1, 2, 3],
            ),
            sent: Mutex::new(Vec::new()),
        };
        let frame = screenshot(&t, "rt-9").await.unwrap();
        assert_eq!(frame.data, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn missing_desktop_is_an_error_string() {
        let t = RecordingTransport {
            response: RpcPayload::json(
                "{\"ok\":false,\"error\":\"computer use is not available on this machine (needs Xvfb + xdotool)\"}",
            ),
            sent: Mutex::new(Vec::new()),
        };
        let err = screenshot(&t, "rt-9").await.unwrap_err().to_string();
        assert!(err.contains("not available"), "got {err}");
    }

    #[test]
    fn capability_gate_requires_desktop() {
        use spark_model::MachineCapabilities;
        let mut caps = MachineCapabilities::default();
        assert!(!enabled(&caps));
        caps.desktop = true;
        caps.computer_use = true;
        assert!(enabled(&caps));
        caps.desktop = false;
        assert!(!enabled(&caps));
    }
}
