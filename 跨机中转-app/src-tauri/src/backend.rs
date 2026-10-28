//! The transfer backend: wraps the `localsend` core crate (HTTP server +
//! discovery + client) and drives it from Tauri commands, emitting typed
//! events for the UI.

use crate::events::*;
use base64::Engine as _;
use bytes::Bytes;
use futures_util::StreamExt;
use localsend::discovery::{
    DEFAULT_DISCOVERY_TIMEOUT, DeviceChannel, DeviceIdentity, DiscoveredDevice, DiscoveryConfig,
    DiscoveryEvent, DiscoveryHandle, HttpChannel, StatefulDevice,
};
use localsend::http::client::v2::LsHttpClientV2;
use localsend::http::client::ClientError;
use localsend::http::dto_v2::{PrepareUploadRequestDtoV2, RegisterDtoV2};
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2, SessionEndReasonV2};
use localsend::http::server::web::WebConfig;
use localsend::http::server::{ServerConfigV2, ServerHandle, TlsConfig, start_with_port};
use localsend::http::state::ClientInfo;
use localsend::model::discovery::{DeviceType, PROTOCOL_VERSION_V2, ProtocolType};
use localsend::model::transfer::{FileContent, FileDto};
use localsend::multicast::{DEFAULT_MULTICAST_GROUP, DEFAULT_MULTICAST_GROUP_V6, DEFAULT_PORT};
use localsend::reqwest;
use localsend::util::interface::InterfaceFilter;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter, Manager, State};

/// The app handle, stored at startup so helper functions (which run inside
/// commands without a `State`) can reach Tauri's path resolver on Android.
static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// ---------- identity (adapted from cli/src/storage/identity.rs) ----------

pub struct Identity {
    pub alias: String,
    pub port: u16,
    pub cert_pem: String,
    pub key_pem: String,
    pub fingerprint: String,
}

impl Identity {
    pub fn load_or_generate(dir: &Path, alias: String, port: u16) -> anyhow::Result<Self> {
        let path = dir.join("identity.pem");
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_pem(&text, alias, port),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate(alias, port)?;
                std::fs::create_dir_all(dir)?;
                #[cfg(unix)]
                {
                    use std::io::Write;
                    use std::os::unix::fs::OpenOptionsExt;
                    let mut f = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&path)?;
                    f.write_all(format!("{}{}", identity.cert_pem, identity.key_pem).as_bytes())?;
                }
                #[cfg(not(unix))]
                std::fs::write(&path, format!("{}{}", identity.cert_pem, identity.key_pem))?;
                Ok(identity)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn from_pem(text: &str, alias: String, port: u16) -> anyhow::Result<Self> {
        let blocks = pem::parse_many(text)?;
        let cert = blocks
            .iter()
            .find(|b| b.tag() == "CERTIFICATE")
            .ok_or_else(|| anyhow::anyhow!("missing CERTIFICATE block"))?;
        let key = blocks
            .iter()
            .find(|b| b.tag().ends_with("PRIVATE KEY"))
            .ok_or_else(|| anyhow::anyhow!("missing PRIVATE KEY block"))?;
        let key_pem = pem::encode(key);
        Ok(Self {
            alias,
            port,
            fingerprint: localsend::crypto::cert::fingerprint_from_cert_der(cert.contents()),
            cert_pem: pem::encode(cert),
            key_pem,
        })
    }

    fn generate(alias: String, port: u16) -> anyhow::Result<Self> {
        let cert = localsend::crypto::cert::generate_self_signed()?;
        Ok(Self {
            alias,
            port,
            fingerprint: cert.fingerprint,
            cert_pem: cert.certificate_pem,
            key_pem: cert.private_key_pem,
        })
    }

    fn tls_config(&self) -> TlsConfig {
        TlsConfig {
            cert: self.cert_pem.clone(),
            private_key: self.key_pem.clone(),
        }
    }

    #[cfg(target_os = "android")]
    fn device_model_string() -> String {
        // 读 Android ro.product.model(厂商给的机型,如 "PKC110")。
        std::process::Command::new("/system/bin/getprop")
            .arg("ro.product.model")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Android".to_string())
    }
    #[cfg(not(target_os = "android"))]
    fn device_model_string() -> String {
        "跨机中转".to_string()
    }

    #[cfg(target_os = "android")]
    const DEVICE_TYPE: DeviceType = DeviceType::Mobile;
    #[cfg(not(target_os = "android"))]
    const DEVICE_TYPE: DeviceType = DeviceType::Desktop;

    pub fn client_info(&self) -> ClientInfo {
        ClientInfo {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.to_string(),
            device_model: Some(Self::device_model_string()),
            device_type: Some(Self::DEVICE_TYPE),
            token: self.fingerprint.clone(),
        }
    }

    pub fn register_dto(&self) -> RegisterDtoV2 {
        RegisterDtoV2 {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.to_string(),
            device_model: Some(Self::device_model_string()),
            device_type: Some(Self::DEVICE_TYPE),
            fingerprint: self.fingerprint.clone(),
            port: self.port,
            protocol: ProtocolType::Https,
            download: false,
        }
    }

    pub fn multicast_device(&self) -> localsend::multicast::MulticastDevice {
        localsend::multicast::MulticastDevice {
            alias: self.alias.clone(),
            version: PROTOCOL_VERSION_V2.to_string(),
            device_model: Some(Self::device_model_string()),
            device_type: Some(Self::DEVICE_TYPE),
            fingerprint: self.fingerprint.clone(),
            port: self.port,
            protocol: ProtocolType::Https,
            download: false,
        }
    }
}

// ---------- shared state ----------

struct PendingReceive {
    session_id: String,
    alias: String,
    sender: SenderTarget,
    files: HashMap<String, FileDto>,
    decision_tx: oneshot::Sender<PrepareUploadDecisionV2>,
}

#[derive(Clone)]
struct SenderTarget {
    host: String,
    port: u16,
    protocol: ProtocolType,
    fingerprint: String,
}

struct ReceiveSession {
    session_id: String,
    alias: String,
    sender: SenderTarget,
    files: HashMap<String, FileDto>,
    /// file_id -> absolute path where it landed (filled per FileUpload).
    saved_paths: HashMap<String, String>,
    total_bytes: u64,
    finished_files: usize,
    failed_files: usize,
    finalized_bytes: u64,
    in_progress: HashMap<String, Arc<AtomicU64>>,
    started: Instant,
    ended: Option<SessionEndReasonV2>,
}

impl ReceiveSession {
    fn done_bytes(&self) -> u64 {
        self.finalized_bytes
            + self
                .in_progress
                .values()
                .map(|p| p.load(Ordering::Relaxed))
                .sum::<u64>()
    }
}

struct SendTask {
    send_id: String,
    alias: String,
    /// Target device fingerprint (for UI grouping).
    fingerprint: String,
    /// "files" | "text"
    kind: &'static str,
    /// For kind=text: the message preview.
    text_preview: Option<String>,
    /// For kind=files: per-file payloads (saved_path = local source path)
    /// emitted with every progress event so the UI can preview files.
    files_payload: Vec<FilePayload>,
    total_bytes: u64,
    sent: Arc<AtomicU64>,
    cancel_token: CancellationToken,
    by_peer: Arc<std::sync::atomic::AtomicBool>,
    session_id: Mutex<Option<String>>,
    host: String,
    /// Groups the per-device sends of one broadcast so the UI shows one bubble.
    batch: Option<String>,
}

pub struct AppState {
    pub identity: Arc<Identity>,
    pub destination: Mutex<PathBuf>,
    pub server: Arc<ServerHandle>,
    pub discovery: Arc<DiscoveryHandle>,
    /// Latest event sender for server restarts (we never restart, but the
    /// channel is what the server was given).
    _server_tx: mpsc::Sender<ServerEventV2>,

    pending: AsyncMutex<Option<PendingReceive>>,
    receive: AsyncMutex<Option<ReceiveSession>>,
    sends: AsyncMutex<HashMap<String, Arc<SendTask>>>,
    paired: Mutex<HashSet<String>>,
}

