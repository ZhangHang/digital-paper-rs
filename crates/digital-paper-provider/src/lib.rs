use anyhow::{anyhow, Result};
use digital_paper_domain::{
    AdvancedCapabilities, BridgeError, BridgeErrorCode, ConnectionLog, DeviceStatus,
    DeviceSummary, RemoteEntry, TransportKind, UsbStatus, UsbSwitchMode, WifiConfigInput,
    WifiNetwork,
};
use digital_paper_rust_provider::{RustNativeProvider, RustProviderError};
use std::sync::Arc;

pub trait DptProvider: Send + Sync {
    fn discover_devices(&self) -> Result<Vec<DeviceSummary>>;
    fn list_all_entries(&self) -> Result<Vec<RemoteEntry>>;
    fn list_document_entries(&self) -> Result<Vec<RemoteEntry>>;
    fn connect(
        &self,
        addr: Option<String>,
        serial: Option<String>,
        transport_hint: Option<TransportKind>,
    ) -> Result<DeviceSummary>;
    fn pair(&self, pin: String) -> Result<DeviceSummary>;
    fn begin_pair_for_addr(&self, addr: String) -> Result<()>;
    fn validate_access(&self, root_path: String) -> Result<()>;
    fn list_entries(&self, path: String) -> Result<Vec<RemoteEntry>>;
    fn upload(&self, local_path: String, remote_path: String) -> Result<()>;
    fn download(&self, remote_path: String, local_path: String) -> Result<()>;
    fn delete(&self, path: String) -> Result<()>;
    fn move_entry(&self, src: String, dst: String) -> Result<()>;
    fn rename_entry(&self, path: String, new_name: String) -> Result<()>;
    fn create_folder(&self, path: String) -> Result<()>;
    fn copy_entry(&self, src: String, dst: String) -> Result<()>;
    fn path_exists(&self, path: String) -> Result<bool>;
    fn path_is_folder(&self, path: String) -> Result<bool>;
    fn device_info(&self) -> Result<DeviceStatus>;
    fn register_info(&self, addr: Option<String>) -> Result<serde_json::Value>;
    fn battery_info(&self) -> Result<serde_json::Value>;
    fn firmware_version(&self) -> Result<String>;
    fn mac_address(&self) -> Result<String>;
    fn api_version(&self, addr: Option<String>) -> Result<String>;
    fn list_wifi(&self) -> Result<Vec<WifiNetwork>>;
    fn scan_wifi(&self) -> Result<Vec<WifiNetwork>>;
    fn add_wifi(&self, ssid: String, security: String, passwd: String) -> Result<()>;
    fn add_wifi_full(&self, config: WifiConfigInput) -> Result<()>;
    fn remove_wifi(&self, ssid: String, security: String) -> Result<()>;
    fn toggle_wifi(&self, enabled: bool) -> Result<()>;
    fn get_config(&self) -> Result<serde_json::Value>;
    fn set_config(&self, config: serde_json::Value) -> Result<()>;
    fn get_config_value(&self, key: String) -> Result<serde_json::Value>;
    fn set_config_value(&self, key: String, value: serde_json::Value) -> Result<()>;
    fn set_datetime_now(&self) -> Result<()>;
    fn list_templates(&self) -> Result<serde_json::Value>;
    fn upload_template(&self, local_path: String, remote_path: String) -> Result<()>;
    fn delete_template(&self, template_name: String) -> Result<()>;
    fn display_document(&self, document_id: String, page: u32) -> Result<()>;
    fn take_screenshot(&self) -> Result<Vec<u8>>;
    fn ping(&self) -> Result<bool>;
    fn update_firmware(&self, local_path: String) -> Result<()>;
    fn sync_folder(
        &self,
        local_path: String,
        remote_path: String,
        dry_run: bool,
        assume_yes: bool,
    ) -> Result<serde_json::Value>;
    fn usb_status(&self) -> Result<UsbStatus>;
    fn usb_switch_mode(&self, mode: UsbSwitchMode) -> Result<UsbStatus>;
    fn usb_recover(&self) -> Result<UsbStatus>;
    fn import_credentials(
        &self,
        sony_app_folder: Option<String>,
        device_id_path: Option<String>,
        private_key_path: Option<String>,
    ) -> Result<DeviceSummary>;
    fn detect_advanced_capabilities(&self) -> Result<AdvancedCapabilities>;
    fn read_logs(&self) -> Result<Vec<ConnectionLog>>;
}

pub type ProviderRef = Arc<dyn DptProvider>;

pub fn rust_native_provider() -> ProviderRef {
    Arc::new(RustOnlyProvider::new())
}

pub struct RustOnlyProvider {
    native: RustNativeProvider,
}

impl RustOnlyProvider {
    pub fn new() -> Self {
        Self {
            native: RustNativeProvider::new(),
        }
    }
}

