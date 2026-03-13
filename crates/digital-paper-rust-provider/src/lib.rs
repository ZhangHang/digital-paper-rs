use base64::Engine;
use digital_paper_domain::{
    AdvancedCapabilities, BatteryStatus, ConnectionLog, DeviceStatus, DeviceSummary, RemoteEntry,
    RemoteEntryType, StorageStatus, TransportKind, WifiNetwork,
};
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use openssl::{hash::MessageDigest, pkey::PKey, sign::Signer};
use parking_lot::Mutex;
use pbkdf2::pbkdf2_hmac;
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rand::{rngs::OsRng, RngCore};
use reqwest::blocking::{multipart, Client};
use reqwest::header::{HeaderMap, CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    io::Write,
    net::Ipv4Addr,
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;
use walkdir::WalkDir;

const QUERY_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC;
const DH_GROUP14_PRIME_HEX: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AACAA68FFFFFFFFFFFFFFFF";

#[derive(Debug, Error, Clone)]
pub enum RustProviderError {
    #[error("device not found")]
    DeviceNotFound,
    #[error("pairing required")]
    PairingRequired,
    #[error("invalid pairing pin")]
    PinInvalid,
    #[error("authentication failed")]
    AuthFailed,
    #[error("transport unreachable")]
    TransportUnreachable,
    #[error("operation not implemented in rust provider: {0}")]
    NotImplemented(&'static str),
    #[error("conflict detected")]
    ConflictDetected,
    #[error("unsupported over usb")]
    UnsupportedOverUsb,
    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Default)]
struct RustState {
    selected_addr: Option<String>,
    session: Option<RustSession>,
    pending_pairing: Option<PendingPairing>,
    logs: Vec<ConnectionLog>,
}

#[derive(Clone)]
struct RustSession {
    addr: String,
    cookie: String,
    client: Client,
}

#[derive(Clone)]
struct PendingPairing {
    addr: String,
    client: Client,
    reg_base: String,
    n1: Vec<u8>,
    n2: Vec<u8>,
    auth_key: Vec<u8>,
    key_wrap_key: Vec<u8>,
    yb: Vec<u8>,
    ya: Vec<u8>,
    e_hash: Vec<u8>,
    m3hmac: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPendingPairing {
    addr: String,
    n1: Vec<u8>,
    n2: Vec<u8>,
    auth_key: Vec<u8>,
    key_wrap_key: Vec<u8>,
    yb: Vec<u8>,
    ya: Vec<u8>,
    e_hash: Vec<u8>,
    m3hmac: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct ResolveEntry {
    entry_id: String,
    entry_type: String,
}

pub struct RustNativeProvider {
    state: Mutex<RustState>,
}

impl Default for RustNativeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl RustNativeProvider {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RustState::default()),
        }
    }

    pub fn discover_devices(&self) -> std::result::Result<Vec<DeviceSummary>, RustProviderError> {
        let mut devices = Vec::new();
        let credentials = read_credentials_files().unwrap_or((None, None));
        let addrs = candidate_addrs();
        self.log(format!("Discovery candidates: {}", addrs.join(", ")));
        let probes = probe_addrs_concurrently(&addrs, 24, Duration::from_millis(750));
        for addr in addrs {
            let detected_name = probes.get(&addr).cloned().flatten();
            let is_usb = addr.starts_with("172.25.47.") || addr.starts_with("172.20.");
            if let Some(detected_name) = detected_name {
                let paired = match &credentials {
                    (Some(client_id), Some(private_key_pem)) => self
                        .authenticate_session(&addr, client_id.clone(), private_key_pem.clone())
                        .is_ok(),
                    _ => false,
                };
                devices.push(DeviceSummary {
                    id: format!("rust-{addr}"),
                    serial: None,
                    name: detected_name,
                    reachable_addrs: vec![addr.clone()],
                    transport_kinds: if is_usb {
                        vec![TransportKind::UsbNetwork, TransportKind::Wifi]
                    } else {
                        vec![TransportKind::Wifi]
                    },
                    paired,
                });
            } else {
                self.log(format!("No device response from {addr}"));
            }
        }
        if devices.is_empty() {
            self.log("Discovery finished without any device candidates".into());
            return Err(RustProviderError::DeviceNotFound);
        }
        let _ = save_known_addrs(
            devices
                .iter()
                .flat_map(|d| d.reachable_addrs.iter().cloned()),
        );
        self.log(format!("Discovered {} device candidates", devices.len()));
        Ok(devices)
    }

    pub fn list_all_entries(&self) -> std::result::Result<Vec<RemoteEntry>, RustProviderError> {
        let session = self.require_session()?;
        let value = self.get_json(
            &session,
            "/documents2?entry_type=all&fields=entry_path,entry_type,modified_date,file_size",
        )?;
        let entries = value
            .get("entry_list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(entries.iter().map(map_remote_entry).collect())
    }

    pub fn list_document_entries(
        &self,
    ) -> std::result::Result<Vec<RemoteEntry>, RustProviderError> {
        let entries = self.list_all_entries()?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.entry_type == RemoteEntryType::Document)
            .collect())
    }

    pub fn connect(
        &self,
        addr: Option<String>,
        _serial: Option<String>,
        transport_hint: Option<TransportKind>,
    ) -> std::result::Result<DeviceSummary, RustProviderError> {
        let target = if let Some(addr) = addr {
            addr
        } else {
            self.discover_devices()?
                .into_iter()
                .next()
                .and_then(|d| d.reachable_addrs.first().cloned())
                .ok_or(RustProviderError::DeviceNotFound)?
        };

        if !probe_addr(&target) && target != "digitalpaper.local" {
            return Err(RustProviderError::TransportUnreachable);
        }

        let mut paired = false;
        let mut session = None;
        if let (Some(client_id), Some(private_key_pem)) =
            read_credentials_files().unwrap_or((None, None))
        {
            if let Ok(s) = self.authenticate_session(&target, client_id, private_key_pem) {
                paired = true;
                session = Some(s);
            }
        }

        let device = DeviceSummary {
            id: format!("rust-{target}"),
            serial: None,
            name: if target == "digitalpaper.local" {
                "Sony Digital Paper".to_string()
            } else {
                format!("Sony Digital Paper ({target})")
            },
            reachable_addrs: vec![target.clone()],
            transport_kinds: vec![transport_hint.unwrap_or(TransportKind::Wifi)],
            paired,
        };
        let mut state = self.state.lock();
        state.selected_addr = Some(target.clone());
        state.session = session;
        state.pending_pairing = None;
        drop(state);
        let _ = save_known_addrs([target.clone()]);
        self.log(format!("Connected to {target} (paired={paired})"));
        Ok(device)
    }

    pub fn pair(&self, pin: String) -> std::result::Result<DeviceSummary, RustProviderError> {
        if pin.len() != 8 || !pin.chars().all(|c| c.is_ascii_digit()) {
            return Err(RustProviderError::PinInvalid);
        }
        let pending = {
            let state = self.state.lock();
            state.pending_pairing.clone()
        };
        let pending = if let Some(pending) = pending {
            pending
        } else if let Some(pending) = read_pending_pairing()? {
            pending
        } else {
            self.begin_pair()?;
            self.state
                .lock()
                .pending_pairing
                .clone()
                .ok_or(RustProviderError::PairingRequired)?
        };
        let addr = pending.addr.clone();
        let (client_id, private_key) = self.finish_pairing(pending, &pin)?;
        persist_credentials(&client_id, &private_key)?;
        let session = self.authenticate_session(&addr, client_id, private_key)?;
        {
            let mut state = self.state.lock();
            state.session = Some(session);
            state.pending_pairing = None;
        }
        let _ = clear_pending_pairing();
        self.log(format!("Pair succeeded for {addr}"));
        Ok(DeviceSummary {
            id: format!("rust-{addr}"),
            serial: None,
            name: if addr == "digitalpaper.local" {
                "Sony Digital Paper".to_string()
            } else {
                format!("Sony Digital Paper ({addr})")
            },
            reachable_addrs: vec![addr],
            transport_kinds: vec![TransportKind::Wifi],
            paired: true,
        })
    }

    pub fn begin_pair(&self) -> std::result::Result<(), RustProviderError> {
        let addr = self
            .state
            .lock()
            .selected_addr
            .clone()
            .ok_or(RustProviderError::PairingRequired)?;
        let pending = self.begin_pair_with_retry(&addr)?;
        save_pending_pairing(&pending)?;
        self.state.lock().pending_pairing = Some(pending);
        self.log(format!("PIN requested for {addr}"));
        Ok(())
    }

    pub fn begin_pair_for_address(&self, addr: &str) -> std::result::Result<(), RustProviderError> {
        let pending = self.begin_pair_with_retry(addr)?;
        save_pending_pairing(&pending)?;
        self.state.lock().pending_pairing = Some(pending);
        self.log(format!("PIN requested for {addr}"));
        Ok(())
    }

    fn begin_pair_with_retry(
        &self,
        addr: &str,
    ) -> std::result::Result<PendingPairing, RustProviderError> {
        let mut last_error = None;
        for attempt in 1..=5 {
            match self.begin_pair_for_addr(addr) {
                Ok(pending) => return Ok(pending),
                Err(err) if pair_begin_retryable(&err) && attempt < 5 => {
                    self.log(format!(
                        "pair begin attempt {attempt} failed for {addr}: {err}; retrying"
                    ));
                    last_error = Some(err);
                    thread::sleep(Duration::from_secs(2));
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_error.unwrap_or_else(|| RustProviderError::Internal("pair begin failed".into())))
    }

    pub fn list_entries(
        &self,
        path: String,
    ) -> std::result::Result<Vec<RemoteEntry>, RustProviderError> {
        let session = self.require_session()?;
        let endpoint =
            "/documents2?entry_type=all&fields=entry_path,entry_type,modified_date,file_size";
        let value = self.get_json(&session, endpoint)?;
        let entries = value
            .get("entry_list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut result = Vec::new();
        for entry in entries {
            let Some(entry_path) = entry.get("entry_path").and_then(Value::as_str) else {
                continue;
            };
            if entry_path == path {
                continue;
            }
            if parent_path(entry_path) != path.trim_end_matches('/') {
                continue;
            }
            result.push(map_remote_entry(&entry));
        }
        Ok(result)
    }

    pub fn upload(
        &self,
        local_path: String,
        remote_path: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let local = PathBuf::from(&local_path);
        let data = fs::read(&local)
            .map_err(|e| RustProviderError::Internal(format!("read local file failed: {e}")))?;
        let mut target_path = remote_path.clone();
        if self.path_is_folder(&session, &remote_path)? {
            let Some(name) = local.file_name().and_then(|v| v.to_str()) else {
                return Err(RustProviderError::Internal("invalid local filename".into()));
            };
            target_path = format!("{}/{}", remote_path.trim_end_matches('/'), name);
        }

        let doc_id = match self.resolve_object(&session, &target_path) {
            Ok(obj) if obj.entry_type == "document" => obj.entry_id,
            _ => {
                let parent = parent_path(&target_path);
                self.new_folder(&session, parent)?;
                let parent_id = self.resolve_object(&session, parent)?.entry_id;
                let created = self.post_json(
                    &session,
                    "/documents2",
                    &json!({
                        "file_name": basename(&target_path),
                        "parent_folder_id": parent_id,
                        "document_source": ""
                    }),
                )?;
                created
                    .get("document_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        RustProviderError::Internal("missing document_id from create".into())
                    })?
                    .to_string()
            }
        };

        let part = multipart::Part::bytes(data).file_name(basename(&target_path).to_string());
        let form = multipart::Form::new().part("file", part);
        self.put_multipart(&session, &format!("/documents/{doc_id}/file"), form)?;
        self.log(format!("Uploaded {local_path} -> {target_path}"));
        Ok(())
    }

    pub fn download(
        &self,
        remote_path: String,
        local_path: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let object = self.resolve_object(&session, &remote_path)?;
        if object.entry_type != "document" {
            return Err(RustProviderError::Internal(
                "download target is not document".into(),
            ));
        }
        let bytes = self.get_bytes(&session, &format!("/documents/{}/file", object.entry_id))?;
        let local = PathBuf::from(&local_path);
        if let Some(parent) = local.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| {
                    RustProviderError::Internal(format!("create local folder failed: {e}"))
                })?;
            }
        }
        let mut file = fs::File::create(&local)
            .map_err(|e| RustProviderError::Internal(format!("create file failed: {e}")))?;
        file.write_all(&bytes)
            .map_err(|e| RustProviderError::Internal(format!("write file failed: {e}")))?;
        self.log(format!("Downloaded {remote_path} -> {local_path}"));
        Ok(())
    }

    pub fn delete(&self, path: String) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let object = self.resolve_object(&session, &path)?;
        if object.entry_type == "folder" {
            self.delete_empty(&session, &format!("/folders/{}", object.entry_id))?;
        } else {
            self.delete_empty(&session, &format!("/documents/{}", object.entry_id))?;
        }
        self.log(format!("Deleted {path}"));
        Ok(())
    }

    pub fn move_entry(
        &self,
        src: String,
        dst: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let src_obj = self.resolve_object(&session, &src)?;
        let (parent_folder_id, new_name) = match self.resolve_object(&session, &dst) {
            Ok(dst_obj) if dst_obj.entry_type == "folder" => (dst_obj.entry_id, None),
            _ => {
                let parent = parent_path(&dst);
                let parent_id = self.resolve_object(&session, parent)?.entry_id;
                (parent_id, Some(basename(&dst).to_string()))
            }
        };
        let mut payload = json!({ "parent_folder_id": parent_folder_id });
        if let Some(name) = new_name {
            let key = if src_obj.entry_type == "folder" {
                "folder_name"
            } else {
                "file_name"
            };
            payload[key] = Value::String(name);
        }
        let endpoint = if src_obj.entry_type == "folder" {
            format!("/folders/{}", src_obj.entry_id)
        } else {
            format!("/documents/{}", src_obj.entry_id)
        };
        self.put_json(&session, &endpoint, &payload)?;
        self.log(format!("Moved {src} -> {dst}"));
        Ok(())
    }

    pub fn rename_entry(
        &self,
        path: String,
        new_name: String,
    ) -> std::result::Result<(), RustProviderError> {
        let dst = format!("{}/{}", parent_path(&path), new_name);
        self.move_entry(path, dst)
    }

    pub fn create_folder(&self, path: String) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        self.new_folder(&session, &path)?;
        self.log(format!("Created folder {path}"));
        Ok(())
    }

    pub fn copy_entry(&self, src: String, dst: String) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let old_id = self.resolve_object(&session, &src)?.entry_id;
        let (parent_id, new_name) = match self.resolve_object(&session, &dst) {
            Ok(obj) if obj.entry_type == "folder" => (obj.entry_id, None),
            Ok(_) => {
                let parent = parent_path(&dst);
                let parent_id = self.resolve_object(&session, parent)?.entry_id;
                (parent_id, Some(basename(&dst).to_string()))
            }
            Err(RustProviderError::DeviceNotFound) => {
                let parent = parent_path(&dst);
                self.new_folder(&session, parent)?;
                let parent_id = self.resolve_object(&session, parent)?.entry_id;
                (parent_id, Some(basename(&dst).to_string()))
            }
            Err(err) => return Err(err),
        };
        let mut payload = json!({ "parent_folder_id": parent_id });
        if let Some(name) = new_name {
            payload["file_name"] = Value::String(name);
        }
        self.post_json(&session, &format!("/documents/{old_id}/copy"), &payload)?;
        self.log(format!("Copied {src} -> {dst}"));
        Ok(())
    }

    pub fn path_exists(&self, path: String) -> std::result::Result<bool, RustProviderError> {
        let session = self.require_session()?;
        match self.resolve_object(&session, &path) {
            Ok(_) => Ok(true),
            Err(RustProviderError::DeviceNotFound) => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub fn path_is_folder_public(
        &self,
        path: String,
    ) -> std::result::Result<bool, RustProviderError> {
        let session = self.require_session()?;
        self.path_is_folder(&session, &path)
    }

    pub fn device_info(&self) -> std::result::Result<DeviceStatus, RustProviderError> {
        let session = self.require_session()?;
        let battery = self.get_json(&session, "/system/status/battery")?;
        let storage = self.get_json(&session, "/system/status/storage")?;
        let firmware = self.get_json(&session, "/system/status/firmware_version")?;
        let owner = self.get_json(&session, "/system/configs/owner")?;
        let wifi = self.get_json(&session, "/system/configs/wifi")?;
        Ok(DeviceStatus {
            battery: BatteryStatus {
                level_percent: parse_battery_level(&battery),
                charging: matches!(
                    battery.get("status").and_then(Value::as_str),
                    Some("charge" | "full")
                ) || battery.get("plugged").and_then(Value::as_str) == Some("usb"),
            },
            storage: StorageStatus {
                total_bytes: storage.get("total").and_then(Value::as_u64),
                free_bytes: storage.get("free").and_then(Value::as_u64),
            },
            firmware_version: firmware
                .get("value")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            owner: owner
                .get("value")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            wifi_enabled: wifi.get("value").and_then(Value::as_str) == Some("on"),
        })
    }

    pub fn register_info(
        &self,
        addr: Option<String>,
    ) -> std::result::Result<Value, RustProviderError> {
        let target = self.resolve_target_addr(addr)?;
        let client = self.registration_client(Duration::from_secs(10))?;
        let response = client
            .get(format!("http://{target}:8080/register/information"))
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("register info read failed: {e}")))?;
        if !status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "register information http error: status={status} body={text}"
            )));
        }
        serde_json::from_str(&text)
            .map_err(|e| RustProviderError::Internal(format!("register info parse failed: {e}; body={text}")))
    }

    pub fn battery_info(&self) -> std::result::Result<Value, RustProviderError> {
        let session = self.require_session()?;
        self.get_json(&session, "/system/status/battery")
    }

    pub fn firmware_version(&self) -> std::result::Result<String, RustProviderError> {
        let session = self.require_session()?;
        let value = self.get_json(&session, "/system/status/firmware_version")?;
        value
            .get("value")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| RustProviderError::Internal("missing firmware version".into()))
    }

    pub fn mac_address(&self) -> std::result::Result<String, RustProviderError> {
        let session = self.require_session()?;
        let value = self.get_json(&session, "/system/status/mac_address")?;
        value
            .get("value")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| RustProviderError::Internal("missing mac address".into()))
    }

    pub fn api_version(
        &self,
        addr: Option<String>,
    ) -> std::result::Result<String, RustProviderError> {
        let target = self.resolve_target_addr(addr)?;
        let client = self.registration_client(Duration::from_secs(10))?;
        let response = client
            .get(format!("http://{target}:8080/api_version"))
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("api version read failed: {e}")))?;
        if !status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "api version http error: status={status} body={text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| RustProviderError::Internal(format!("api version parse failed: {e}; body={text}")))?;
        value
            .get("value")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| RustProviderError::Internal("missing api version".into()))
    }

    pub fn list_wifi(&self) -> std::result::Result<Vec<WifiNetwork>, RustProviderError> {
        let session = self.require_session()?;
        let value = self.get_json(&session, "/system/configs/wifi_accesspoints")?;
        let mut out = Vec::new();
        for ap in value
            .get("aplist")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let ssid = ap
                .get("ssid")
                .and_then(Value::as_str)
                .and_then(decode_base64_text)
                .unwrap_or_default();
            let security = ap
                .get("security")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            out.push(WifiNetwork {
                id: Some(format!("{ssid}|{security}")),
                ssid,
                security,
                connected: ap.get("connected").and_then(Value::as_str) == Some("true")
                    || ap.get("connected").and_then(Value::as_str) == Some("on")
                    || ap.get("connected").and_then(Value::as_bool) == Some(true),
            });
        }
        Ok(out)
    }

    pub fn scan_wifi(&self) -> std::result::Result<Vec<WifiNetwork>, RustProviderError> {
        let session = self.require_session()?;
        let value = self.post_json(
            &session,
            "/system/controls/wifi_accesspoints/scan",
            &json!({}),
        )?;
        let mut out = Vec::new();
        for ap in value
            .get("aplist")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let ssid = ap
                .get("ssid")
                .and_then(Value::as_str)
                .and_then(decode_base64_text)
                .unwrap_or_default();
            let security = ap
                .get("security")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            out.push(WifiNetwork {
                id: Some(format!("{ssid}|{security}")),
                ssid,
                security,
                connected: false,
            });
        }
        Ok(out)
    }

    pub fn add_wifi(
        &self,
        ssid: String,
        security: String,
        passwd: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        self.put_json(
            &session,
            "/system/controls/wifi_accesspoints/register",
            &json!({
                "ssid": base64::engine::general_purpose::STANDARD.encode(ssid.as_bytes()),
                "security": security,
                "passwd": passwd,
                "dhcp": "true",
                "static_address": "",
                "gateway": "",
                "network_mask": "",
                "dns1": "",
                "dns2": "",
                "proxy": "false"
            }),
        )?;
        self.log("Wi-Fi network added".into());
        Ok(())
    }

    pub fn remove_wifi(
        &self,
        ssid: String,
        security: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        self.delete_empty(
            &session,
            &format!("/system/configs/wifi_accesspoints/{}/{}", ssid, security),
        )?;
        self.log("Wi-Fi network removed".into());
        Ok(())
    }

    pub fn toggle_wifi(&self, enabled: bool) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let value = if enabled { "on" } else { "off" };
        self.put_json(&session, "/system/configs/wifi", &json!({ "value": value }))?;
        self.log(format!("Wi-Fi toggled {value}"));
        Ok(())
    }

    pub fn get_config(&self) -> std::result::Result<Value, RustProviderError> {
        let session = self.require_session()?;
        self.get_json(&session, "/system/configs/")
    }

    pub fn get_config_value(&self, key: &str) -> std::result::Result<Value, RustProviderError> {
        let session = self.require_session()?;
        self.get_json(&session, &format!("/system/configs/{key}"))
    }

    pub fn set_config_value(
        &self,
        key: &str,
        value: Value,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        self.put_json(&session, &format!("/system/configs/{key}"), &value)?;
        self.log(format!("Config updated for {key}"));
        Ok(())
    }

    pub fn set_datetime_now(&self) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let now = chrono_like_now_utc();
        self.put_json(&session, "/system/configs/datetime", &json!({ "value": now }))?;
        self.log("Device datetime updated".into());
        Ok(())
    }

    pub fn list_templates(&self) -> std::result::Result<Value, RustProviderError> {
        let session = self.require_session()?;
        self.get_json(&session, "/viewer/configs/note_templates")
    }

    pub fn upload_template(
        &self,
        local_path: String,
        remote_path: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let filename = basename(&remote_path).to_string();
        let created = self.post_json(
            &session,
            "/viewer/configs/note_templates",
            &json!({
                "templateName": filename,
                "document_source": ""
            }),
        )?;
        let template_id = created
            .get("note_template_id")
            .and_then(Value::as_str)
            .ok_or_else(|| RustProviderError::Internal("missing note_template_id".into()))?;
        let data = fs::read(&local_path)
            .map_err(|e| RustProviderError::Internal(format!("read local template failed: {e}")))?;
        let part = multipart::Part::bytes(data).file_name(filename.clone());
        let form = multipart::Form::new().part("file", part);
        self.put_multipart(
            &session,
            &format!("/viewer/configs/note_templates/{template_id}/file"),
            form,
        )?;
        self.log(format!("Uploaded template {local_path} -> {remote_path}"));
        Ok(())
    }

    pub fn delete_template(
        &self,
        template_name: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let templates = self.list_templates()?;
        let template_id = templates
            .get("template_list")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|item| item.get("template_name").and_then(Value::as_str) == Some(template_name.as_str()))
            .and_then(|item| item.get("note_template_id").and_then(Value::as_str))
            .ok_or(RustProviderError::DeviceNotFound)?;
        self.delete_empty(
            &session,
            &format!("/viewer/configs/note_templates/{template_id}"),
        )?;
        self.log(format!("Deleted template {template_name}"));
        Ok(())
    }

    pub fn display_document(
        &self,
        document_id: String,
        page: u32,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        self.put_json(
            &session,
            "/viewer/controls/open2",
            &json!({ "document_id": document_id, "page": page }),
        )?;
        self.log(format!("Display requested for document {document_id} page {page}"));
        Ok(())
    }

    pub fn take_screenshot(&self) -> std::result::Result<Vec<u8>, RustProviderError> {
        let session = self.require_session()?;
        self.get_bytes(&session, "/system/controls/screen_shot2?query=jpeg")
    }

    pub fn ping(&self) -> std::result::Result<bool, RustProviderError> {
        let session = self.require_session()?;
        let response = session
            .client
            .get(format!("{}{}", base_url(&session.addr), "/ping"))
            .header(COOKIE, format!("Credentials={}", session.cookie))
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        Ok(response.status().is_success())
    }

    pub fn update_firmware(
        &self,
        local_path: String,
    ) -> std::result::Result<(), RustProviderError> {
        let session = self.require_session()?;
        let data = fs::read(&local_path)
            .map_err(|e| RustProviderError::Internal(format!("read firmware failed: {e}")))?;
        let part = multipart::Part::bytes(data).file_name("FwUpdater.pkg");
        let form = multipart::Form::new().part("file", part);
        self.put_multipart(&session, "/system/controls/update_firmware/file", form)?;
        let precheck = self.get_json(&session, "/system/controls/update_firmware/precheck")?;
        let battery_ok = precheck.get("battery").and_then(Value::as_str) == Some("ok");
        let image_ok = precheck.get("image_file").and_then(Value::as_str) == Some("ok");
        if !battery_ok || !image_ok {
            return Err(RustProviderError::Internal(format!(
                "firmware precheck failed: {precheck}"
            )));
        }
        self.put_json(&session, "/system/controls/update_firmware", &json!({}))?;
        self.log(format!("Firmware update uploaded from {local_path}"));
        Ok(())
    }

    pub fn sync_folder(
        &self,
        local_path: String,
        remote_path: String,
    ) -> std::result::Result<serde_json::Value, RustProviderError> {
        let session = self.require_session()?;
        self.new_folder(&session, &remote_path)?;
        let all_remote = self
            .get_json(
                &session,
                "/documents2?entry_type=all&fields=entry_path,entry_type,modified_date,file_size",
            )?
            .get("entry_list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let remote_docs: std::collections::HashSet<String> = all_remote
            .into_iter()
            .filter_map(|v| {
                let ty = v.get("entry_type").and_then(Value::as_str).unwrap_or("");
                if ty != "document" {
                    return None;
                }
                v.get("entry_path")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .collect();

        let mut uploaded = Vec::new();
        let mut skipped = Vec::new();
        let mut conflicts = Vec::new();
        for entry in WalkDir::new(&local_path)
            .into_iter()
            .filter_map(|v| v.ok())
            .filter(|v| v.file_type().is_file())
        {
            let rel = entry.path().strip_prefix(&local_path).map_err(|e| {
                RustProviderError::Internal(format!("sync relative path failed: {e}"))
            })?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            let remote_target = format!("{}/{}", remote_path.trim_end_matches('/'), rel);
            if remote_docs.contains(&remote_target) {
                skipped.push(remote_target.clone());
                conflicts.push(remote_target);
                continue;
            }
            self.upload(
                entry.path().to_string_lossy().to_string(),
                remote_target.clone(),
            )?;
            uploaded.push(remote_target);
        }

        let result = json!({
            "uploaded": uploaded,
            "skipped": skipped,
            "conflicts": conflicts
        });
        self.log("Sync completed".into());
        Ok(result)
    }

    pub fn import_credentials(
        &self,
        sony_app_folder: Option<String>,
        device_id_path: Option<String>,
        private_key_path: Option<String>,
    ) -> std::result::Result<DeviceSummary, RustProviderError> {
        let (device_src, key_src) =
            if let (Some(device), Some(key)) = (device_id_path, private_key_path) {
                (PathBuf::from(device), PathBuf::from(key))
            } else if let Some(folder) = sony_app_folder {
                find_credentials_in_folder(PathBuf::from(folder))?
            } else {
                return Err(RustProviderError::Internal(
                    "provide sony_app_folder or both credential file paths".into(),
                ));
            };

        let config = home_config_dir();
        fs::create_dir_all(&config)
            .map_err(|e| RustProviderError::Internal(format!("create config dir failed: {e}")))?;
        fs::copy(&device_src, default_device_id_path())
            .map_err(|e| RustProviderError::Internal(format!("copy deviceid failed: {e}")))?;
        fs::copy(&key_src, default_private_key_path())
            .map_err(|e| RustProviderError::Internal(format!("copy privatekey failed: {e}")))?;

        let addr = self
            .state
            .lock()
            .selected_addr
            .clone()
            .unwrap_or_else(|| "digitalpaper.local".to_string());
        self.connect(Some(addr), None, None)
    }

    pub fn detect_advanced_capabilities(
        &self,
    ) -> std::result::Result<AdvancedCapabilities, RustProviderError> {
        Ok(AdvancedCapabilities {
            apk_install_supported: false,
            apk_install_message: Some(
                "Digital Paper devices do not expose APK install in the known HTTP API".into(),
            ),
        })
    }

    pub fn read_logs(&self) -> std::result::Result<Vec<ConnectionLog>, RustProviderError> {
        Ok(self.state.lock().logs.clone())
    }

    fn log(&self, message: String) {
        let ts = format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        let mut state = self.state.lock();
        let id = format!("{}", state.logs.len() + 1);
        state.logs.push(ConnectionLog {
            id,
            timestamp: ts,
            message: message.clone(),
        });
        if state.logs.len() > 200 {
            let keep_from = state.logs.len() - 200;
            state.logs.drain(0..keep_from);
        }
        drop(state);
        append_debug_log("provider", &message);
    }

    fn require_session(&self) -> std::result::Result<RustSession, RustProviderError> {
        let mut state = self.state.lock();
        if let Some(session) = state.session.clone() {
            return Ok(session);
        }
        let Some(addr) = state.selected_addr.clone() else {
            return Err(RustProviderError::PairingRequired);
        };
        let (Some(client_id), Some(private_key_pem)) =
            read_credentials_files().unwrap_or((None, None))
        else {
            return Err(RustProviderError::PairingRequired);
        };
        let session = self.authenticate_session(&addr, client_id, private_key_pem)?;
        state.session = Some(session.clone());
        Ok(session)
    }

    fn refresh_session_for_addr(
        &self,
        addr: &str,
    ) -> std::result::Result<RustSession, RustProviderError> {
        let (Some(client_id), Some(private_key_pem)) =
            read_credentials_files().unwrap_or((None, None))
        else {
            return Err(RustProviderError::PairingRequired);
        };
        let session = self.authenticate_session(addr, client_id, private_key_pem)?;
        let mut state = self.state.lock();
        state.selected_addr = Some(addr.to_string());
        state.session = Some(session.clone());
        Ok(session)
    }

    fn authenticate_session(
        &self,
        addr: &str,
        client_id: String,
        private_key_pem: String,
    ) -> std::result::Result<RustSession, RustProviderError> {
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.authenticate_session_once(addr, &client_id, &private_key_pem) {
                Ok(session) => return Ok(session),
                Err(err @ RustProviderError::AuthFailed)
                | Err(err @ RustProviderError::Internal(_)) if attempt < 3 => {
                    last_error = Some(err);
                    thread::sleep(Duration::from_millis(250));
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_error.unwrap_or_else(|| RustProviderError::AuthFailed))
    }

    fn authenticate_session_once(
        &self,
        addr: &str,
        client_id: &str,
        private_key_pem: &str,
    ) -> std::result::Result<RustSession, RustProviderError> {
        let client = Client::builder()
            .http1_only()
            .cookie_store(true)
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| RustProviderError::Internal(format!("http client build failed: {e}")))?;
        let base = base_url(addr);
        let nonce_response = client
            .get(format!("{base}/auth/nonce/{client_id}"))
            .send()
            .map_err(|e| RustProviderError::Internal(format!("nonce request failed: {e}")))?;
        let nonce_status = nonce_response.status();
        let nonce_text = nonce_response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("nonce read failed: {e}")))?;
        if !nonce_status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "nonce http error: status={nonce_status} body={nonce_text}"
            )));
        }
        let nonce_value: Value = serde_json::from_str(&nonce_text)
            .map_err(|e| RustProviderError::Internal(format!("parse nonce failed: {e}; body={nonce_text}")))?;
        let nonce = nonce_value
            .get("nonce")
            .and_then(Value::as_str)
            .ok_or_else(|| RustProviderError::Internal("missing nonce".into()))?;
        let signed_nonce = sign_nonce_rsa_sha256_base64(&private_key_pem, nonce)?;

        let auth_body = serde_json::to_string(&json!({
            "client_id": client_id,
            "nonce_signed": signed_nonce,
        }))
        .map_err(|e| RustProviderError::Internal(format!("auth payload encode failed: {e}")))?;
        let response = client
            .put(format!("{base}/auth"))
            .header(CONTENT_TYPE, "application/json")
            .body(auth_body)
            .send()
            .map_err(|e| RustProviderError::Internal(format!("auth request failed: {e}")))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("auth response read failed: {e}")))?;
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => RustProviderError::Internal(format!(
                    "auth http error: status={status} headers={headers:?} body={body}"
                )),
                404 => RustProviderError::DeviceNotFound,
                409 => RustProviderError::ConflictDetected,
                _ => RustProviderError::Internal(format!(
                    "auth http error: status={status} headers={headers:?} body={body}"
                )),
            });
        }
        let cookie = extract_credentials_cookie_from_headers(&headers).ok_or_else(|| {
            RustProviderError::Internal(format!(
                "auth succeeded without credentials cookie; headers={headers:?}; body={body}",
            ))
        })?;
        Ok(RustSession {
            addr: addr.to_string(),
            cookie,
            client,
        })
    }

    fn begin_pair_for_addr(
        &self,
        addr: &str,
    ) -> std::result::Result<PendingPairing, RustProviderError> {
        let client = Client::builder()
            .http1_only()
            .cookie_store(true)
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| RustProviderError::Internal(format!("http client build failed: {e}")))?;

        let reg_base = format!("http://{addr}:8080/");
        let cleanup_url = format!("{reg_base}register/cleanup");
        let _ = client.put(&cleanup_url).send();
        thread::sleep(Duration::from_secs(1));
        let m1: Value = client
            .post(format!("{reg_base}register/pin"))
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?
            .error_for_status()
            .map_err(map_status_error)?
            .json()
            .map_err(|e| RustProviderError::Internal(format!("register/pin parse failed: {e}")))?;
        thread::sleep(Duration::from_secs(1));

        let n1 = decode_base64_bytes(value_str(&m1, "a")?)?;
        let mac = decode_base64_bytes(value_str(&m1, "b")?)?;
        let yb_wire = decode_base64_bytes(value_str(&m1, "c")?)?;
        let yb_int = BigUint::from_bytes_be(&yb_wire);
        let yb = to_fixed_len(&yb_int, 256);
        let mut n2 = [0_u8; 16];
        OsRng.fill_bytes(&mut n2);

        let p = BigUint::parse_bytes(DH_GROUP14_PRIME_HEX.as_bytes(), 16)
            .ok_or_else(|| RustProviderError::Internal("invalid dh prime".into()))?;
        let g = BigUint::from(2_u32);
        let mut a_raw = [0_u8; 32];
        OsRng.fill_bytes(&mut a_raw);
        let a = BigUint::from_bytes_be(&a_raw);
        let ya_int = g.modpow(&a, &p);
        let mut ya = vec![0_u8];
        ya.extend_from_slice(&to_fixed_len(&ya_int, 256));

        let zz_int = yb_int.modpow(&a, &p);
        let zz = to_fixed_len(&zz_int, 256);

        let mut derived = [0_u8; 48];
        let mut salt = Vec::new();
        salt.extend_from_slice(&n1);
        salt.extend_from_slice(&mac);
        salt.extend_from_slice(&n2);
        pbkdf2_hmac::<Sha256>(&zz, &salt, 10_000, &mut derived);
        let auth_key = &derived[..32];
        let key_wrap_key = &derived[32..];

        let m2hmac = hmac_sha256(auth_key, &[&n1, &mac, &yb, &n1, &n2, &mac, &ya])?;
        let m2 = json!({
            "a": b64(&n1),
            "b": b64(&n2),
            "c": b64(&mac),
            "d": b64(&ya),
            "e": b64(&m2hmac),
        });
        let m2_text = serde_json::to_string(&m2)
            .map_err(|e| RustProviderError::Internal(format!("register/hash encode failed: {e}")))?;
        thread::sleep(Duration::from_secs(1));
        let hash_response = client
            .post(format!("{reg_base}register/hash"))
            .header("Accept", "application/json")
            .json(&m2)
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        let hash_status = hash_response.status();
        let hash_text = hash_response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("register/hash read failed: {e}")))?;
        if !hash_status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "register/hash http error: status={hash_status} request_n1={} request_n2={} request_mac={} request_ya_len={} request_body={} body={hash_text}",
                b64(&n1),
                b64(&n2),
                b64(&mac),
                ya.len(),
                m2_text,
            )));
        }
        let m3: Value = serde_json::from_str(&hash_text)
            .map_err(|e| RustProviderError::Internal(format!("register/hash parse failed: {e}; body={hash_text}")))?;

        let m3_a = decode_base64_bytes(value_str(&m3, "a")?)?;
        if m3_a != n2 {
            return Err(RustProviderError::Internal(format!(
                "register/hash nonce mismatch: expected_n2={} got_a={} body={}",
                b64(&n2),
                b64(&m3_a),
                m3
            )));
        }
        let e_hash = decode_base64_bytes(value_str(&m3, "b")?)?;
        let m3hmac = decode_base64_bytes(value_str(&m3, "e")?)?;
        let expect_m3hmac = hmac_sha256(auth_key, &[&n1, &n2, &mac, &ya, &m2hmac, &n2, &e_hash])?;
        if m3hmac != expect_m3hmac {
            return Err(RustProviderError::Internal(format!(
                "register/hash m3 hmac mismatch: expected={} got={} body={}",
                b64(&expect_m3hmac),
                b64(&m3hmac),
                m3
            )));
        }

        Ok(PendingPairing {
            addr: addr.to_string(),
            client,
            reg_base,
            n1,
            n2: n2.to_vec(),
            auth_key: auth_key.to_vec(),
            key_wrap_key: key_wrap_key.to_vec(),
            yb,
            ya,
            e_hash,
            m3hmac,
        })
    }

    fn finish_pairing(
        &self,
        pending: PendingPairing,
        pin: &str,
    ) -> std::result::Result<(String, String), RustProviderError> {
        let PendingPairing {
            addr: _,
            client,
            reg_base,
            n1,
            n2,
            auth_key,
            key_wrap_key,
            yb,
            ya,
            e_hash,
            m3hmac,
        } = pending;

        let auth_key = auth_key.as_slice();
        let key_wrap_key = key_wrap_key.as_slice();

        let psk = hmac_sha256(auth_key, &[pin.as_bytes()])?;
        let mut rs = [0_u8; 16];
        OsRng.fill_bytes(&mut rs);
        let r_hash = hmac_sha256(auth_key, &[&rs, &psk, &yb, &ya])?;
        let wrapped_rs = wrap(&rs, auth_key, key_wrap_key)?;
        let m4hmac = hmac_sha256(
            auth_key,
            &[&n2, &e_hash, &m3hmac, &n1, &r_hash, &wrapped_rs],
        )?;
        let m4 = json!({
            "a": b64(&n1),
            "b": b64(&r_hash),
            "d": b64(&wrapped_rs),
            "e": b64(&m4hmac),
        });
        thread::sleep(Duration::from_secs(1));
        let ca_response = client
            .post(format!("{reg_base}register/ca"))
            .header("Accept", "application/json")
            .json(&m4)
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        let ca_status = ca_response.status();
        let ca_text = ca_response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("register/ca read failed: {e}")))?;
        if !ca_status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "register/ca http error: status={ca_status} request_n1={} request_r_hash={} request_wrapped_rs_len={} body={ca_text}",
                b64(&n1),
                b64(&r_hash),
                wrapped_rs.len(),
            )));
        }
        let m5: Value = serde_json::from_str(&ca_text)
            .map_err(|e| RustProviderError::Internal(format!("register/ca parse failed: {e}; body={ca_text}")))?;
        let m5_a = decode_base64_bytes(value_str(&m5, "a")?)?;
        if m5_a != n2 {
            return Err(RustProviderError::Internal(format!(
                "register/ca nonce mismatch: expected_n2={} got_a={} body={}",
                b64(&n2),
                b64(&m5_a),
                m5
            )));
        }
        let wrapped_es_cert = decode_base64_bytes(value_str(&m5, "d")?)?;
        let m5hmac = decode_base64_bytes(value_str(&m5, "e")?)?;
        let expect_m5hmac = hmac_sha256(
            auth_key,
            &[&n1, &r_hash, &wrapped_rs, &m4hmac, &n2, &wrapped_es_cert],
        )?;
        if m5hmac != expect_m5hmac {
            return Err(RustProviderError::Internal(format!(
                "register/ca hmac mismatch: expected={} got={} body={}",
                b64(&expect_m5hmac),
                b64(&m5hmac),
                m5
            )));
        }
        let es_cert = unwrap(&wrapped_es_cert, auth_key, key_wrap_key)?;
        if es_cert.len() < 17 {
            return Err(RustProviderError::AuthFailed);
        }
        let es = &es_cert[..16];
        let cert = &es_cert[16..];
        let expect_e_hash = hmac_sha256(auth_key, &[es, &psk, &yb, &ya])?;
        if expect_e_hash != e_hash {
            return Err(RustProviderError::Internal(format!(
                "register/ca e_hash mismatch: expected={} got={}",
                b64(&expect_e_hash),
                b64(&e_hash)
            )));
        }

        let rsa = openssl::rsa::Rsa::generate(2048)
            .map_err(|e| RustProviderError::Internal(format!("rsa generate failed: {e}")))?;
        let private_key = String::from_utf8(
            rsa.private_key_to_pem()
                .map_err(|e| RustProviderError::Internal(format!("pem encode failed: {e}")))?,
        )
        .map_err(|e| RustProviderError::Internal(format!("pem utf8 failed: {e}")))?;
        let public_key = rsa
            .public_key_to_pem()
            .map_err(|e| RustProviderError::Internal(format!("pub pem encode failed: {e}")))?;
        let client_id = Uuid::new_v4().to_string();

        let mut payload = Vec::new();
        payload.extend_from_slice(client_id.as_bytes());
        payload.extend_from_slice(&public_key);
        let wrapped_did_kpub = wrap(&payload, auth_key, key_wrap_key)?;
        let m6hmac = hmac_sha256(
            auth_key,
            &[&n2, &wrapped_es_cert, &m5hmac, &n1, &wrapped_did_kpub],
        )?;
        let m6 = json!({
            "a": b64(&n1),
            "d": b64(&wrapped_did_kpub),
            "e": b64(&m6hmac),
        });
        thread::sleep(Duration::from_secs(1));
        let register_response = client
            .post(format!("{reg_base}register"))
            .header("Accept", "application/json")
            .json(&m6)
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        let register_status = register_response.status();
        let register_text = register_response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("register read failed: {e}")))?;
        if !register_status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "register http error: status={register_status} request_n1={} request_wrapped_did_len={} body={register_text}",
                b64(&n1),
                wrapped_did_kpub.len(),
            )));
        }
        let _ = client.put(format!("{reg_base}register/cleanup")).send();

        if cert.is_empty() {
            return Err(RustProviderError::AuthFailed);
        }
        Ok((client_id, private_key))
    }

    fn resolve_target_addr(
        &self,
        addr: Option<String>,
    ) -> std::result::Result<String, RustProviderError> {
        if let Some(addr) = addr {
            return Ok(addr);
        }
        if let Some(addr) = self.state.lock().selected_addr.clone() {
            return Ok(addr);
        }
        Ok("digitalpaper.local".to_string())
    }

    fn registration_client(
        &self,
        timeout: Duration,
    ) -> std::result::Result<Client, RustProviderError> {
        Client::builder()
            .http1_only()
            .danger_accept_invalid_certs(true)
            .timeout(timeout)
            .build()
            .map_err(|e| RustProviderError::Internal(format!("http client build failed: {e}")))
    }

    fn resolve_object(
        &self,
        session: &RustSession,
        path: &str,
    ) -> std::result::Result<ResolveEntry, RustProviderError> {
        let encoded = quote_plus(path);
        let value = self.get_json(session, &format!("/resolve/entry/path/{encoded}"))?;
        serde_json::from_value(value).map_err(|e| {
            RustProviderError::Internal(format!("resolve object parse failed for {path}: {e}"))
        })
    }

    fn path_is_folder(
        &self,
        session: &RustSession,
        remote_path: &str,
    ) -> std::result::Result<bool, RustProviderError> {
        if remote_path.ends_with('/') {
            return Ok(true);
        }
        match self.resolve_object(session, remote_path) {
            Ok(obj) => Ok(obj.entry_type == "folder"),
            Err(RustProviderError::DeviceNotFound) => Ok(false),
            Err(err) => Err(err),
        }
    }

    fn new_folder(
        &self,
        session: &RustSession,
        remote_path: &str,
    ) -> std::result::Result<(), RustProviderError> {
        if remote_path.is_empty() {
            return Ok(());
        }
        if self.resolve_object(session, remote_path).is_ok() {
            return Ok(());
        }
        let parent = parent_path(remote_path);
        if parent.is_empty() {
            return Ok(());
        }
        self.new_folder(session, parent)?;
        let parent_id = self.resolve_object(session, parent)?.entry_id;
        self.post_json(
            session,
            "/folders2",
            &json!({ "folder_name": basename(remote_path), "parent_folder_id": parent_id }),
        )?;
        Ok(())
    }

    fn get_json(
        &self,
        session: &RustSession,
        endpoint: &str,
    ) -> std::result::Result<Value, RustProviderError> {
        self.get_json_with_retry(session, endpoint, true)
    }

    fn get_json_with_retry(
        &self,
        session: &RustSession,
        endpoint: &str,
        allow_retry: bool,
    ) -> std::result::Result<Value, RustProviderError> {
        let response = session
            .client
            .get(format!("{}{}", base_url(&session.addr), endpoint))
            .header(COOKIE, format!("Credentials={}", session.cookie))
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        if allow_retry && matches!(response.status().as_u16(), 401 | 403) {
            let fresh = self.refresh_session_for_addr(&session.addr)?;
            return self.get_json_with_retry(&fresh, endpoint, false);
        }
        response
            .error_for_status()
            .map_err(map_status_error)?
            .json()
            .map_err(|e| RustProviderError::Internal(format!("json decode failed: {e}")))
    }

    fn get_bytes(
        &self,
        session: &RustSession,
        endpoint: &str,
    ) -> std::result::Result<Vec<u8>, RustProviderError> {
        self.get_bytes_with_retry(session, endpoint, true)
    }

    fn get_bytes_with_retry(
        &self,
        session: &RustSession,
        endpoint: &str,
        allow_retry: bool,
    ) -> std::result::Result<Vec<u8>, RustProviderError> {
        let response = session
            .client
            .get(format!("{}{}", base_url(&session.addr), endpoint))
            .header(COOKIE, format!("Credentials={}", session.cookie))
            .send()
            .map_err(|e| RustProviderError::Internal(format!("bytes request failed: {e}")))?;
        let status = response.status();
        if allow_retry && matches!(status.as_u16(), 401 | 403) {
            let fresh = self.refresh_session_for_addr(&session.addr)?;
            return self.get_bytes_with_retry(&fresh, endpoint, false);
        }
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .map_err(|e| RustProviderError::Internal(format!("bytes decode failed: {e}")))?;
        if !status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "bytes http error: endpoint={endpoint} status={status} headers={headers:?} body={}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        Ok(bytes.to_vec())
    }

    fn post_json(
        &self,
        session: &RustSession,
        endpoint: &str,
        payload: &Value,
    ) -> std::result::Result<Value, RustProviderError> {
        self.post_json_with_retry(session, endpoint, payload, true)
    }

    fn post_json_with_retry(
        &self,
        session: &RustSession,
        endpoint: &str,
        payload: &Value,
        allow_retry: bool,
    ) -> std::result::Result<Value, RustProviderError> {
        let response = session
            .client
            .post(format!("{}{}", base_url(&session.addr), endpoint))
            .header(COOKIE, format!("Credentials={}", session.cookie))
            .json(payload)
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        if allow_retry && matches!(response.status().as_u16(), 401 | 403) {
            let fresh = self.refresh_session_for_addr(&session.addr)?;
            return self.post_json_with_retry(&fresh, endpoint, payload, false);
        }
        response
            .error_for_status()
            .map_err(map_status_error)?
            .json()
            .map_err(|e| RustProviderError::Internal(format!("json decode failed: {e}")))
    }

    fn put_json(
        &self,
        session: &RustSession,
        endpoint: &str,
        payload: &Value,
    ) -> std::result::Result<Value, RustProviderError> {
        self.put_json_with_retry(session, endpoint, payload, true)
    }

    fn put_json_with_retry(
        &self,
        session: &RustSession,
        endpoint: &str,
        payload: &Value,
        allow_retry: bool,
    ) -> std::result::Result<Value, RustProviderError> {
        let response = session
            .client
            .put(format!("{}{}", base_url(&session.addr), endpoint))
            .header(COOKIE, format!("Credentials={}", session.cookie))
            .json(payload)
            .send()
            .map_err(|e| RustProviderError::Internal(format!("put request failed: {e}")))?;
        let status = response.status();
        if allow_retry && matches!(status.as_u16(), 401 | 403) {
            let fresh = self.refresh_session_for_addr(&session.addr)?;
            return self.put_json_with_retry(&fresh, endpoint, payload, false);
        }
        let text = response
            .text()
            .map_err(|e| RustProviderError::Internal(format!("put response read failed: {e}")))?;
        if !status.is_success() {
            return Err(RustProviderError::Internal(format!(
                "put http error: endpoint={endpoint} status={status} payload={} body={text}",
                payload
            )));
        }
        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text)
            .map_err(|_| RustProviderError::Internal(format!("json decode failed: body={text}")))
    }

    fn put_multipart(
        &self,
        session: &RustSession,
        endpoint: &str,
        form: multipart::Form,
    ) -> std::result::Result<(), RustProviderError> {
        session
            .client
            .put(format!("{}{}", base_url(&session.addr), endpoint))
            .header(COOKIE, format!("Credentials={}", session.cookie))
            .multipart(form)
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?
            .error_for_status()
            .map_err(map_status_error)?;
        Ok(())
    }

    fn delete_empty(
        &self,
        session: &RustSession,
        endpoint: &str,
    ) -> std::result::Result<(), RustProviderError> {
        self.delete_empty_with_retry(session, endpoint, true)
    }

    fn delete_empty_with_retry(
        &self,
        session: &RustSession,
        endpoint: &str,
        allow_retry: bool,
    ) -> std::result::Result<(), RustProviderError> {
        let response = session
            .client
            .delete(format!("{}{}", base_url(&session.addr), endpoint))
            .header(COOKIE, format!("Credentials={}", session.cookie))
            .send()
            .map_err(|_| RustProviderError::TransportUnreachable)?;
        if allow_retry && matches!(response.status().as_u16(), 401 | 403) {
            let fresh = self.refresh_session_for_addr(&session.addr)?;
            return self.delete_empty_with_retry(&fresh, endpoint, false);
        }
        response.error_for_status().map_err(map_status_error)?;
        Ok(())
    }
}

