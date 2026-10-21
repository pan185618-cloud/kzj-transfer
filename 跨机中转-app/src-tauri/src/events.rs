//! Events the backend emits to the frontend, plus commands it accepts.
//! Everything is JSON-serializable so the UI just listens.

use localsend::discovery::StatefulDevice;
use serde::Serialize;

/// Tauri event name for the full device list refresh.
pub const EVT_DEVICES: &str = "devices-changed";
/// Tauri event name for a log line.
pub const EVT_LOG: &str = "log";
/// Tauri event name for an incoming transfer request needing a decision.
pub const EVT_RECEIVE_REQUEST: &str = "receive-request";
/// Tauri event name for receive progress updates.
pub const EVT_RECEIVE_PROGRESS: &str = "receive-progress";
/// Tauri event name for send progress updates.
pub const EVT_SEND_PROGRESS: &str = "send-progress";
/// Tauri event name for completed receives (files landed on disk).
pub const EVT_RECEIVE_DONE: &str = "receive-done";
/// Tauri event name for a received text/link message (chat bubble).
pub const EVT_MESSAGE: &str = "message-received";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePayload {
    pub fingerprint: String,
    pub alias: String,
    pub host: String,
    pub port: u16,
    pub device_model: Option<String>,
    pub device_type: Option<String>,
    pub protocol: String,
    pub paired: bool,
}

impl From<&StatefulDevice> for DevicePayload {
    fn from(d: &StatefulDevice) -> Self {
        let dev = &d.device;
        let (host, port, protocol) = dev
            .http()
            .map(|h| (h.host.clone(), h.port, h.protocol.as_str().to_string()))
            .unwrap_or_else(|| ("unknown".to_string(), 0, "https".to_string()));
        Self {
            fingerprint: dev.fingerprint.clone(),
            alias: dev.alias.clone(),
            host,
            port,
            device_model: dev.device_model.clone(),
            device_type: dev.device_type.as_ref().map(|t| format!("{t:?}")),
            protocol,
            // Filled in by the backend, which owns the paired set.
            paired: false,
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPayload {
    pub category: String,
    pub text: String,
    pub ts_ms: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveRequestPayload {
    pub session_id: String,
    pub alias: String,
    pub files: Vec<FilePayload>,
    pub total_bytes: u64,
    /// Host/port/protocol so we can cancel back to the sender later.
    pub host: String,
    pub port: u16,
    pub protocol: String,
    pub fingerprint: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePayload {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub file_type: String,
    pub preview: Option<String>,
    /// Absolute path once the file has landed on disk (receive-done only).
    pub saved_path: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveProgressPayload {
    pub session_id: String,
    pub alias: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub finished_files: usize,
    pub total_files: usize,
    pub failed_files: usize,
    pub speed_bps: f64,
    /// Sender device fingerprint, for grouping by device in the UI.
    pub fingerprint: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveDonePayload {
    pub session_id: String,
    pub alias: String,
    pub received_files: usize,
    pub failed_files: usize,
    pub saved_to: String,
    /// Per-file details for the finished session.
    pub files: Vec<FilePayload>,
    /// "finished" | "cancelled"
    pub reason: String,
    /// Sender device fingerprint, for grouping by device in the UI.
    pub fingerprint: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePayload {
    /// Device fingerprint of the peer (sender for incoming, target for outgoing echo).
    pub fingerprint: String,
    pub alias: String,
    /// "in" | "out"
    pub direction: String,
    pub text: String,
    pub ts_ms: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendProgressPayload {
    /// Local send id we generated (not the remote session id).
    pub send_id: String,
    /// Remote session id once prepare-upload succeeds.
    pub session_id: Option<String>,
    pub alias: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: f64,
    /// "sending" | "done" | "failed" | "cancelled"
    pub state: String,
    pub error: Option<String>,
    /// Target device fingerprint, for grouping by device in the UI.
    pub fingerprint: String,
    /// "files" | "text"
    pub kind: String,
    /// For kind=text: the message preview.
    pub text_preview: Option<String>,
    /// For kind=files: per-file details; saved_path = local source path,
    /// so the UI can preview/ reopen what it sent.
    pub files: Vec<FilePayload>,
}