/// reqwest::Error's Display only prints the top-level
/// "error sending request for url" — the real reason
/// (证书指纹不匹配 / 连接被拒 / 超时) sits in the source chain.
fn reqwest_error_detail(err: &ClientError) -> String {
    // Walk the chain so the TLS reject reason buried under
    // "error sending request" reaches the user.
    let mut detail = err.to_string();
    let mut src = std::error::Error::source(err);
    let mut depth = 0;
    while let Some(e) = src {
        let s = e.to_string();
        if !detail.contains(&s) {
            detail.push_str(&format!(": {s}"));
        }
        src = e.source();
        depth += 1;
        if depth >= 8 {
            break;
        }
    }
    if detail.contains("fingerprint mismatch") {
        detail.push_str("(对方设备指纹已变化,可能是重装或发现了同名的旧设备,请让对方重新在线)");
    }
    detail
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn emit_log(app: &AppHandle, category: &str, text: String) {
    let _ = app.emit(
        EVT_LOG,
        LogPayload {
            category: category.to_string(),
            text,
            ts_ms: now_ms(),
        },
    );
}

fn emit_devices(app: &AppHandle, discovery: &DiscoveryHandle, paired: &Mutex<HashSet<String>>) {
    let paired = paired.lock().unwrap();
    let mut list: Vec<DevicePayload> = discovery
        .devices()
        .iter()
        .map(|d| {
            let mut p = DevicePayload::from(d);
            p.paired = paired.contains(&p.fingerprint);
            p
        })
        .collect();
    list.sort_by(|a, b| a.alias.cmp(&b.alias));
    let _ = app.emit(EVT_DEVICES, list);
}

fn config_dir() -> PathBuf {
    // Android: dirs::config_dir()/home_dir() come back empty (no $HOME), so
    // fall back to the app-private data dir exposed by Tauri.
    #[cfg(target_os = "android")]
    {
        if let Some(app) = APP_HANDLE.get() {
            if let Ok(dir) = app.path().app_data_dir() {
                return dir;
            }
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
        .join("跨机中转")
}

fn default_download_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        if let Some(app) = APP_HANDLE.get() {
            if let Ok(dir) = app.path().download_dir() {
                return dir;
            }
        }
    }
    dirs::download_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
}

/// Android 的 hostname 固定是 "localhost",对方看到的设备名就成了 localhost。
/// 改用系统里的「设备名称」或市场名(如 "OPPO Find X8 Pro")。
#[cfg(target_os = "android")]
fn default_alias() -> String {
    let getprop = |k: &str| {
        std::process::Command::new("/system/bin/getprop")
            .arg(k)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let settings = std::process::Command::new("/system/bin/settings")
        .args(["get", "global", "device_name"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "null");
    settings
        .or_else(|| getprop("ro.vendor.oplus.market.name"))
        .or_else(|| getprop("ro.product.marketname"))
        .or_else(|| {
            let brand = getprop("ro.product.brand").unwrap_or_default();
            getprop("ro.product.model").map(|m| format!("{brand} {m}").trim().to_string())
        })
        .unwrap_or_else(|| "Android".to_string())
}

#[cfg(not(target_os = "android"))]
fn default_alias() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "跨机中转".to_string())
}

fn load_alias_config() -> (String, u16, PathBuf, HashSet<String>) {
    let dir = config_dir();
    let path = dir.join("config.toml");
    let mut alias = default_alias();
    let mut port = 53317u16;
    let mut dest = default_download_dir();
    let mut paired = HashSet::new();
    if let Ok(text) = std::fs::read_to_string(&path) {
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("alias") {
                if let Some(v) = v.trim_start().strip_prefix('=') {
                    let v = v.trim().trim_matches('"').to_string();
                    // 旧版在 Android 上把 hostname "localhost" 存成了设备名,忽略它
                    if !v.is_empty() && v != "localhost" {
                        alias = v;
                    }
                }
            } else if let Some(v) = line.strip_prefix("port") {
                if let Some(v) = v.trim_start().strip_prefix('=') {
                    if let Ok(p) = v.trim().parse() {
                        port = p;
                    }
                }
            } else if let Some(v) = line.strip_prefix("destination") {
                if let Some(v) = v.trim_start().strip_prefix('=') {
                    dest = PathBuf::from(v.trim().trim_matches('"'));
                }
            } else if let Some(v) = line.strip_prefix("paired") {
                if let Some(v) = v.trim_start().strip_prefix('=') {
                    for f in v.trim().trim_start_matches('[').trim_end_matches(']').split(',') {
                        let f = f.trim().trim_matches('"');
                        if !f.is_empty() {
                            paired.insert(f.to_string());
                        }
                    }
                }
            }
        }
    }
    (alias, port, dest, paired)
}