fn parse_battery_level(battery: &Value) -> Option<u8> {
    battery
        .get("remain")
        .and_then(Value::as_u64)
        .and_then(|v| u8::try_from(v).ok())
        .or_else(|| {
            battery
                .get("level")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<u8>().ok())
        })
}

fn value_str<'a>(value: &'a Value, key: &str) -> std::result::Result<&'a str, RustProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RustProviderError::Internal(format!("missing {key}")))
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> std::result::Result<Vec<u8>, RustProviderError> {
    let mut h = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|e| RustProviderError::Internal(format!("hmac init failed: {e}")))?;
    for part in parts {
        h.update(part);
    }
    Ok(h.finalize().into_bytes().to_vec())
}

fn wrap(
    data: &[u8],
    auth_key: &[u8],
    key_wrap_key: &[u8],
) -> std::result::Result<Vec<u8>, RustProviderError> {
    let kwa_full = hmac_sha256(auth_key, &[data])?;
    let kwa = &kwa_full[..8];
    let mut payload = data.to_vec();
    payload.extend_from_slice(kwa);

    let mut iv = [0_u8; 16];
    OsRng.fill_bytes(&mut iv);
    let mut encrypted = openssl::symm::encrypt(
        openssl::symm::Cipher::aes_128_cbc(),
        key_wrap_key,
        Some(&iv),
        &payload,
    )
    .map_err(|e| RustProviderError::Internal(format!("wrap encrypt failed: {e}")))?;
    encrypted.extend_from_slice(&iv);
    Ok(encrypted)
}