impl DptProvider for RustOnlyProvider {
    fn discover_devices(&self) -> Result<Vec<DeviceSummary>> {
        self.native.discover_devices().map_err(map_rust_error)
    }

    fn list_all_entries(&self) -> Result<Vec<RemoteEntry>> {
        self.native.list_all_entries().map_err(map_rust_error)
    }

    fn list_document_entries(&self) -> Result<Vec<RemoteEntry>> {
        self.native.list_document_entries().map_err(map_rust_error)
    }

    fn connect(
        &self,
        addr: Option<String>,
        serial: Option<String>,
        transport_hint: Option<TransportKind>,
    ) -> Result<DeviceSummary> {
        self.native
            .connect(addr, serial, transport_hint)
            .map_err(map_rust_error)
    }

    fn pair(&self, pin: String) -> Result<DeviceSummary> {
        self.native.pair(pin).map_err(map_rust_error)
    }

    fn begin_pair_for_addr(&self, addr: String) -> Result<()> {
        self.native
            .begin_pair_for_address(&addr)
            .map_err(map_rust_error)
    }

    fn validate_access(&self, root_path: String) -> Result<()> {
        self.native.list_entries(root_path).map(|_| ()).map_err(map_rust_error)
    }

    fn list_entries(&self, path: String) -> Result<Vec<RemoteEntry>> {
        self.native.list_entries(path).map_err(map_rust_error)
    }

    fn upload(&self, local_path: String, remote_path: String) -> Result<()> {
        self.native
            .upload(local_path, remote_path)
            .map_err(map_rust_error)
    }

    fn download(&self, remote_path: String, local_path: String) -> Result<()> {
        self.native
            .download(remote_path, local_path)
            .map_err(map_rust_error)
    }

    fn delete(&self, path: String) -> Result<()> {
        self.native.delete(path).map_err(map_rust_error)
    }

    fn move_entry(&self, src: String, dst: String) -> Result<()> {
        self.native.move_entry(src, dst).map_err(map_rust_error)
    }

    fn rename_entry(&self, path: String, new_name: String) -> Result<()> {
        self.native
            .rename_entry(path, new_name)
            .map_err(map_rust_error)
    }

    fn create_folder(&self, path: String) -> Result<()> {
        self.native.create_folder(path).map_err(map_rust_error)
    }

    fn copy_entry(&self, src: String, dst: String) -> Result<()> {
        self.native.copy_entry(src, dst).map_err(map_rust_error)
    }

    fn path_exists(&self, path: String) -> Result<bool> {
        self.native.path_exists(path).map_err(map_rust_error)
    }

    fn path_is_folder(&self, path: String) -> Result<bool> {
        self.native.path_is_folder_public(path).map_err(map_rust_error)
    }

    fn device_info(&self) -> Result<DeviceStatus> {
        self.native.device_info().map_err(map_rust_error)
    }

    fn register_info(&self, addr: Option<String>) -> Result<serde_json::Value> {
        self.native.register_info(addr).map_err(map_rust_error)
    }

    fn battery_info(&self) -> Result<serde_json::Value> {
        self.native.battery_info().map_err(map_rust_error)
    }

    fn firmware_version(&self) -> Result<String> {
        self.native.firmware_version().map_err(map_rust_error)
    }

    fn mac_address(&self) -> Result<String> {
        self.native.mac_address().map_err(map_rust_error)
    }

    fn api_version(&self, addr: Option<String>) -> Result<String> {
        self.native.api_version(addr).map_err(map_rust_error)
    }

    fn list_wifi(&self) -> Result<Vec<WifiNetwork>> {
        self.native.list_wifi().map_err(map_rust_error)
    }

    fn scan_wifi(&self) -> Result<Vec<WifiNetwork>> {
        self.native.scan_wifi().map_err(map_rust_error)
    }

    fn add_wifi(&self, ssid: String, security: String, passwd: String) -> Result<()> {
        self.native
            .add_wifi(ssid, security, passwd)
            .map_err(map_rust_error)
    }

    fn add_wifi_full(&self, config: WifiConfigInput) -> Result<()> {
        self.native.add_wifi_full(config).map_err(map_rust_error)
    }

    fn remove_wifi(&self, ssid: String, security: String) -> Result<()> {
        self.native
            .remove_wifi(ssid, security)
            .map_err(map_rust_error)
    }

    fn toggle_wifi(&self, enabled: bool) -> Result<()> {
        self.native.toggle_wifi(enabled).map_err(map_rust_error)
    }

    fn get_config(&self) -> Result<serde_json::Value> {
        self.native.get_config().map_err(map_rust_error)
    }

    fn set_config(&self, config: serde_json::Value) -> Result<()> {
        self.native.set_config(config).map_err(map_rust_error)
    }

