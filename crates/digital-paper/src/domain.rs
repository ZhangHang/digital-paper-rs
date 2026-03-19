use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_DEVICE_HOST: &str = "digitalpaper.local";
pub const USB_FALLBACK_ADDR: &str = "172.25.47.1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransportKind {
    Wifi,
    UsbNetwork,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RemoteEntryType {
    Document,
    Folder,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub id: String,
    pub serial: Option<String>,
    pub name: String,
    pub reachable_addrs: Vec<String>,
    pub transport_kinds: Vec<TransportKind>,
    pub paired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedDevice {
    pub summary: DeviceSummary,
    pub is_default: bool,
    pub last_connected_at: Option<String>,
    pub last_remote_path: Option<String>,
    pub last_local_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub path: String,
    pub name: String,
    pub entry_type: RemoteEntryType,
    pub size: Option<u64>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatteryStatus {
    pub level_percent: Option<u8>,
    pub charging: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub battery: BatteryStatus,
    pub storage: StorageStatus,
    pub firmware_version: Option<String>,
    pub owner: Option<String>,
    pub wifi_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WifiNetwork {
    pub id: Option<String>,
    pub ssid: String,
    pub security: String,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WifiConfigInput {
    pub ssid: String,
    pub security: String,
    pub passwd: String,
    pub dhcp: String,
    pub static_address: String,
    pub gateway: String,
    pub network_mask: String,
    pub dns1: String,
    pub dns2: String,
    pub proxy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsbSwitchMode {
    Auto,
    Ecm,
    Rndis,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsbStatusKind {
    NoUsbHardware,
    UsbSerialOnly,
    UsbNetworkVisible,
    DptEndpointReachable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsbStatus {
    pub kind: UsbStatusKind,
    pub tty_paths: Vec<String>,
    pub iface_names: Vec<String>,
    pub candidate_addrs: Vec<String>,
    pub endpoint_addr: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedCapabilities {
    pub apk_install_supported: bool,
    pub apk_install_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionLog {
    pub id: String,
    pub timestamp: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BridgeErrorCode {
    DeviceNotFound,
    PairingRequired,
    PinInvalid,
    AuthFailed,
    TransportUnreachable,
    ConflictDetected,
    UnsupportedOverUsb,
    InternalError,
}

impl BridgeErrorCode {
    pub fn from_wire(value: &str) -> Self {
        match value {
            "device_not_found" => Self::DeviceNotFound,
            "pairing_required" => Self::PairingRequired,
            "pin_invalid" => Self::PinInvalid,
            "auth_failed" => Self::AuthFailed,
            "transport_unreachable" => Self::TransportUnreachable,
            "conflict_detected" => Self::ConflictDetected,
            "unsupported_over_usb" => Self::UnsupportedOverUsb,
            _ => Self::InternalError,
        }
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct BridgeError {
    pub code: BridgeErrorCode,
    pub message: String,
    pub details: Option<String>,
}

impl BridgeError {
    pub fn new(code: BridgeErrorCode, message: impl Into<String>, details: Option<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }
}