fn unwrap(
    data: &[u8],
    auth_key: &[u8],
    key_wrap_key: &[u8],
) -> std::result::Result<Vec<u8>, RustProviderError> {
    if data.len() < 16 {
        return Err(RustProviderError::AuthFailed);
    }
    let (ciphertext, iv) = data.split_at(data.len() - 16);
    let mut unwrapped = openssl::symm::decrypt(
        openssl::symm::Cipher::aes_128_cbc(),
        key_wrap_key,
        Some(iv),
        ciphertext,
    )
    .map_err(|_| RustProviderError::AuthFailed)?;
    if unwrapped.len() < 8 {
        return Err(RustProviderError::AuthFailed);
    }
    let kwa = unwrapped.split_off(unwrapped.len() - 8);
    let local_kwa = hmac_sha256(auth_key, &[&unwrapped])?;
    if kwa != local_kwa[..8] {
        return Err(RustProviderError::AuthFailed);
    }
    Ok(unwrapped)
}

fn to_fixed_len(n: &BigUint, len: usize) -> Vec<u8> {
    let raw = n.to_bytes_be();
    if raw.len() >= len {
        raw[raw.len() - len..].to_vec()
    } else {
        let mut out = vec![0_u8; len - raw.len()];
        out.extend_from_slice(&raw);
        out
    }
}