fn save_config(state: &AppState) {
    let dir = config_dir();
    let paired: Vec<String> = state.paired.lock().unwrap().iter().cloned().collect();
    let dest = state.destination.lock().unwrap().clone();
    let text = format!(
        "alias = \"{}\"\nport = {}\ndestination = \"{}\"\npaired = [{}]\n",
        state.identity.alias,
        state.identity.port,
        dest.display(),
        paired
            .iter()
            .map(|f| format!("\"{f}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("config.toml"), text);
}

/// Spawned from `tauri::Builder::setup`.
pub fn start(app: AppHandle) {
    let _ = APP_HANDLE.set(app.clone());
    tauri::async_runtime::spawn(async move {
        if let Err(err) = start_inner(app.clone()).await {
            emit_log(&app, "error", format!("Backend failed to start: {err:#}"));
        }
    });
}

async fn start_inner(app: AppHandle) -> anyhow::Result<()> {
    let (alias, port, dest, paired) = load_alias_config();
    let identity = Arc::new(Identity::load_or_generate(&config_dir(), alias, port)?);
    let (server_tx, server_rx) = mpsc::channel::<ServerEventV2>(32);
    let (_server_stop_tx, server_stop_rx) = oneshot::channel::<()>();
    let server = start_with_port(
        identity.port,
        Some(identity.tls_config()),
        identity.client_info(),
        None,
        Some(ServerConfigV2 {
            pin: None,
            verify_checksums: true,
            event_tx: server_tx.clone(),
        }),
        WebConfig::default(),
        server_stop_rx,
    )
    .await?;

    let (discovery_tx, mut discovery_rx) = mpsc::channel::<DiscoveryEvent>(32);
    let (discovery_stop_tx, discovery_stop_rx) = oneshot::channel::<()>();
    let discovery = Arc::new(
        localsend::discovery::start(
            DiscoveryConfig {
                group: DEFAULT_MULTICAST_GROUP,
                group_v6: Some(DEFAULT_MULTICAST_GROUP_V6),
                port: DEFAULT_PORT,
                interface_filter: InterfaceFilter::default(),
                device: identity.multicast_device(),
                identity: DeviceIdentity {
                    cert_pem: identity.cert_pem.clone(),
                    private_key_pem: identity.key_pem.clone(),
                },
                timeout: DEFAULT_DISCOVERY_TIMEOUT,
                event_tx: Some(discovery_tx),
            },
            discovery_stop_rx,
        )
        .await,
    );

    let state = Arc::new(AppState {
        identity,
        destination: Mutex::new(dest),
        server: Arc::new(server),
        discovery,
        _server_tx: server_tx,
        pending: AsyncMutex::new(None),
        receive: AsyncMutex::new(None),
        sends: AsyncMutex::new(HashMap::new()),
        paired: Mutex::new(paired),
    });
    app.manage(state.clone());

    emit_log(&app, "system", format!("已启动,设备名: {}", state.identity.alias));
    emit_devices(&app, &state.discovery, &state.paired);

    // Announce ourselves and probe paired/known peers, then refresh the list.
    {
        let discovery = state.discovery.clone();
        let app2 = app.clone();
        let paired2 = Mutex::new(state.paired.lock().unwrap().clone());
        let port = state.identity.port;
        tokio::spawn(async move {
            let interface_ips =
                localsend::util::interface::local_interface_addresses(&InterfaceFilter::default())
                    .unwrap_or_default();
            let _ = discovery
                .discover_staged(
                    Vec::new(),
                    interface_ips,
                    port,
                    ProtocolType::Https,
                    Duration::from_secs(1),
                )
                .await;
            emit_devices(&app2, &discovery, &paired2);
        });
    }

    // Main event loop: server events + discovery events → UI events.
    let mut server_rx = server_rx;
    let discovery2 = state.discovery.clone();
    loop {
        tokio::select! {
            Some(event) = server_rx.recv() => {
                handle_server_event(&app, &state, event).await;
            }
            Some(event) = discovery_rx.recv() => {
                match event {
                    DiscoveryEvent::Discovered { .. } | DiscoveryEvent::Updated { .. } => {
                        emit_devices(&app, &discovery2, &state.paired);
                    }
                    DiscoveryEvent::MulticastFailed => {
                        emit_log(&app, "discovery", "组播发现不可用,只能手动指定 IP".to_string());
                    }
                }
            }
            else => break,
        }
    }
    drop(discovery_stop_tx);
    Ok(())
}

// ---------- server event handling (adapted from cli/src/app/receive.rs) ----------

fn message_of(files: &HashMap<String, FileDto>) -> Option<String> {
    if files.len() != 1 {
        return None;
    }
    let file = files.values().next()?;
    if file.file_type != "text" && !file.file_type.starts_with("text/") {
        return None;
    }
    file.preview.clone()
}

/// A device that reached us over HTTP (register or prepare-upload) is
/// reachable — feed it into the discovery store so it shows up in the list.
/// Mirrors cli/src/app/discovery.rs `device_confirmed`.
/// Awaits the store update, so a following `emit_devices` already sees the device
/// (it used to be spawned, and the list sent to the UI raced ahead of the add).
async fn device_confirmed(state: &Arc<AppState>, host: String, info: RegisterDtoV2) {
    if info.fingerprint == state.identity.fingerprint {
        return;
    }
    let device = DiscoveredDevice {
        alias: info.alias,
        version: info.version,
        device_model: info.device_model,
        device_type: info.device_type,
        fingerprint: info.fingerprint,
        channel: DeviceChannel::Http(HttpChannel {
            host,
            port: info.port,
            protocol: info.protocol,
        }),
        download: info.download,
    };
    state.discovery.add_device(device).await;
}

async fn handle_server_event(app: &AppHandle, state: &Arc<AppState>, event: ServerEventV2) {
    match event {
        ServerEventV2::Register { ip, info } => {
            device_confirmed(state, ip.to_string(), info).await;
            emit_devices(app, &state.discovery, &state.paired);
        }
        ServerEventV2::PrepareUpload {
            session_id,
            ip,
            info,
            cert_fingerprint,
            files,
            decision_tx,
        } => {
            // The sender is clearly reachable; feed it into the store.
            device_confirmed(state, ip.to_string(), info.clone()).await;
            emit_devices(app, &state.discovery, &state.paired);
            if let Some(message) = message_of(&files) {
                let _ = decision_tx.send(PrepareUploadDecisionV2::Accept(HashSet::new()));
                let fp = cert_fingerprint
                    .clone()
                    .unwrap_or_else(|| info.fingerprint.clone());
                let _ = app.emit(
                    EVT_MESSAGE,
                    MessagePayload {
                        fingerprint: fp,
                        alias: info.alias.clone(),
                        direction: "in".to_string(),
                        text: message.clone(),
                        ts_ms: now_ms(),
                    },
                );
                emit_log(app, "receive", format!("{} 发来消息", info.alias));
                return;
            }

            let sender = SenderTarget {
                host: ip.to_string(),
                port: info.port,
                protocol: info.protocol.clone(),
                fingerprint: cert_fingerprint.unwrap_or_else(|| info.fingerprint.clone()),
            };

            // Auto-accept paired devices.
            if state.paired.lock().unwrap().contains(&sender.fingerprint) {
                let ids: HashSet<String> = files.keys().cloned().collect();
                if decision_tx
                    .send(PrepareUploadDecisionV2::Accept(ids))
                    .is_ok()
                {
                    let total = files.values().map(|f| f.size).sum();
                    *state.receive.lock().await = Some(ReceiveSession {
                        session_id: session_id.clone(),
                        alias: info.alias.clone(),
                        sender,
                        files,
                        saved_paths: HashMap::new(),
                        total_bytes: total,
                        finished_files: 0,
                        failed_files: 0,
                        finalized_bytes: 0,
                        in_progress: HashMap::new(),
                        started: Instant::now(),
                        ended: None,
                    });
                    emit_log(app, "receive", format!("{} 开始自动接收(已配对)", info.alias));
                }
                return;
            }

            // Ask the user.
            let total: u64 = files.values().map(|f| f.size).sum();
            let file_payloads: Vec<FilePayload> = files
                .iter()
                .map(|(id, f)| FilePayload {
                    id: id.clone(),
                    name: f.file_name.clone(),
                    size: f.size,
                    file_type: f.file_type.clone(),
                    preview: f.preview.clone(),
                    saved_path: None,
                })
                .collect();
            *state.pending.lock().await = Some(PendingReceive {
                session_id: session_id.clone(),
                alias: info.alias.clone(),
                sender: sender.clone(),
                files: files.clone(),
                decision_tx,
            });
            let _ = app.emit(
                EVT_RECEIVE_REQUEST,
                ReceiveRequestPayload {
                    session_id,
                    alias: info.alias.clone(),
                    files: file_payloads,
                    total_bytes: total,
                    host: sender.host,
                    port: sender.port,
                    protocol: sender.protocol.as_str().to_string(),
                    fingerprint: sender.fingerprint,
                },
            );
        }
        ServerEventV2::FileUpload {
            session_id,
            file_id,
            file,
            target_tx,
        } => {
            let mut guard = state.receive.lock().await;
            let Some(session) = guard
                .as_mut()
                .filter(|s| s.session_id == session_id)
            else {
                return; // dropping target_tx fails the upload
            };
            let dest = state.destination.lock().unwrap().clone();
            let path = unique_path(&dest, &file.file_name);
            session
                .saved_paths
                .insert(file_id.clone(), path.display().to_string());
            let progress = Arc::new(AtomicU64::new(0));
            session.in_progress.insert(file_id.clone(), progress.clone());
            let (progress_tx, mut progress_rx) = mpsc::channel::<u64>(32);
            let app2 = app.clone();
            let state2 = state.clone();
            let sid = session_id.clone();
            tokio::spawn(async move {
                while let Some(written) = progress_rx.recv().await {
                    progress.store(written, Ordering::Relaxed);
                    emit_receive_progress(&app2, &state2, &sid).await;
                }
            });

            let (result_tx, result_rx) = oneshot::channel::<Result<(), String>>();
            let app3 = app.clone();
            let state3 = state.clone();
            tokio::spawn(async move {
                let result = result_rx.await.unwrap_or_else(|_| Err("upload aborted".into()));
                let mut guard = state3.receive.lock().await;
                if let Some(session) = guard
                    .as_mut()
                    .filter(|s| s.session_id == session_id)
                {
                    session.in_progress.remove(&file_id);
                    match &result {
                        Ok(()) => {
                            session.finished_files += 1;
                            session.finalized_bytes +=
                                session.files.get(&file_id).map(|f| f.size).unwrap_or(0);
                        }
                        Err(err) => {
                            session.failed_files += 1;
                            let alias = session.alias.clone();
                            drop(guard);
                            emit_log(&app3, "receive", format!("{alias}: {err}"));
                            return finish_receive_if_done(&app3, &state3).await;
                        }
                    }
                    drop(guard);
                    finish_receive_if_done(&app3, &state3).await;
                }
            });
            let _ = target_tx.send(FileUploadTarget::Path {
                path,
                result_tx,
                progress_tx: Some(progress_tx),
            });
        }
        ServerEventV2::SessionEnd { session_id, reason } => {
            let mut guard = state.receive.lock().await;
            if let Some(session) = guard
                .as_mut()
                .filter(|s| s.session_id == session_id)
            {
                session.ended = Some(reason);
            }
            drop(guard);
            finish_receive_if_done(app, state).await;
        }
        ServerEventV2::PrepareUploadAborted { session_id } => {
            let mut guard = state.pending.lock().await;
            if let Some(p) = guard.as_ref().filter(|p| p.session_id == session_id) {
                let alias = p.alias.clone();
                *guard = None;
                drop(guard);
                emit_log(app, "receive", format!("{alias}: 对方取消了请求"));
            }
        }
        ServerEventV2::CancelReceived { ip, session_id } => {
            let sends = state.sends.lock().await;
            for task in sends.values() {
                let sid = task.session_id.lock().unwrap().clone();
                if sid.as_deref() == Some(session_id.as_str()) && task.host == ip.to_string() {
                    task.by_peer.store(true, Ordering::Relaxed);
                    task.cancel_token.cancel();
                }
            }
        }
        ServerEventV2::ListenerFailed { error } => {
            emit_log(app, "receive", format!("接收服务停止: {error}"));
        }
    }
}

async fn emit_receive_progress(app: &AppHandle, state: &Arc<AppState>, session_id: &str) {
    let guard = state.receive.lock().await;
    if let Some(s) = guard.as_ref().filter(|s| s.session_id == session_id) {
        let elapsed = s.started.elapsed().as_secs_f64().max(0.01);
        let _ = app.emit(
            EVT_RECEIVE_PROGRESS,
            ReceiveProgressPayload {
                session_id: s.session_id.clone(),
                alias: s.alias.clone(),
                done_bytes: s.done_bytes(),
                total_bytes: s.total_bytes,
                finished_files: s.finished_files,
                total_files: s.files.len(),
                failed_files: s.failed_files,
                speed_bps: s.done_bytes() as f64 / elapsed,
                fingerprint: s.sender.fingerprint.clone(),
            },
        );
    }
}

async fn finish_receive_if_done(app: &AppHandle, state: &Arc<AppState>) {
    let mut guard = state.receive.lock().await;
    let done = guard
        .as_ref()
        .is_some_and(|s| s.ended.is_some() && s.in_progress.is_empty());
    if !done {
        return;
    }
    let session = guard.take().unwrap();
    drop(guard);
    let dest = state.destination.lock().unwrap().clone();
    let reason = match session.ended.unwrap() {
        SessionEndReasonV2::Finished => "finished",
        SessionEndReasonV2::Cancelled => "cancelled",
    };
    let files: Vec<FilePayload> = session
        .files
        .iter()
        .map(|(id, f)| FilePayload {
            id: id.clone(),
            name: f.file_name.clone(),
            size: f.size,
            file_type: f.file_type.clone(),
            preview: None,
            saved_path: session.saved_paths.get(id).cloned(),
        })
        .collect();
    let _ = app.emit(
        EVT_RECEIVE_DONE,
        ReceiveDonePayload {
            session_id: session.session_id.clone(),
            alias: session.alias.clone(),
            received_files: session.finished_files,
            failed_files: session.failed_files,
            saved_to: dest.display().to_string(),
            files,
            reason: reason.to_string(),
            fingerprint: session.sender.fingerprint.clone(),
        },
    );
    emit_log(
        app,
        "receive",
        format!(
            "{}: 接收完成 {} 个文件 ({})",
            session.alias,
            session.finished_files,
            dest.display()
        ),
    );
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let safe = name.replace('/', "_").replace('\\', "_").replace(':', "_");
    let candidate = dir.join(&safe);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(&safe)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| safe.clone());
    let ext = Path::new(&safe)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..1000 {
        let p = dir.join(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    candidate
}

// ---------- send task (adapted from cli/src/send_task.rs) ----------

fn collect_files(paths: &[PathBuf]) -> (HashMap<String, FileDto>, HashMap<String, FileSource>, u64) {
    let mut files = HashMap::new();
    let mut sources = HashMap::new();
    let mut total = 0u64;
    for path in paths {
        collect_path(path, path, &mut files, &mut sources, &mut total);
    }
    (files, sources, total)
}

/// Android: SAF file pickers return `content://` URIs, not real paths. We can't
/// turn those into a PathBuf — instead we record the URI + metadata and open the
/// actual ParcelFileDescriptor at send time via ContentResolver.
#[cfg(target_os = "android")]
fn collect_files_android(items: Vec<AndroidPickItem>) -> (HashMap<String, FileDto>, HashMap<String, FileSource>, u64) {
    let mut files = HashMap::new();
    let mut sources = HashMap::new();
    let mut total = 0u64;
    for item in items {
        let id = uuid::Uuid::new_v4().to_string();
        let name = item.name.clone().unwrap_or_else(|| {
            item.uri
                .rsplit('/')
                .next()
                .unwrap_or("file")
                .to_string()
        });
        let file_type = item
            .mime_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let size = item.size.unwrap_or(0) as u64;
        files.insert(
            id.clone(),
            FileDto {
                id: id.clone(),
                file_name: name,
                size,
                file_type,
                sha256: None,
                preview: None,
                metadata: None,
            },
        );
        sources.insert(id, FileSource::ContentUri(item.uri));
        total += size;
    }
    (files, sources, total)
}

/// One row picked by Android's SAF dialog — URI plus whatever metadata the
/// ContentResolver could tell us. Anything the provider won't say stays None.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub struct AndroidPickItem {
    pub uri: String,
    pub name: Option<String>,
    pub size: Option<i64>,
    pub mime_type: Option<String>,
}

enum FileSource {
    Path(PathBuf),
    Bytes(Bytes),
    /// Android: a `content://` URI resolved on demand via ContentResolver.
    /// The raw fd is only opened at send time (in run_send), because that's
    /// where `run_on_android_context` can hand us a live JNIEnv.
    #[cfg(target_os = "android")]
    ContentUri(String),
}

/// Android: open a `content://` URI as a raw read fd via ContentResolver.
/// Ownership of the detached fd is handed to the caller (`FileContent::Fd`
/// closes it when it finishes reading). JNIEnv lives on the main Android
/// thread — wry's `dispatch` posts our closure there.
#[cfg(target_os = "android")]
fn open_content_uri_fd(_app: &AppHandle, uri: &str) -> Result<std::os::fd::RawFd, String> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    let uri = uri.to_string();
    wry::prelude::dispatch(move |env, activity, _webview| {
        let result: Result<std::os::fd::RawFd, String> = (|| {
            let uri_obj = env
                .new_string(&uri)
                .map_err(|e| format!("jni new_string: {e}"))?;
            let uri_class = env
                .find_class("android/net/Uri")
                .map_err(|e| format!("find Uri class: {e}"))?;
            let uri = env
                .call_static_method(
                    uri_class,
                    "parse",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[jni::objects::JValue::Object(&uri_obj.into())],
                )
                .map_err(|e| format!("Uri.parse: {e}"))?
                .l()
                .map_err(|e| format!("Uri.parse not object: {e}"))?;
            let resolver = env
                .call_method(
                    activity,
                    "getContentResolver",
                    "()Landroid/content/ContentResolver;",
                    &[],
                )
                .map_err(|e| format!("getContentResolver: {e}"))?
                .l()
                .map_err(|e| format!("resolver not object: {e}"))?;
            let mode = env
                .new_string("r")
                .map_err(|e| format!("jni new_string: {e}"))?;
            let pfd = env
                .call_method(
                    &resolver,
                    "openFileDescriptor",
                    "(Landroid/net/Uri;Ljava/lang/String;)Landroid/os/ParcelFileDescriptor;",
                    &[
                        jni::objects::JValue::Object(&uri),
                        jni::objects::JValue::Object(&mode.into()),
                    ],
                )
                .map_err(|e| format!("openFileDescriptor: {e}"))?
                .l()
                .map_err(|e| format!("pfd not object: {e}"))?;
            if pfd.is_null() {
                return Err("content provider returned null".to_string());
            }
            let fd = env
                .call_method(&pfd, "detachFd", "()I", &[])
                .map_err(|e| format!("detachFd: {e}"))?
                .i()
                .map_err(|e| format!("detachFd not int: {e}"))?;
            if fd < 0 {
                return Err(format!("detachFd returned {fd}"));
            }
            Ok(fd as std::os::fd::RawFd)
        })();
        let _ = tx.send(result);
    });
    rx.recv()
        .map_err(|e| format!("android context channel closed: {e}"))?
}

fn collect_path(
    root: &Path,
    path: &Path,
    files: &mut HashMap<String, FileDto>,
    sources: &mut HashMap<String, FileSource>,
    total: &mut u64,
) {
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                collect_path(root, &e, files, sources, total);
            }
        }
        return;
    }
    let Ok(meta) = path.metadata() else {
        return;
    };
    if !meta.is_file() {
        return;
    }
    let file_name = if path == root {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string())
    } else {
        path.strip_prefix(root.parent().unwrap_or(root))
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let id = uuid::Uuid::new_v4().to_string();
    let file_type = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string();
    files.insert(
        id.clone(),
        FileDto {
            id: id.clone(),
            file_name,
            size: meta.len(),
            file_type,
            sha256: None,
            preview: None,
            metadata: None,
        },
    );
    sources.insert(id, FileSource::Path(path.to_path_buf()));
    *total += meta.len();
}