    fn get_config_value(&self, key: String) -> Result<serde_json::Value> {
        self.native.get_config_value(&key).map_err(map_rust_error)
    }

    fn set_config_value(&self, key: String, value: serde_json::Value) -> Result<()> {
        self.native
            .set_config_value(&key, value)
            .map_err(map_rust_error)
    }

    fn set_datetime_now(&self) -> Result<()> {
        self.native.set_datetime_now().map_err(map_rust_error)
    }

    fn list_templates(&self) -> Result<serde_json::Value> {
        self.native.list_templates().map_err(map_rust_error)
    }

    fn upload_template(&self, local_path: String, remote_path: String) -> Result<()> {
        self.native
            .upload_template(local_path, remote_path)
            .map_err(map_rust_error)
    }

    fn delete_template(&self, template_name: String) -> Result<()> {
        self.native
            .delete_template(template_name)
            .map_err(map_rust_error)
    }

    fn display_document(&self, document_id: String, page: u32) -> Result<()> {
        self.native
            .display_document(document_id, page)
            .map_err(map_rust_error)
    }

    fn take_screenshot(&self) -> Result<Vec<u8>> {
        self.native.take_screenshot().map_err(map_rust_error)
    }

    fn ping(&self) -> Result<bool> {
        self.native.ping().map_err(map_rust_error)
    }

    fn update_firmware(&self, local_path: String) -> Result<()> {
        self.native.update_firmware(local_path).map_err(map_rust_error)
    }

    fn sync_folder(
        &self,
        local_path: String,
        remote_path: String,
        dry_run: bool,
        assume_yes: bool,
    ) -> Result<serde_json::Value> {
        self.native
            .sync_folder(local_path, remote_path, dry_run, assume_yes)
            .map_err(map_rust_error)
    }

    fn usb_status(&self) -> Result<UsbStatus> {
        self.native.usb_status().map_err(map_rust_error)
    }

    fn usb_switch_mode(&self, mode: UsbSwitchMode) -> Result<UsbStatus> {
        self.native.usb_switch_mode(mode).map_err(map_rust_error)
    }

    fn usb_recover(&self) -> Result<UsbStatus> {
        self.native.usb_recover().map_err(map_rust_error)
    }

    fn import_credentials(
        &self,
        sony_app_folder: Option<String>,
        device_id_path: Option<String>,
        private_key_path: Option<String>,
    ) -> Result<DeviceSummary> {
        self.native
            .import_credentials(sony_app_folder, device_id_path, private_key_path)
            .map_err(map_rust_error)
    }

    fn detect_advanced_capabilities(&self) -> Result<AdvancedCapabilities> {
        self.native
            .detect_advanced_capabilities()
            .map_err(map_rust_error)
    }

    fn read_logs(&self) -> Result<Vec<ConnectionLog>> {
        self.native.read_logs().map_err(map_rust_error)
    }
}

fn map_rust_error(err: RustProviderError) -> anyhow::Error {
    let bridge_error = match err {
        RustProviderError::DeviceNotFound => {
            BridgeError::new(BridgeErrorCode::DeviceNotFound, "device not found", None)
        }
        RustProviderError::PairingRequired => {
            BridgeError::new(BridgeErrorCode::PairingRequired, "pairing required", None)
        }
        RustProviderError::PinInvalid => {
            BridgeError::new(BridgeErrorCode::PinInvalid, "invalid pairing pin", None)
        }
        RustProviderError::TransportUnreachable => BridgeError::new(
            BridgeErrorCode::TransportUnreachable,
            "transport unreachable",
            None,
        ),
        RustProviderError::AuthFailed => {
            BridgeError::new(BridgeErrorCode::AuthFailed, "authentication failed", None)
        }
        RustProviderError::ConflictDetected => {
            BridgeError::new(BridgeErrorCode::ConflictDetected, "conflict detected", None)
        }
        RustProviderError::UnsupportedOverUsb => BridgeError::new(
            BridgeErrorCode::UnsupportedOverUsb,
            "unsupported over usb",
            None,
        ),
        RustProviderError::NotImplemented(op) => BridgeError::new(
            BridgeErrorCode::InternalError,
            format!("rust provider method not implemented: {op}"),
            None,
        ),
        RustProviderError::Internal(message) => {
            BridgeError::new(BridgeErrorCode::InternalError, message, None)
        }
    };
    anyhow!(bridge_error)
}
#[cfg(test)]
mod tests {
    use super::map_rust_error;
    use digital_paper_rust_provider::RustProviderError;

    #[test]
    fn rust_error_maps_to_bridge_error_code() {
        let err = map_rust_error(RustProviderError::PinInvalid);
        let mapped = err
            .downcast_ref::<digital_paper_domain::BridgeError>()
            .expect("bridge error expected");
        assert_eq!(mapped.code, digital_paper_domain::BridgeErrorCode::PinInvalid);
    }
}