fn map_status_error(err: reqwest::Error) -> RustProviderError {
    if let Some(code) = err.status() {
        return match code.as_u16() {
            401 | 403 => RustProviderError::AuthFailed,
            404 => RustProviderError::DeviceNotFound,
            409 => RustProviderError::ConflictDetected,
            _ => RustProviderError::Internal(format!("http error: {code}")),
        };
    }
    RustProviderError::Internal(format!("http request failed: {err}"))
}

fn find_credentials_in_folder(
    folder: PathBuf,
) -> std::result::Result<(PathBuf, PathBuf), RustProviderError> {
    let mut device = None;
    let mut key = None;
    for entry in WalkDir::new(folder).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "deviceid.dat" {
            device = Some(entry.path().to_path_buf());
        } else if name == "privatekey.dat" {
            key = Some(entry.path().to_path_buf());
        }
        if device.is_some() && key.is_some() {
            break;
        }
    }
    match (device, key) {
        (Some(d), Some(k)) => Ok((d, k)),
        _ => Err(RustProviderError::Internal(
            "credentials not found in provided folder".into(),
        )),
    }
}

fn persist_credentials(
    client_id: &str,
    private_key: &str,
) -> std::result::Result<(), RustProviderError> {
    let dir = home_config_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| RustProviderError::Internal(format!("create config dir failed: {e}")))?;
    fs::write(default_device_id_path(), client_id)
        .map_err(|e| RustProviderError::Internal(format!("write deviceid failed: {e}")))?;
    fs::write(default_private_key_path(), private_key)
        .map_err(|e| RustProviderError::Internal(format!("write privatekey failed: {e}")))?;
    Ok(())
}