async fn run_send(
    app: AppHandle,
    state: Arc<AppState>,
    task: Arc<SendTask>,
    device: StatefulDevice,
    files: HashMap<String, FileDto>,
    mut sources: HashMap<String, FileSource>,
) {
    let send_id = task.send_id.clone();
    let alias = task.alias.clone();
    macro_rules! progress {
        ($state:expr, $err:expr) => {{
            let elapsed_total = task.sent.load(Ordering::Relaxed);
            let _ = app.emit(
                EVT_SEND_PROGRESS,
                SendProgressPayload {
                    send_id: send_id.clone(),
                    session_id: task.session_id.lock().unwrap().clone(),
                    alias: alias.clone(),
                    done_bytes: elapsed_total,
                    total_bytes: task.total_bytes,
                    speed_bps: 0.0,
                    state: $state.to_string(),
                    error: $err,
                    fingerprint: task.fingerprint.clone(),
                    kind: task.kind.to_string(),
                    text_preview: task.text_preview.clone(),
                    files: task.files_payload.clone(),
                    batch: task.batch.clone(),
                },
            );
        }};
    }

    let fail = |msg: String| {
        let app = app.clone();
        let task = task.clone();
        async move {
            let _ = app.emit(
                EVT_SEND_PROGRESS,
                SendProgressPayload {
                    send_id: task.send_id.clone(),
                    session_id: task.session_id.lock().unwrap().clone(),
                    alias: task.alias.clone(),
                    done_bytes: task.sent.load(Ordering::Relaxed),
                    total_bytes: task.total_bytes,
                    speed_bps: 0.0,
                    state: "failed".to_string(),
                    error: Some(msg),
                    fingerprint: task.fingerprint.clone(),
                    kind: task.kind.to_string(),
                    text_preview: task.text_preview.clone(),
                    files: task.files_payload.clone(),
                    batch: task.batch.clone(),
                },
            );
        }
    };

    // Try every known address of the device, best-ranked first: a peer that
    // moved to a new IP keeps its stale channels listed as Available (nothing
    // in core ever marks a channel NotReachable), so the first address may be
    // dead. Only transport-level failures (unreachable host, TLS reject)
    // fall through to the next address — a StatusCode answer means the peer
    // itself decided, retrying another address of the same peer changes
    // nothing, and a fingerprint mismatch would fail identically everywhere.
    let channels: Vec<HttpChannel> = device
        .get_ranked_channels()
        .into_iter()
        .filter_map(|c| c.http().cloned())
        .collect();
    if channels.is_empty() {
        fail(format!("{alias}: 没有可用地址")).await;
        return;
    }

    let expected_fingerprint = Some(device.device.fingerprint.clone());
    let is_message = files.len() == 1 && files.values().all(|f| f.preview.is_some());

    let mut client = None;
    let mut http = None;
    let mut protocol = ProtocolType::Http;
    let mut prepared = None;
    let mut last_err: Option<ClientError> = None;
    let total_channels = channels.len();
    for (idx, channel) in channels.into_iter().enumerate() {
        protocol = channel.protocol.clone();
        let expected = match protocol {
            ProtocolType::Https => expected_fingerprint.clone(),
            ProtocolType::Http => None,
        };
        let c = match LsHttpClientV2::try_new(
            &state.identity.key_pem,
            &state.identity.cert_pem,
            expected,
            None,
        ) {
            Ok(c) => c,
            Err(err) => {
                fail(format!("{alias}: HTTP client 创建失败: {err}")).await;
                return;
            }
        };
        match c
            .prepare_upload(
                protocol.clone(),
                &channel.host,
                channel.port,
                None,
                PrepareUploadRequestDtoV2 {
                    info: state.identity.register_dto(),
                    files: files.clone(),
                },
                None,
                task.cancel_token.clone(),
            )
            .await
        {
            Ok(p) => {
                client = Some(c);
                http = Some(channel);
                prepared = Some(p);
                break;
            }
            Err(ClientError::Cancelled) => {
                progress!("cancelled", None);
                return;
            }
            Err(ClientError::StatusCode(err)) => {
                let reason = match err.status {
                    401 => "需要 PIN 码".to_string(),
                    403 => "对方拒绝了".to_string(),
                    409 => "对方正忙(另一会话进行中)".to_string(),
                    429 => "请求太频繁".to_string(),
                    status => format!("HTTP {status}"),
                };
                fail(format!("{alias}: {reason}")).await;
                return;
            }
            Err(err) => {
                // The mismatch string sits somewhere in the source chain;
                // reqwest_error_detail already walks it fully.
                let detail = reqwest_error_detail(&err);
                if detail.contains("fingerprint mismatch") {
                    fail(format!("{alias}: {detail}")).await;
                    return;
                }
                emit_log(
                    &app,
                    "send",
                    format!(
                        "{alias}: 地址 {}:{} 不可达,重试下一个 ({}/{})",
                        channel.host,
                        channel.port,
                        idx + 1,
                        total_channels
                    ),
                );
                last_err = Some(err);
            }
        }
    }

    let (Some(client), Some(http), Some(prepared)) = (client, http, prepared) else {
        let detail = last_err
            .as_ref()
            .map(reqwest_error_detail)
            .unwrap_or_else(|| "没有可用地址".to_string());
        fail(format!("{alias}: {detail}")).await;
        return;
    };

    let Some(response) = prepared.response else {
        if is_message {
            if let Some(text) = task.text_preview.clone() {
                let _ = app.emit(
                    EVT_MESSAGE,
                    MessagePayload {
                        fingerprint: task.fingerprint.clone(),
                        alias: alias.clone(),
                        direction: "out".to_string(),
                        text,
                        ts_ms: now_ms(),
                    },
                );
            }
            progress!("done", None);
            return;
        }
        fail(format!("{alias}: 对方拒绝了所有文件")).await;
        return;
    };
    if prepared.status_code != 200 || response.session_id.is_empty() {
        fail(format!("{alias}: 对方响应无效")).await;
        return;
    }
    *task.session_id.lock().unwrap() = Some(response.session_id.clone());

    let mut file_ids: Vec<&String> = response.files.keys().collect();
    file_ids.sort_by_key(|id| &files[*id].file_name);

    let started = Instant::now();
    let mut sent_files = 0usize;
    let mut sent_bytes = 0u64;
    let progress_counter = task.sent.clone();
    for file_id in file_ids {
        let token = &response.files[file_id];
        let file = &files[file_id];
        let content = match sources.remove(file_id.as_str()) {
            Some(FileSource::Path(p)) => FileContent::Path(p),
            Some(FileSource::Bytes(b)) => {
                let (tx, rx) = mpsc::channel(1);
                let _ = tx.try_send(b);
                FileContent::Stream(rx)
            }
            #[cfg(target_os = "android")]
            Some(FileSource::ContentUri(uri)) => {
                match open_content_uri_fd(&app, &uri) {
                    Ok(fd) => FileContent::Fd(fd),
                    Err(e) => {
                        fail(format!("{alias}: 无法读取文件 {uri}: {e}")).await;
                        return;
                    }
                }
            }
            #[cfg(not(target_os = "android"))]
            Some(other) => {
                let _ = other;
                continue;
            }
            None => continue,
        };
        let base = sent_bytes;
        let pc = progress_counter.clone();
        let app2 = app.clone();
        let send_id2 = send_id.clone();
        let alias2 = alias.clone();
        let total = task.total_bytes;
        let started2 = started;
        let fp2 = task.fingerprint.clone();
        let kind2 = task.kind;
        let tp2 = task.text_preview.clone();
        let files2 = task.files_payload.clone();
        let batch2 = task.batch.clone();
        let mut sent_of_file = 0u64;
        let stream = content.into_stream().map(move |chunk| {
            let chunk = chunk?;
            sent_of_file += chunk.len() as u64;
            let done = base + sent_of_file;
            pc.store(done, Ordering::Relaxed);
            let _ = app2.emit(
                EVT_SEND_PROGRESS,
                SendProgressPayload {
                    send_id: send_id2.clone(),
                    session_id: None,
                    alias: alias2.clone(),
                    done_bytes: done,
                    total_bytes: total,
                    speed_bps: done as f64 / started2.elapsed().as_secs_f64().max(0.01),
                    state: "sending".to_string(),
                    error: None,
                    fingerprint: fp2.clone(),
                    kind: kind2.to_string(),
                    text_preview: tp2.clone(),
                    files: files2.clone(),
                    batch: batch2.clone(),
                },
            );
            Ok::<Bytes, anyhow::Error>(chunk)
        });
        let body = reqwest::Body::wrap_stream(stream);

        match client
            .upload(
                protocol.clone(),
                &http.host,
                http.port,
                None,
                &response.session_id,
                file_id,
                token,
                body,
                task.cancel_token.clone(),
            )
            .await
        {
            Ok(()) => {
                sent_files += 1;
                sent_bytes += file.size;
                progress_counter.store(sent_bytes, Ordering::Relaxed);
            }
            Err(ClientError::Cancelled) => {
                if !task.by_peer.load(Ordering::Relaxed) {
                    let _ = client
                        .cancel(protocol.clone(), &http.host, http.port, &response.session_id)
                        .await;
                }
                progress!("cancelled", None);
                return;
            }
            Err(err) => {
                let _ = client
                    .cancel(protocol.clone(), &http.host, http.port, &response.session_id)
                    .await;
                fail(format!(
                    "{alias}: 上传 {} 失败: {}",
                    file.file_name,
                    reqwest_error_detail(&err)
                ))
                .await;
                return;
            }
        }
    }

    if is_message && sent_files == 1 {
        progress!("done", None);
        return;
    }
    emit_log(
        &app,
        "send",
        format!(
            "{alias}: 已发送 {sent_files} 个文件 ({:.1} MB)",
            sent_bytes as f64 / 1e6
        ),
    );
    progress!("done", None);
}