fn candidate_addrs() -> Vec<String> {
    let mut values: BTreeSet<String> = BTreeSet::new();
    if let Ok(raw) = std::env::var("DPT_DEVICE_ADDRS") {
        for addr in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            values.insert(addr.to_owned());
        }
    }
    for addr in filtered_known_addrs() {
        values.insert(addr);
    }
    for addr in fallback_device_addrs() {
        values.insert((*addr).to_string());
    }
    values.insert("digitalpaper.local".into());
    for addr in lan_probe_candidates() {
        values.insert(addr);
    }
    values.into_iter().collect()
}

fn fallback_device_addrs() -> &'static [&'static str] {
    &["172.25.47.1"]
}

fn filtered_known_addrs() -> Vec<String> {
    let subnets = current_ipv4_subnets();
    read_known_addrs()
        .into_iter()
        .filter(|addr| match addr.parse::<Ipv4Addr>() {
            Ok(ip) if is_private_ipv4(ip) => subnets.iter().any(|(network, broadcast)| {
                let ip_u32 = u32::from(ip);
                ip_u32 > *network && ip_u32 < *broadcast
            }),
            Ok(_) => true,
            Err(_) => true,
        })
        .collect()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownAddrs {
    addrs: Vec<String>,
}

fn known_addrs_path() -> PathBuf {
    home_config_dir().join("known_devices.json")
}

fn read_known_addrs() -> Vec<String> {
    let path = known_addrs_path();
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<KnownAddrs>(&text) else {
        return Vec::new();
    };
    value.addrs
}

fn save_known_addrs<I>(addrs: I) -> std::result::Result<(), RustProviderError>
where
    I: IntoIterator<Item = String>,
{
    let mut all: BTreeSet<String> = read_known_addrs().into_iter().collect();
    for addr in addrs {
        let trimmed = addr.trim();
        if !trimmed.is_empty() {
            all.insert(trimmed.to_owned());
        }
    }
    let value = KnownAddrs {
        addrs: all.into_iter().collect(),
    };
    fs::create_dir_all(home_config_dir())
        .map_err(|e| RustProviderError::Internal(format!("create config dir failed: {e}")))?;
    let text = serde_json::to_string_pretty(&value)
        .map_err(|e| RustProviderError::Internal(format!("serialize known addrs failed: {e}")))?;
    fs::write(known_addrs_path(), text)
        .map_err(|e| RustProviderError::Internal(format!("write known addrs failed: {e}")))?;
    Ok(())
}

fn lan_probe_candidates() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return out;
    };
    for iface in ifaces {
        let (ip, netmask) = match iface.addr {
            if_addrs::IfAddr::V4(v4) => (v4.ip, v4.netmask),
            if_addrs::IfAddr::V6(_) => continue,
        };
        if ip.is_loopback() {
            continue;
        }
        let oct = ip.octets();
        if !(oct[0] == 10
            || (oct[0] == 172 && (16..=31).contains(&oct[1]))
            || (oct[0] == 192 && oct[1] == 168))
        {
            continue;
        }

        let ip_u32 = u32::from(ip);
        let mask_u32 = u32::from(netmask);
        let network = ip_u32 & mask_u32;
        let broadcast = network | !mask_u32;
        let host_count = broadcast.saturating_sub(network).saturating_sub(1);

        if host_count <= 64 {
            for host_u32 in (network + 1)..broadcast {
                let candidate = Ipv4Addr::from(host_u32);
                if candidate == ip {
                    continue;
                }
                out.push(candidate.to_string());
            }
        } else {
            for host in [1_u8, 2, 10, 20, 30, 50, 80, 92, 100, 110, 150, 200, 254] {
                if host == oct[3] {
                    continue;
                }
                let candidate = Ipv4Addr::new(oct[0], oct[1], oct[2], host);
                let candidate_u32 = u32::from(candidate);
                if candidate_u32 <= network || candidate_u32 >= broadcast {
                    continue;
                }
                out.push(candidate.to_string());
            }
        }
    }
    out
}

fn current_ipv4_subnets() -> Vec<(u32, u32)> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for iface in ifaces {
        let (ip, netmask) = match iface.addr {
            if_addrs::IfAddr::V4(v4) => (v4.ip, v4.netmask),
            if_addrs::IfAddr::V6(_) => continue,
        };
        if ip.is_loopback() {
            continue;
        }
        let oct = ip.octets();
        if !(oct[0] == 10
            || (oct[0] == 172 && (16..=31).contains(&oct[1]))
            || (oct[0] == 192 && oct[1] == 168))
        {
            continue;
        }
        let ip_u32 = u32::from(ip);
        let mask_u32 = u32::from(netmask);
        let network = ip_u32 & mask_u32;
        let broadcast = network | !mask_u32;
        out.push((network, broadcast));
    }
    out
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let oct = ip.octets();
    oct[0] == 10
        || (oct[0] == 172 && (16..=31).contains(&oct[1]))
        || (oct[0] == 192 && oct[1] == 168)
}

fn read_credentials_files() -> Option<(Option<String>, Option<String>)> {
    let device_id = read_trimmed(default_device_id_path()).ok();
    let private_key = read_trimmed(default_private_key_path()).ok();
    Some((device_id, private_key))
}

fn default_device_id_path() -> PathBuf {
    home_config_dir().join("deviceid.dat")
}