// ---------- Tauri commands ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPayload {
    pub alias: String,
    pub port: u16,
    pub destination: String,
    pub fingerprint: String,
}

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<ConfigPayload, String> {
    Ok(ConfigPayload {
        alias: state.identity.alias.clone(),
        port: state.identity.port,
        destination: state.destination.lock().unwrap().display().to_string(),
        fingerprint: state.identity.fingerprint.clone(),
    })
}

#[tauri::command]
pub async fn list_devices(state: State<'_, Arc<AppState>>) -> Result<Vec<DevicePayload>, String> {
    let paired = state.paired.lock().unwrap();
    Ok(state
        .discovery
        .devices()
        .iter()
        .map(|d| {
            let mut p = DevicePayload::from(d);
            p.paired = paired.contains(&p.fingerprint);
            p
        })
        .collect())
}

#[tauri::command]
pub async fn send_files(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    fingerprint: String,
    paths: Vec<String>,
    picked: Option<Vec<AndroidPickItem>>,
    batch: Option<String>,
) -> Result<String, String> {
    let device = state
        .discovery
        .device_by_fingerprint(&fingerprint)
        .ok_or_else(|| "设备未被发现,请稍候或检查是否在同一局域网".to_string())?;

    #[cfg(target_os = "android")]
    let (files, sources, total_bytes) = {
        let picked = picked.unwrap_or_default();
        if !picked.is_empty() {
            collect_files_android(picked)
        } else {
            let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
            for p in &paths {
                if !p.exists() {
                    return Err(format!("文件不存在: {}", p.display()));
                }
            }
            collect_files(&paths)
        }
    };
    #[cfg(not(target_os = "android"))]
    let (files, sources, total_bytes) = {
        let _ = picked;
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        for p in &paths {
            if !p.exists() {
                return Err(format!("文件不存在: {}", p.display()));
            }
        }
        collect_files(&paths)
    };

    if files.is_empty() {
        return Err("没有可发送的文件".to_string());
    }
    // Per-file payload for the UI: saved_path = local source path so sent
    // images can be previewed and reopened from the chat bubble.
    let mut files_payload: Vec<FilePayload> = files
        .values()
        .map(|f| FilePayload {
            id: f.id.clone(),
            name: f.file_name.clone(),
            size: f.size,
            file_type: f.file_type.clone(),
            preview: f.preview.clone(),
            saved_path: sources.get(&f.id).and_then(|s| match s {
                FileSource::Path(p) => Some(p.display().to_string()),
                FileSource::Bytes(_) => None,
                #[cfg(target_os = "android")]
                FileSource::ContentUri(_) => None,
            }),
        })
        .collect();
    files_payload.sort_by(|a, b| a.name.cmp(&b.name));
    let send_id = uuid::Uuid::new_v4().to_string();
    let task = Arc::new(SendTask {
        send_id: send_id.clone(),
        alias: device.device.alias.clone(),
        fingerprint: device.device.fingerprint.clone(),
        kind: "files",
        text_preview: None,
        files_payload,
        total_bytes,
        sent: Arc::new(AtomicU64::new(0)),
        cancel_token: CancellationToken::new(),
        by_peer: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        session_id: Mutex::new(None),
        host: device
            .get_best_channel()
            .and_then(|c| c.http())
            .map(|h| h.host.clone())
            .unwrap_or_default(),
        batch,
    });
    state
        .sends
        .lock()
        .await
        .insert(send_id.clone(), task.clone());
    let st = state.inner().clone();
    let app2 = app.clone();
    let done_id = send_id.clone();
    tokio::spawn(async move {
        run_send(app2, st.clone(), task, device, files, sources).await;
        st.sends.lock().await.remove(&done_id);
    });
    Ok(send_id)
}