fn default_private_key_path() -> PathBuf {
    home_config_dir().join("privatekey.dat")
}

fn pending_pairing_path() -> PathBuf {
    home_config_dir().join("pending_pairing.json")
}

fn home_config_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config/dpt")
}

fn save_pending_pairing(pending: &PendingPairing) -> std::result::Result<(), RustProviderError> {
    let value = PersistedPendingPairing {
        addr: pending.addr.clone(),
        n1: pending.n1.clone(),
        n2: pending.n2.clone(),
        auth_key: pending.auth_key.clone(),
        key_wrap_key: pending.key_wrap_key.clone(),
        yb: pending.yb.clone(),
        ya: pending.ya.clone(),
        e_hash: pending.e_hash.clone(),
        m3hmac: pending.m3hmac.clone(),
    };
    fs::create_dir_all(home_config_dir())
        .map_err(|e| RustProviderError::Internal(format!("create config dir failed: {e}")))?;
    let text = serde_json::to_string_pretty(&value)
        .map_err(|e| RustProviderError::Internal(format!("serialize pending pairing failed: {e}")))?;
    fs::write(pending_pairing_path(), text)
        .map_err(|e| RustProviderError::Internal(format!("write pending pairing failed: {e}")))?;
    Ok(())
}

fn read_pending_pairing() -> std::result::Result<Option<PendingPairing>, RustProviderError> {
    let path = pending_pairing_path();
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(None);
    };
    let value: PersistedPendingPairing = serde_json::from_str(&text)
        .map_err(|e| RustProviderError::Internal(format!("parse pending pairing failed: {e}")))?;
    let client = Client::builder()
        .http1_only()
        .cookie_store(true)
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| RustProviderError::Internal(format!("http client build failed: {e}")))?;
    let reg_base = format!("http://{}:8080/", value.addr);
    Ok(Some(PendingPairing {
        addr: value.addr,
        client,
        reg_base,
        n1: value.n1,
        n2: value.n2,
        auth_key: value.auth_key,
        key_wrap_key: value.key_wrap_key,
        yb: value.yb,
        ya: value.ya,
        e_hash: value.e_hash,
        m3hmac: value.m3hmac,
    }))
}