#[tauri::command]
pub async fn send_text(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    fingerprint: String,
    text: String,
    batch: Option<String>,
) -> Result<String, String> {
    let device = state
        .discovery
        .device_by_fingerprint(&fingerprint)
        .ok_or_else(|| "设备未被发现".to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    let preview = text.chars().take(256).collect::<String>();
    let bytes = Bytes::from(text.into_bytes());
    let size = bytes.len() as u64;
    let mut files = HashMap::new();
    files.insert(
        id.clone(),
        FileDto {
            id: id.clone(),
            file_name: "message.txt".to_string(),
            size,
            file_type: "text/plain".to_string(),
            sha256: None,
            preview: Some(preview.clone()),
            metadata: None,
        },
    );
    let mut sources = HashMap::new();
    sources.insert(id, FileSource::Bytes(bytes));

    let send_id = uuid::Uuid::new_v4().to_string();
    let task = Arc::new(SendTask {
        send_id: send_id.clone(),
        alias: device.device.alias.clone(),
        fingerprint: device.device.fingerprint.clone(),
        kind: "text",
        text_preview: Some(preview.clone()),
        files_payload: vec![],
        total_bytes: size,
        sent: Arc::new(AtomicU64::new(0)),
        cancel_token: CancellationToken::new(),
        by_peer: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        session_id: Mutex::new(None),
        host: device
            .get_best_channel()
            .and_then(|c| c.http())
            .map(|h| h.host.clone())
            .unwrap_or_default(),
        batch,
    });
    state
        .sends
        .lock()
        .await
        .insert(send_id.clone(), task.clone());
    let st = state.inner().clone();
    let done_id = send_id.clone();
    tokio::spawn(async move {
        run_send(app, st.clone(), task, device, files, sources).await;
        st.sends.lock().await.remove(&done_id);
    });
    Ok(send_id)
}

#[tauri::command]
pub async fn answer_receive(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    accept: bool,
    pair: bool,
) -> Result<(), String> {
    let pending = state.pending.lock().await.take();
    let Some(pending) = pending else {
        return Err("没有待处理的请求".to_string());
    };
    if !accept {
        let _ = pending.decision_tx.send(PrepareUploadDecisionV2::Decline);
        emit_log(&app, "receive", format!("{}: 已拒绝", pending.alias));
        return Ok(());
    }
    if pair {
        state
            .paired
            .lock()
            .unwrap()
            .insert(pending.sender.fingerprint.clone());
        save_config(&state);
        emit_log(&app, "receive", format!("{}: 已配对,以后自动接收", pending.alias));
        emit_devices(&app, &state.discovery, &state.paired);
    }
    let ids: HashSet<String> = pending.files.keys().cloned().collect();
    if pending
        .decision_tx
        .send(PrepareUploadDecisionV2::Accept(ids))
        .is_err()
    {
        return Err("请求已失效".to_string());
    }
    let total = pending.files.values().map(|f| f.size).sum();
    *state.receive.lock().await = Some(ReceiveSession {
        session_id: pending.session_id.clone(),
        alias: pending.alias.clone(),
        sender: pending.sender,
        files: pending.files,
        saved_paths: HashMap::new(),
        total_bytes: total,
        finished_files: 0,
        failed_files: 0,
        finalized_bytes: 0,
        in_progress: HashMap::new(),
        started: Instant::now(),
        ended: None,
    });
    emit_log(&app, "receive", format!("{}: 开始接收", pending.alias));
    Ok(())
}

#[tauri::command]
pub async fn cancel_send(
    state: State<'_, Arc<AppState>>,
    send_id: String,
) -> Result<(), String> {
    if let Some(task) = state.sends.lock().await.get(&send_id) {
        task.cancel_token.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn cancel_receive(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let session = state.receive.lock().await.take();
    let Some(session) = session else {
        return Ok(());
    };
    state.server.cancel_v2_session(&session.session_id).await;
    // Best effort notify sender.
    let expected = match session.sender.protocol {
        ProtocolType::Https => Some(session.sender.fingerprint.clone()),
        ProtocolType::Http => None,
    };
    if let Ok(client) = LsHttpClientV2::try_new(
        &state.identity.key_pem,
        &state.identity.cert_pem,
        expected,
        Some(Duration::from_secs(5)),
    ) {
        let _ = client
            .cancel(
                session.sender.protocol.clone(),
                &session.sender.host,
                session.sender.port,
                &session.session_id,
            )
            .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn set_alias(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    alias: String,
) -> Result<(), String> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Err("设备名不能为空".to_string());
    }
    // Alias is baked into the cert identity; changing it needs a restart of
    // the server. Cheapest correct approach: update config, tell the user to
    // restart the app.
    let dir = config_dir();
    let paired: Vec<String> = state.paired.lock().unwrap().iter().cloned().collect();
    let dest = state.destination.lock().unwrap().clone();
    let text = format!(
        "alias = \"{}\"\nport = {}\ndestination = \"{}\"\npaired = [{}]\n",
        alias,
        state.identity.port,
        dest.display(),
        paired
            .iter()
            .map(|f| format!("\"{f}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("config.toml"), text).map_err(|e| e.to_string())?;
    emit_log(
        &app,
        "system",
        "设备名已保存,重启应用后生效".to_string(),
    );
    Ok(())
}

#[tauri::command]
pub async fn set_destination(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err("不是有效目录".to_string());
    }
    *state.destination.lock().unwrap() = p;
    save_config(&state);
    Ok(())
}

#[tauri::command]
pub async fn open_received_dir(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let dest = state.destination.lock().unwrap().clone();
    open::that_detached(&dest).map_err(|e| e.to_string())
}

/// Copy a local file to a user-chosen location (另存为). The destination is
/// picked by the UI via the dialog plugin; this command only copies.
#[tauri::command]
pub async fn copy_file_to(src: String, dst: String) -> Result<(), String> {
    let src_path = PathBuf::from(&src);
    if !src_path.is_file() {
        return Err("源文件不存在".to_string());
    }
    let dst_path = PathBuf::from(&dst);
    if dst_path == src_path {
        return Ok(());
    }
    if let Some(parent) = dst_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    tokio::task::spawn_blocking(move || std::fs::copy(&src_path, &dst_path).map(|_| ()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("复制失败: {e}"))?;
    Ok(())
}

/// Open a received file with the system default app.
#[tauri::command]
pub async fn open_file(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("文件不存在".to_string());
    }
    open::that_detached(&p).map_err(|e| e.to_string())
}

/// Read a local image file and return a data URL for inline preview.
/// Only image/* files under ~20MB are served; anything else errors out.
#[tauri::command]
pub async fn read_image_preview(path: String) -> Result<String, String> {
    const MAX_BYTES: u64 = 20 * 1024 * 1024;
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("文件不存在".to_string());
    }
    let mime = mime_guess::from_path(&p)
        .first_raw()
        .unwrap_or_default()
        .to_string();
    if !mime.starts_with("image/") {
        return Err("不是图片文件".to_string());
    }
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err("图片过大,无法预览".to_string());
    }
    let bytes =
        tokio::task::spawn_blocking(move || std::fs::read(p))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e: std::io::Error| e.to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

/// Open a link in the default browser (本机打开,不中转).
#[tauri::command]
pub async fn open_link(url: String) -> Result<(), String> {
    let u = url.trim();
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err("不是有效链接".to_string());
    }
    open::that_detached(u).map_err(|e| e.to_string())
}

/// Drag received files/folders (or a link) out of the window into Finder /
/// Explorer or another app (微信、QQ、Claude…). Must be invoked while the
/// mouse button is still held: the OS drag session takes over the pointer.
/// `icon` is a base64 PNG (optionally a data URL) used as the drag image.
/// Emits `drag-out-end` when the session finishes so the UI can ignore the
/// drop if the user releases over our own window.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tauri::command]
pub fn start_drag_out(
    window: tauri::WebviewWindow,
    paths: Vec<String>,
    url: Option<String>,
    icon: Option<String>,
) -> Result<(), String> {
    let files: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    if let Some(p) = files.iter().find(|p| !p.is_absolute() || !p.exists()) {
        return Err(format!("文件不存在: {}", p.display()));
    }
    let url = url.filter(|u| u.starts_with("http://") || u.starts_with("https://"));
    if files.is_empty() && url.is_none() {
        return Err("没有可拖出的内容".to_string());
    }
    // Windows: the drag crate cannot carry raw data, only files — drag a
    // temporary .url Internet Shortcut instead, which Explorer, browsers and
    // chat apps all accept as a link.
    #[cfg(target_os = "windows")]
    let (files, url) = if files.is_empty() {
        let link = url.clone().unwrap_or_default();
        let host = link
            .split("://")
            .nth(1)
            .and_then(|r| r.split(['/', '?', '#']).next())
            .unwrap_or("link")
            .replace(':', "_");
        let dir = std::env::temp_dir().join("kuajizhongzhuan-links");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{host}.url"));
        std::fs::write(&path, format!("[InternetShortcut]\r\nURL={link}\r\n"))
            .map_err(|e| e.to_string())?;
        (vec![path], url)
    } else {
        (files, url)
    };
    let icon_bytes = icon
        .map(|s| s.rsplit(',').next().unwrap_or_default().to_string())
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| include_bytes!("../icons/32x32.png").to_vec());

    let app = window.app_handle().clone();
    let win = window.clone();
    window
        .run_on_main_thread(move || {
            let item = if !files.is_empty() {
                drag::DragItem::Files(files)
            } else {
                let link = url.unwrap_or_default();
                drag::DragItem::Data {
                    provider: Box::new(move |_ty| Some(link.clone().into_bytes())),
                    types: vec!["public.url".into(), "public.utf8-plain-text".into()],
                }
            };
            let done = app.clone();
            let res = drag::start_drag(
                &win,
                item,
                drag::Image::Raw(icon_bytes),
                move |result, _pos| {
                    let dropped = matches!(result, drag::DragResult::Dropped);
                    let _ = done.emit("drag-out-end", dropped);
                },
                drag::Options::default(),
            );
            if let Err(e) = res {
                let _ = app.emit("drag-out-end", false);
                eprintln!("start_drag failed: {e}");
            }
        })
        .map_err(|e| e.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[tauri::command]
pub fn start_drag_out(
    paths: Vec<String>,
    url: Option<String>,
    icon: Option<String>,
) -> Result<(), String> {
    let _ = (paths, url, icon);
    Err("当前平台不支持拖出".to_string())
}

/// Read absolute file paths off the general pasteboard so that pasting a
/// copied file (Finder ⌘C → ⌘V in the composer) sends it like a drop.
/// Returns an empty list when the pasteboard holds no file URLs.
/// Same read path as the vendored wry drag-drop fix (wry#1844): the modern
/// `readObjectsForClasses:options:` API, with NSFilenamesPboardType as
/// fallback for apps that only publish the legacy type.
#[tauri::command]
pub async fn paste_file_paths() -> Result<Vec<String>, String> {
    paste_file_paths_impl()
}

#[cfg(target_os = "macos")]
fn paste_file_paths_impl() -> Result<Vec<String>, String> {
    use objc2::ClassType;
    use objc2_app_kit::NSPasteboard;
    #[allow(deprecated)]
    use objc2_app_kit::NSFilenamesPboardType;
    use objc2_foundation::{NSArray, NSString, NSURL};
    use std::ffi::CStr;

    let mut out: Vec<String> = Vec::new();
    unsafe {
        let pb = NSPasteboard::generalPasteboard();

        let url_classes = NSArray::from_slice(&[NSURL::class()]);
        if let Some(items) = pb.readObjectsForClasses_options(&url_classes, None) {
            for item in items.iter() {
                if let Some(url) = item.downcast_ref::<NSURL>() {
                    if !url.isFileURL() {
                        continue;
                    }
                    // `filePathURL` resolves file reference URLs
                    // (`file:///.file/id=...`) to real path URLs.
                    if let Some(path) = url.filePathURL().and_then(|u| u.path()) {
                        out.push(
                            CStr::from_ptr(path.UTF8String())
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }

        #[allow(deprecated)]
        {
            let types = NSArray::arrayWithObject(NSFilenamesPboardType);
            if pb.availableTypeFromArray(&types).is_some() {
                if let Some(pl) = pb.propertyListForType(NSFilenamesPboardType) {
                    if let Ok(paths) = pl.downcast::<NSArray>() {
                        for path in paths.iter() {
                            if let Ok(path) = path.downcast::<NSString>() {
                                out.push(
                                    CStr::from_ptr(path.UTF8String())
                                        .to_string_lossy()
                                        .into_owned(),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(not(target_os = "macos"))]
fn paste_file_paths_impl() -> Result<Vec<String>, String> {
    // Windows/Linux: WebView2/gtk paste doesn't expose absolute file paths
    // through DataTransfer either; add a native reader per-platform if needed.
    Ok(Vec::new())
}

/// 剪贴板内容:文件路径(Finder 复制)、图片(截图/网页图片,已存成临时 PNG)
/// 或文字。优先级 文件 > 图片 > 文字,和系统粘贴的直觉一致。
#[derive(Serialize, Default)]
pub struct ClipboardPayload {
    files: Vec<String>,
    image: Option<String>,
    text: Option<String>,
    /// Android:剪贴板里是图片/文件时给 content:// URI,按选择器同样的方式发送
    picked: Vec<AndroidPickItem>,
}

#[tauri::command]
pub async fn read_clipboard() -> Result<ClipboardPayload, String> {
    #[cfg(target_os = "android")]
    {
        return android_read_clipboard();
    }
    #[allow(unreachable_code)]
    let files = paste_file_paths_impl()?;
    if !files.is_empty() {
        return Ok(ClipboardPayload { files, ..Default::default() });
    }
    if let Some(png) = clipboard_image_png() {
        let dir = std::env::temp_dir().join("kuajizhongzhuan-paste");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("粘贴的图片_{secs}.png"));
        std::fs::write(&path, png).map_err(|e| e.to_string())?;
        return Ok(ClipboardPayload {
            image: Some(path.display().to_string()),
            ..Default::default()
        });
    }
    Ok(ClipboardPayload {
        text: clipboard_text(),
        ..Default::default()
    })
}

/// macOS:剪贴板里的 PNG 直接用,TIFF(截图、多数 app 复制图片)转成 PNG。
#[cfg(target_os = "macos")]
fn clipboard_image_png() -> Option<Vec<u8>> {
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypePNG,
        NSPasteboardTypeTIFF,
    };
    use objc2_foundation::NSDictionary;
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        if let Some(d) = pb.dataForType(NSPasteboardTypePNG) {
            return Some(d.to_vec());
        }
        let tiff = pb.dataForType(NSPasteboardTypeTIFF)?;
        let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
        let png = rep.representationUsingType_properties(
            NSBitmapImageFileType::PNG,
            &NSDictionary::new(),
        )?;
        Some(png.to_vec())
    }
}

#[cfg(target_os = "macos")]
fn clipboard_text() -> Option<String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    unsafe {
        NSPasteboard::generalPasteboard()
            .stringForType(NSPasteboardTypeString)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    }
}

/// Android:ClipboardManager.getPrimaryClip()。app 在前台时系统允许读取。
/// 条目带 URI(复制的图片/文件)就返回 URI,否则 coerceToText 取文字。
#[cfg(target_os = "android")]
fn android_read_clipboard() -> Result<ClipboardPayload, String> {
    use jni::objects::{JObject, JValue};
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    wry::prelude::dispatch(move |env, activity, _webview| {
        let r: Result<ClipboardPayload, String> = (|| {
            let name = env.new_string("clipboard").map_err(|e| e.to_string())?;
            let cm = env
                .call_method(
                    activity,
                    "getSystemService",
                    "(Ljava/lang/String;)Ljava/lang/Object;",
                    &[JValue::Object(&name.into())],
                )
                .and_then(|v| v.l())
                .map_err(|e| format!("getSystemService: {e}"))?;
            let clip = env
                .call_method(&cm, "getPrimaryClip", "()Landroid/content/ClipData;", &[])
                .and_then(|v| v.l())
                .map_err(|e| format!("getPrimaryClip: {e}"))?;
            let mut out = ClipboardPayload::default();
            if clip.is_null() {
                return Ok(out);
            }
            let count = env
                .call_method(&clip, "getItemCount", "()I", &[])
                .and_then(|v| v.i())
                .map_err(|e| e.to_string())?;
            let resolver = env
                .call_method(activity, "getContentResolver", "()Landroid/content/ContentResolver;", &[])
                .and_then(|v| v.l())
                .map_err(|e| e.to_string())?;
            let mut texts = Vec::new();
            for i in 0..count {
                let item = env
                    .call_method(&clip, "getItemAt", "(I)Landroid/content/ClipData$Item;", &[JValue::Int(i)])
                    .and_then(|v| v.l())
                    .map_err(|e| e.to_string())?;
                let uri = env
                    .call_method(&item, "getUri", "()Landroid/net/Uri;", &[])
                    .and_then(|v| v.l())
                    .map_err(|e| e.to_string())?;
                if !uri.is_null() {
                    let us = env
                        .call_method(&uri, "toString", "()Ljava/lang/String;", &[])
                        .and_then(|v| v.l())
                        .map_err(|e| e.to_string())?;
                    let us: String = env.get_string(&us.into()).map_err(|e| e.to_string())?.into();
                    if us.starts_with("content://") {
                        let mime = env
                            .call_method(&resolver, "getType", "(Landroid/net/Uri;)Ljava/lang/String;", &[JValue::Object(&uri)])
                            .and_then(|v| v.l())
                            .ok()
                            .filter(|o| !o.is_null())
                            .and_then(|o| env.get_string(&o.into()).ok().map(String::from));
                        if env.exception_check().unwrap_or(false) {
                            let _ = env.exception_clear();
                        }
                        let tail = us.rsplit('/').next().unwrap_or("file").to_string();
                        let ext = mime
                            .as_deref()
                            .and_then(|m| mime_guess::get_mime_extensions_str(m))
                            .and_then(|e| e.first())
                            .map(|e| format!(".{e}"))
                            .unwrap_or_default();
                        let name = if tail.contains('.') { tail } else { format!("粘贴的文件_{tail}{ext}") };
                        out.picked.push(AndroidPickItem { uri: us, name: Some(name), size: None, mime_type: mime });
                        continue;
                    }
                }
                let cs = env
                    .call_method(&item, "coerceToText", "(Landroid/content/Context;)Ljava/lang/CharSequence;", &[JValue::Object(activity)])
                    .and_then(|v| v.l())
                    .map_err(|e| e.to_string())?;
                if !cs.is_null() {
                    let js = env
                        .call_method(&cs, "toString", "()Ljava/lang/String;", &[])
                        .and_then(|v| v.l())
                        .map_err(|e| e.to_string())?;
                    let t: String = env.get_string(&JObject::from(js).into()).map_err(|e| e.to_string())?.into();
                    if !t.is_empty() {
                        texts.push(t);
                    }
                }
            }
            if out.picked.is_empty() && !texts.is_empty() {
                out.text = Some(texts.join("\n"));
            }
            Ok(out)
        })();
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        let _ = tx.send(r);
    });
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "读取剪贴板超时".to_string())?
}

// Windows:网页视图自己能读到剪贴板里的图片和文字(前端走
// navigator.clipboard / paste 事件),这里不重复实现。
#[cfg(not(target_os = "macos"))]
fn clipboard_image_png() -> Option<Vec<u8>> {
    None
}
#[cfg(not(target_os = "macos"))]
fn clipboard_text() -> Option<String> {
    None
}

/// 前端从网页剪贴板拿到的图片字节(Windows/Android),存成临时文件再按文件发送。
#[tauri::command]
pub async fn save_pasted_image(data_base64: String, ext: Option<String>) -> Result<String, String> {
    let raw = data_base64.rsplit(',').next().unwrap_or_default();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| format!("图片数据无效: {e}"))?;
    if bytes.is_empty() {
        return Err("剪贴板图片为空".to_string());
    }
    let ext = ext
        .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()) && e.len() <= 5)
        .unwrap_or_else(|| "png".to_string());
    let dir = std::env::temp_dir().join("kuajizhongzhuan-paste");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("粘贴的图片_{secs}.{ext}"));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

#[tauri::command]
pub async fn refresh_devices(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let interface_ips =
        localsend::util::interface::local_interface_addresses(&InterfaceFilter::default())
            .unwrap_or_default();
    state
        .discovery
        .discover_staged(
            Vec::new(),
            interface_ips,
            state.identity.port,
            ProtocolType::Https,
            Duration::from_secs(1),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Add a device manually by IP (and optional port, defaulting to LocalSend's
/// standard port). This bypasses multicast: the probe is a unicast HTTP
/// register request, which also works when multicast is filtered out.
#[tauri::command]
pub async fn add_device_ip(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    ip: String,
    port: Option<u16>,
) -> Result<(), String> {
    let port = port.unwrap_or(DEFAULT_PORT);
    let found = state
        .discovery
        .discover(&ip, port, ProtocolType::Https)
        .await
        .map_err(|e| error_chain(&e))?;
    match found {
        Some(_) => {
            emit_devices(&app, &state.discovery, &state.paired);
            Ok(())
        }
        None => Err("对方是自己(指纹相同),无需添加".to_string()),
    }
}

// ---------- Android 系统文件选择器 ----------
// tauri-plugin-dialog 在 Android 上只回裸 URI、不能选目录,这里走自带的
// PickerPlugin.kt(ACTION_OPEN_DOCUMENT / ACTION_OPEN_DOCUMENT_TREE)。

#[cfg(target_os = "android")]
pub struct AndroidPicker<R: tauri::Runtime>(pub tauri::plugin::PluginHandle<R>);

#[cfg(target_os = "android")]
pub fn picker_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("picker")
        .setup(|app, api| {
            let handle =
                api.register_android_plugin("com.kuajizhongzhuan.desktop", "PickerPlugin")?;
            app.manage(AndroidPicker(handle));
            Ok(())
        })
        .build()
}

#[cfg(target_os = "android")]
#[derive(serde::Deserialize)]
struct PickResult {
    items: Vec<AndroidPickItem>,
}

#[cfg(target_os = "android")]
async fn android_pick(app: &AppHandle, cmd: &str) -> Result<Vec<AndroidPickItem>, String> {
    let picker = app.state::<AndroidPicker<tauri::Wry>>();
    let r: PickResult = picker
        .0
        .run_mobile_plugin_async(cmd, serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(r.items)
}

/// Android: 选多个文件,返回 uri/name/size/mimeType。桌面端走 dialog 插件,不用这个。
#[tauri::command]
pub async fn pick_files(app: AppHandle) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "android")]
    {
        return android_pick(&app, "pickFiles")
            .await
            .map(|v| serde_json::to_value(v).unwrap_or_default());
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err("仅 Android 使用".to_string())
    }
}

/// Android: 选一个文件夹,递归列出其中所有文件;name 为 "文件夹/相对路径"。
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "android")]
    {
        return android_pick(&app, "pickFolder")
            .await
            .map(|v| serde_json::to_value(v).unwrap_or_default());
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err("仅 Android 使用".to_string())
    }
}

/// "a: b: c" — reqwest's top-level message ("error sending request") hides the
/// actual cause (connection refused / TLS / timeout), so show the whole chain.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        let msg = s.to_string();
        if !out.contains(&msg) {
            out.push_str(": ");
            out.push_str(&msg);
        }
        src = s.source();
    }
    out
}

// ---------- 聊天记录持久化 ----------
// 前端把消息列表序列化成 JSON 交给后端,存成配置目录下的 history.json。
// 写入先落临时文件再 rename,避免写到一半崩溃留下坏文件。

fn history_path() -> PathBuf {
    config_dir().join("history.json")
}

#[tauri::command]
pub async fn load_history() -> Result<String, String> {
    match std::fs::read_to_string(history_path()) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("[]".to_string()),
        Err(e) => Err(format!("读取聊天记录失败: {e}")),
    }
}

#[tauri::command]
pub async fn save_history(json: String) -> Result<(), String> {
    serde_json::from_str::<serde_json::Value>(&json).map_err(|e| format!("记录格式错误: {e}"))?;
    let path = history_path();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("保存聊天记录失败: {e}"))
}

#[tauri::command]
pub async fn clear_history() -> Result<(), String> {
    match std::fs::remove_file(history_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// ---------- 检查更新 ----------
// 版本清单放在 GitHub 仓库 main 分支的 `update/latest.json`,发版时改它即可:
// { "version": "0.2.0", "notes": "...", "mac": url, "windows": url, "android": url }
// 先走 jsDelivr(国内可达),失败再走 raw.githubusercontent。

const UPDATE_URLS: &[&str] = &[
    "https://cdn.jsdelivr.net/gh/pan185618-cloud/kzj-transfer@main/update/latest.json",
    "https://raw.githubusercontent.com/pan185618-cloud/kzj-transfer/main/update/latest.json",
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current: String,
    latest: String,
    available: bool,
    notes: String,
    /// 本平台的下载地址;没有时为空,UI 退回打开发布页
    url: String,
    page: String,
}

fn version_tuple(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<UpdateInfo, String> {
    let current = app.package_info().version.to_string();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let mut last_err = String::new();
    let mut manifest: Option<serde_json::Value> = None;
    for url in UPDATE_URLS {
        // jsDelivr 有缓存,带时间戳参数只对 raw 有用;两边都无害
        let u = format!("{url}?t={}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() / 600)
            .unwrap_or(0));
        match client.get(&u).send().await {
            Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
                Ok(v) => {
                    manifest = Some(v);
                    break;
                }
                Err(e) => last_err = e.to_string(),
            },
            Ok(r) => last_err = format!("HTTP {}", r.status()),
            Err(e) => last_err = error_chain(&e),
        }
    }
    let m = manifest.ok_or_else(|| format!("检查更新失败: {last_err}"))?;
    let latest = m["version"].as_str().unwrap_or("").to_string();
    if latest.is_empty() {
        return Err("更新清单缺少 version".to_string());
    }
    let platform = if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "mac"
    };
    Ok(UpdateInfo {
        available: version_tuple(&latest) > version_tuple(&current),
        current,
        notes: m["notes"].as_str().unwrap_or("").to_string(),
        url: m[platform].as_str().unwrap_or("").to_string(),
        page: m["page"]
            .as_str()
            .unwrap_or("https://github.com/pan185618-cloud/kzj-transfer/releases")
            .to_string(),
        latest,
    })
}

/// Android 没有浏览器式的 open::that,用 ACTION_VIEW 交给系统浏览器下载 APK。
/// 桌面直接打开链接。
#[tauri::command]
pub async fn open_update_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("不是有效链接".to_string());
    }
    #[cfg(target_os = "android")]
    {
        return android_view_url(url);
    }
    #[cfg(not(target_os = "android"))]
    {
        open::that_detached(&url).map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "android")]
fn android_view_url(url: String) -> Result<(), String> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    wry::prelude::dispatch(move |env, activity, _webview| {
        let r: Result<(), String> = (|| {
            let s = env.new_string(&url).map_err(|e| e.to_string())?;
            let uri = env
                .call_static_method(
                    "android/net/Uri",
                    "parse",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[jni::objects::JValue::Object(&s.into())],
                )
                .and_then(|v| v.l())
                .map_err(|e| e.to_string())?;
            let action = env
                .new_string("android.intent.action.VIEW")
                .map_err(|e| e.to_string())?;
            let intent = env
                .new_object(
                    "android/content/Intent",
                    "(Ljava/lang/String;Landroid/net/Uri;)V",
                    &[
                        jni::objects::JValue::Object(&action.into()),
                        jni::objects::JValue::Object(&uri),
                    ],
                )
                .map_err(|e| e.to_string())?;
            env.call_method(
                activity,
                "startActivity",
                "(Landroid/content/Intent;)V",
                &[jni::objects::JValue::Object(&intent)],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        let _ = tx.send(r);
    });
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "打开浏览器超时".to_string())?
}