fn clear_pending_pairing() -> std::result::Result<(), RustProviderError> {
    match fs::remove_file(pending_pairing_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(RustProviderError::Internal(format!("remove pending pairing failed: {e}"))),
    }
}

fn read_trimmed(path: PathBuf) -> std::io::Result<String> {
    let text = fs::read_to_string(path)?;
    Ok(text.trim().to_string())
}

fn probe_addr(addr: &str) -> bool {
    probe_dpt_name(addr, Duration::from_millis(500)).is_some()
}

fn probe_addrs_concurrently(
    addrs: &[String],
    max_workers: usize,
    timeout: Duration,
) -> HashMap<String, Option<String>> {
    if addrs.is_empty() {
        return HashMap::new();
    }

    let workers = max_workers.max(1).min(addrs.len());
    let queue = Arc::new(Mutex::new(
        addrs.iter().cloned().collect::<VecDeque<String>>(),
    ));
    let (tx, rx) = mpsc::channel::<(String, Option<String>)>();
    let mut handles = Vec::with_capacity(workers);

    for _ in 0..workers {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        handles.push(thread::spawn(move || loop {
            let next = {
                let mut lock = queue.lock();
                lock.pop_front()
            };
            let Some(addr) = next else {
                return;
            };
            let detected_name = probe_addr_with_timeout(&addr, timeout);
            let _ = tx.send((addr, detected_name));
        }));
    }
    drop(tx);

    let mut out = HashMap::new();
    for (addr, detected_name) in rx {
        out.insert(addr, detected_name);
    }
    for handle in handles {
        let _ = handle.join();
    }
    out
}

fn probe_addr_with_timeout(addr: &str, timeout: Duration) -> Option<String> {
    probe_dpt_name(addr, timeout)
}

fn probe_dpt_name(addr: &str, timeout: Duration) -> Option<String> {
    let client = Client::builder()
        .http1_only()
        .danger_accept_invalid_certs(true)
        .timeout(timeout)
        .build()
        .map_err(|err| {
            append_debug_log("probe", &format!("Failed to build HTTP client for {addr}: {err}"));
            err
        })
        .ok()?;
    let response = client
        .get(format!("http://{addr}:8080/register/information"))
        .send()
        .map_err(|err| {
            append_debug_log("probe", &format!("register/information request failed for {addr}: {err}"));
            err
        })
        .ok()?;
    if !response.status().is_success() {
        append_debug_log(
            "probe",
            &format!("register/information returned {} for {addr}", response.status()),
        );
        return None;
    }
    let value: Value = response.json().ok()?;
    let model_name = value
        .get("model_name")
        .or_else(|| value.get("modelName"))
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("Sony Digital Paper");
    let serial = value
        .get("serial_number")
        .or_else(|| value.get("serialNumber"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !model_name.to_ascii_lowercase().contains("dpt")
        && !model_name.to_ascii_lowercase().contains("digital paper")
        && serial.is_empty()
    {
        append_debug_log(
            "probe",
            &format!("Ignoring non-DPT response from {addr}: model={model_name} serial={serial}"),
        );
        return None;
    }
    append_debug_log(
        "probe",
        &format!("Detected DPT response from {addr}: model={model_name} serial={serial}"),
    );
    Some(if addr == "digitalpaper.local" {
        model_name.to_string()
    } else {
        format!("{model_name} ({addr})")
    })
}

fn append_debug_log(component: &str, message: &str) {
    let ts = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let line = format!("[{ts}] [{component}] {message}\n");
    let path = debug_log_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn debug_log_path() -> PathBuf {
    home_config_dir().join("debug.log")
}

fn chrono_like_now_utc() -> String {
    use std::process::Command;

    if let Ok(output) = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
    {
        if output.status.success() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }

    "1970-01-01T00:00:00Z".to_string()
}

fn sign_nonce_rsa_sha256_base64(
    private_key_pem: &str,
    nonce: &str,
) -> std::result::Result<String, RustProviderError> {
    let pkey = PKey::private_key_from_pem(private_key_pem.as_bytes())
        .map_err(|e| RustProviderError::Internal(format!("private key parse failed: {e}")))?;
    let mut signer = Signer::new(MessageDigest::sha256(), &pkey)
        .map_err(|e| RustProviderError::Internal(format!("signer init failed: {e}")))?;
    signer
        .update(nonce.as_bytes())
        .map_err(|e| RustProviderError::Internal(format!("signer update failed: {e}")))?;
    let sig = signer
        .sign_to_vec()
        .map_err(|e| RustProviderError::Internal(format!("sign failed: {e}")))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sig))
}

fn extract_credentials_cookie_from_headers(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(SET_COOKIE).iter() {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        if let Some(found) = parse_credentials_cookie(raw) {
            return Some(found);
        }
    }
    None
}

fn parse_credentials_cookie(raw: &str) -> Option<String> {
    for part in raw.split(';') {
        let piece = part.trim();
        if let Some(value) = piece.strip_prefix("Credentials=") {
            return Some(value.to_string());
        }
    }
    None
}

fn decode_base64_bytes(s: &str) -> std::result::Result<Vec<u8>, RustProviderError> {
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .map_err(|e| RustProviderError::Internal(format!("base64 decode failed: {e}")))
}

fn decode_base64_text(s: &str) -> Option<String> {
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .ok()
        .and_then(|v| String::from_utf8(v).ok())
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn quote_plus(path: &str) -> String {
    percent_encode(path.as_bytes(), QUERY_ESCAPE)
        .to_string()
        .replace("%20", "+")
}

fn base_url(addr: &str) -> String {
    let needs_port = !(addr.contains(':') && !addr.starts_with('['));
    if needs_port {
        format!("https://{addr}:8443")
    } else {
        format!("https://{addr}")
    }
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn pair_begin_retryable(err: &RustProviderError) -> bool {
    match err {
        RustProviderError::TransportUnreachable => true,
        RustProviderError::Internal(message) => {
            message.contains("503 Service Unavailable")
                || message.contains("register/hash http error")
                || message.contains("Application is closed")
        }
        _ => false,
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn map_remote_entry(entry: &Value) -> RemoteEntry {
    let path = entry
        .get("entry_path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let name = basename(&path).to_string();
    let entry_type = match entry.get("entry_type").and_then(Value::as_str) {
        Some("folder") => RemoteEntryType::Folder,
        Some("document") => RemoteEntryType::Document,
        _ => RemoteEntryType::Unknown,
    };
    RemoteEntry {
        path,
        name,
        entry_type,
        size: entry.get("file_size").and_then(Value::as_u64).or_else(|| {
            entry
                .get("file_size")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
        }),
        modified_at: entry
            .get("modified_date")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fallback_device_addrs, parse_credentials_cookie, quote_plus, read_known_addrs,
        save_known_addrs,
    };
    use std::sync::Mutex;
    use std::{env, fs};

    static HOME_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parse_credentials_cookie_works() {
        let raw = "Credentials=abc123; Path=/; HttpOnly";
        assert_eq!(parse_credentials_cookie(raw), Some("abc123".to_string()));
    }

    #[test]
    fn quote_plus_encodes_like_python() {
        let encoded = quote_plus("Document/Meeting Notes.pdf");
        assert!(encoded.contains("%2F"));
        assert!(encoded.contains("+"));
    }

    #[test]
    fn known_addr_persistence_roundtrip() {
        let _guard = HOME_TEST_LOCK.lock().expect("lock home test");
        let temp_home = env::temp_dir().join(format!(
            "digital-paper-rust-provider-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&temp_home).expect("create temp home");
        let prev = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", &temp_home);
        }

        save_known_addrs(["192.168.1.92".to_string(), "digitalpaper.local".to_string()])
            .expect("save known addrs");
        let known = read_known_addrs();
        assert!(known.iter().any(|a| a == "192.168.1.92"));
        assert!(known.iter().any(|a| a == "digitalpaper.local"));

        if let Some(prev) = prev {
            unsafe {
                env::set_var("HOME", prev);
            }
        } else {
            unsafe {
                env::remove_var("HOME");
            }
        }
        let _ = fs::remove_dir_all(temp_home);
    }

    #[test]
    fn fallback_addrs_include_python_usb_target() {
        assert!(fallback_device_addrs().contains(&"172.25.47.1"));
    }
}
