use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use digital_paper::{
    provider, ProviderRef, RemoteEntry, RemoteEntryType, RustNativeProvider, UsbSwitchMode,
    WifiConfigInput,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;

#[derive(Debug, Parser)]
#[command(name = "digital-paper-cli")]
#[command(about = "CLI for Sony Digital Paper provider verification and file operations")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    serial: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Discover,
    Validate {
        #[arg(long)]
        addr: Option<String>,
        #[arg(default_value = "Document")]
        path: String,
    },
    #[command(visible_alias = "open")]
    Connect {
        #[arg(long)]
        addr: Option<String>,
    },
    UsbStatus,
    UsbSwitch {
        #[arg(long, default_value = "auto")]
        mode: UsbSwitchModeArg,
    },
    UsbRecover,
    Pair {
        #[arg(long)]
        addr: Option<String>,
        pin: String,
    },
    PairBegin {
        #[arg(long)]
        addr: String,
    },
    PairFinish {
        pin: String,
    },
    Info {
        #[arg(long)]
        addr: Option<String>,
    },
    List {
        #[arg(long)]
        addr: Option<String>,
        #[arg(default_value = "Document")]
        path: String,
        #[arg(long)]
        recursive: bool,
    },
    ListAll {
        #[arg(long)]
        addr: Option<String>,
    },
    ListDocuments {
        #[arg(long)]
        addr: Option<String>,
    },
    Stat {
        #[arg(long)]
        addr: Option<String>,
        path: String,
    },
    Find {
        #[arg(long)]
        addr: Option<String>,
        #[arg(default_value = "Document")]
        path: String,
        #[arg(long)]
        name: String,
    },
    Exists {
        #[arg(long)]
        addr: Option<String>,
        path: String,
    },
    IsFolder {
        #[arg(long)]
        addr: Option<String>,
        path: String,
    },
    Mkdir {
        #[arg(long)]
        addr: Option<String>,
        path: String,
    },
    Rename {
        #[arg(long)]
        addr: Option<String>,
        path: String,
        new_name: String,
    },
    #[command(visible_alias = "move-document")]
    Move {
        #[arg(long)]
        addr: Option<String>,
        src: String,
        dst: String,
    },
    #[command(visible_alias = "copy-document")]
    Copy {
        #[arg(long)]
        addr: Option<String>,
        src: String,
        dst: String,
    },
    Delete {
        #[arg(long)]
        addr: Option<String>,
        path: String,
    },
    Upload {
        #[arg(long)]
        addr: Option<String>,
        local_path: String,
        remote_path: Option<String>,
    },
    Download {
        #[arg(long)]
        addr: Option<String>,
        remote_path: String,
        local_path: String,
    },
    Sync {
        #[arg(long)]
        addr: Option<String>,
        local_path: String,
        remote_path: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, short = 'y')]
        yes: bool,
    },
    RegisterInfo {
        #[arg(long)]
        addr: Option<String>,
    },
    Battery {
        #[arg(long)]
        addr: Option<String>,
    },
    FirmwareVersion {
        #[arg(long)]
        addr: Option<String>,
    },
    MacAddress {
        #[arg(long)]
        addr: Option<String>,
    },
    ApiVersion {
        #[arg(long)]
        addr: Option<String>,
    },
    ListWifi {
        #[arg(long)]
        addr: Option<String>,
    },
    ScanWifi {
        #[arg(long)]
        addr: Option<String>,
    },
    #[command(visible_alias = "wifi-add")]
    AddWifi {
        #[arg(long)]
        addr: Option<String>,
        ssid: String,
        security: String,
        passwd: String,
        #[arg(long, default_value = "true")]
        dhcp: String,
        #[arg(long, default_value = "")]
        static_address: String,
        #[arg(long, default_value = "")]
        gateway: String,
        #[arg(long, default_value = "")]
        network_mask: String,
        #[arg(long, default_value = "")]
        dns1: String,
        #[arg(long, default_value = "")]
        dns2: String,
        #[arg(long, default_value = "false")]
        proxy: String,
    },
    #[command(visible_alias = "wifi-del")]
    RemoveWifi {
        #[arg(long)]
        addr: Option<String>,
        ssid: String,
        security: String,
    },
    EnableWifi {
        #[arg(long)]
        addr: Option<String>,
    },
    DisableWifi {
        #[arg(long)]
        addr: Option<String>,
    },
    ConfigGet {
        #[arg(long)]
        addr: Option<String>,
        key: Option<String>,
    },
    ConfigSet {
        #[arg(long)]
        addr: Option<String>,
        key: String,
        value: String,
    },
    GetConfiguration {
        #[arg(long)]
        addr: Option<String>,
        path: String,
    },
    SetConfiguration {
        #[arg(long)]
        addr: Option<String>,
        path: String,
    },
    SetDatetime {
        #[arg(long)]
        addr: Option<String>,
    },
    ListTemplates {
        #[arg(long)]
        addr: Option<String>,
    },
    UploadTemplate {
        #[arg(long)]
        addr: Option<String>,
        local_path: String,
        remote_path: String,
    },
    DeleteTemplate {
        #[arg(long)]
        addr: Option<String>,
        template_name: String,
    },
    DisplayDocument {
        #[arg(long)]
        addr: Option<String>,
        document_id: String,
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    Screenshot {
        #[arg(long)]
        addr: Option<String>,
        output_path: String,
    },
    Ping {
        #[arg(long)]
        addr: Option<String>,
    },
    UpdateFirmware {
        #[arg(long)]
        addr: Option<String>,
        local_path: String,
    },
    ImportCredentials {
        #[arg(long)]
        sony_app_folder: Option<String>,
        #[arg(long)]
        device_id_path: Option<String>,
        #[arg(long)]
        private_key_path: Option<String>,
    },
    Capabilities,
    Logs {
        #[arg(long)]
        addr: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let provider = provider();

    match cli.command {
        Command::Discover => {
            let mut devices = provider.discover_devices()?;
            if let Some(serial) = cli.serial.as_ref() {
                devices.retain(|d| d.serial.as_deref() == Some(serial.as_str()));
            }
            print_output(cli.json, &devices)
        }
        Command::UsbStatus => {
            let status = provider.usb_status()?;
            print_output(cli.json, &status)
        }
        Command::UsbSwitch { mode } => {
            let status = provider.usb_switch_mode(mode.into())?;
            print_output(cli.json, &status)
        }
        Command::UsbRecover => {
            let status = provider.usb_recover()?;
            print_output(cli.json, &status)
        }
        Command::Validate { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.validate_access(path.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "path": path, "action": "validate_access" }),
            )
        }
        Command::Connect { addr } => {
            let device = provider.connect(addr, cli.serial.clone(), None)?;
            print_output(cli.json, &device)
        }
        Command::Pair { addr, pin } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let device = provider.pair(pin)?;
            print_output(cli.json, &device)
        }
        Command::PairBegin { addr } => pair_begin(cli.json, addr),
        Command::PairFinish { pin } => pair_finish(cli.json, pin),
        Command::Info { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let info = provider.device_info()?;
            print_output(cli.json, &info)
        }
        Command::List {
            addr,
            path,
            recursive,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let entries = if recursive {
                list_entries_recursive(&provider, &path)?
            } else {
                provider.list_entries(path)?
            };
            print_output(cli.json, &entries)
        }
        Command::ListAll { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let entries = provider.list_all_entries()?;
            print_output(cli.json, &entries)
        }
        Command::ListDocuments { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let entries = provider.list_document_entries()?;
            print_output(cli.json, &entries)
        }
        Command::Stat { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let entry = stat_entry(&provider, &path)?;
            print_output(cli.json, &entry)
        }
        Command::Find { addr, path, name } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let entries = find_entries_by_name(&provider, &path, &name)?;
            print_output(cli.json, &entries)
        }
        Command::Exists { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let exists = provider.path_exists(path.clone())?;
            print_output(cli.json, &json!({ "path": path, "exists": exists }))
        }
        Command::IsFolder { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let is_folder = provider.path_is_folder(path.clone())?;
            print_output(cli.json, &json!({ "path": path, "isFolder": is_folder }))
        }
        Command::Mkdir { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.create_folder(path.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "path": path, "action": "create_folder" }),
            )
        }
        Command::Rename {
            addr,
            path,
            new_name,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.rename_entry(path.clone(), new_name.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "path": path, "newName": new_name, "action": "rename" }),
            )
        }
        Command::Move { addr, src, dst } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.move_entry(src.clone(), dst.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "src": src, "dst": dst, "action": "move" }),
            )
        }
        Command::Copy { addr, src, dst } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.copy_entry(src.clone(), dst.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "src": src, "dst": dst, "action": "copy" }),
            )
        }
        Command::Delete { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.delete(path.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "path": path, "action": "delete" }),
            )
        }
        Command::Upload {
            addr,
            local_path,
            remote_path,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let target = remote_path.unwrap_or_else(|| {
                let name = std::path::Path::new(&local_path)
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("upload.pdf");
                format!("Document/{name}")
            });
            provider.upload(local_path.clone(), target.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "localPath": local_path, "remotePath": target, "action": "upload" }),
            )
        }
        Command::Download {
            addr,
            remote_path,
            local_path,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.download(remote_path.clone(), local_path.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "remotePath": remote_path, "localPath": local_path, "action": "download" }),
            )
        }
        Command::Sync {
            addr,
            local_path,
            remote_path,
            dry_run,
            yes,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let result = provider.sync_folder(local_path, remote_path, dry_run, yes)?;
            print_output(cli.json, &result)
        }
        Command::RegisterInfo { addr } => {
            let info = provider.register_info(addr)?;
            print_output(cli.json, &info)
        }
        Command::Battery { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let info = provider.battery_info()?;
            print_output(cli.json, &info)
        }
        Command::FirmwareVersion { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let value = provider.firmware_version()?;
            print_output(cli.json, &json!({ "value": value }))
        }
        Command::MacAddress { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let value = provider.mac_address()?;
            print_output(cli.json, &json!({ "value": value }))
        }
        Command::ApiVersion { addr } => {
            let value = provider.api_version(addr)?;
            print_output(cli.json, &json!({ "value": value }))
        }
        Command::ListWifi { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let items = provider.list_wifi()?;
            print_output(cli.json, &items)
        }
        Command::ScanWifi { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let items = provider.scan_wifi()?;
            print_output(cli.json, &items)
        }
        Command::AddWifi {
            addr,
            ssid,
            security,
            passwd,
            dhcp,
            static_address,
            gateway,
            network_mask,
            dns1,
            dns2,
            proxy,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let cfg = WifiConfigInput {
                ssid: ssid.clone(),
                security: security.clone(),
                passwd,
                dhcp,
                static_address,
                gateway,
                network_mask,
                dns1,
                dns2,
                proxy,
            };
            provider.add_wifi_full(cfg)?;
            print_output(
                cli.json,
                &json!({ "ok": true, "ssid": ssid, "security": security, "action": "add_wifi" }),
            )
        }
        Command::RemoveWifi {
            addr,
            ssid,
            security,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.remove_wifi(ssid.clone(), security.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "ssid": ssid, "security": security, "action": "remove_wifi" }),
            )
        }
        Command::EnableWifi { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.toggle_wifi(true)?;
            print_output(cli.json, &json!({ "ok": true, "enabled": true }))
        }
        Command::DisableWifi { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.toggle_wifi(false)?;
            print_output(cli.json, &json!({ "ok": true, "enabled": false }))
        }
        Command::ConfigGet { addr, key } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            match key {
                Some(key) => {
                    let value = provider.get_config_value(key.clone())?;
                    print_output(cli.json, &json!({ "key": key, "value": value }))
                }
                None => {
                    let value = provider.get_config()?;
                    print_output(cli.json, &value)
                }
            }
        }
        Command::ConfigSet { addr, key, value } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let parsed = parse_json_or_string(&value);
            provider.set_config_value(key.clone(), parsed.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "key": key, "value": parsed, "action": "config_set" }),
            )
        }
        Command::GetConfiguration { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let value = provider.get_config()?;
            fs::write(&path, serde_json::to_string_pretty(&value)?)
                .with_context(|| format!("failed to write configuration to {path}"))?;
            print_output(cli.json, &json!({ "ok": true, "path": path }))
        }
        Command::SetConfiguration { addr, path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read configuration from {path}"))?;
            let value: Value =
                serde_json::from_str(&text).with_context(|| format!("invalid JSON in {path}"))?;
            provider.set_config(value)?;
            print_output(cli.json, &json!({ "ok": true, "path": path }))
        }
        Command::SetDatetime { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.set_datetime_now()?;
            print_output(cli.json, &json!({ "ok": true, "action": "set_datetime" }))
        }
        Command::ListTemplates { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let items = provider.list_templates()?;
            print_output(cli.json, &items)
        }
        Command::UploadTemplate {
            addr,
            local_path,
            remote_path,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.upload_template(local_path.clone(), remote_path.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "localPath": local_path, "remotePath": remote_path, "action": "upload_template" }),
            )
        }
        Command::DeleteTemplate {
            addr,
            template_name,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.delete_template(template_name.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "templateName": template_name, "action": "delete_template" }),
            )
        }
        Command::DisplayDocument {
            addr,
            document_id,
            page,
        } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.display_document(document_id.clone(), page)?;
            print_output(
                cli.json,
                &json!({ "ok": true, "documentId": document_id, "page": page, "action": "display_document" }),
            )
        }
        Command::Screenshot { addr, output_path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let image = provider.take_screenshot()?;
            fs::write(&output_path, &image)
                .with_context(|| format!("failed to write screenshot to {output_path}"))?;
            print_output(
                cli.json,
                &json!({ "ok": true, "outputPath": output_path, "bytes": image.len() }),
            )
        }
        Command::Ping { addr } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            let ok = provider.ping()?;
            print_output(cli.json, &json!({ "ok": ok }))
        }
        Command::UpdateFirmware { addr, local_path } => {
            ensure_connected(&provider, addr, cli.serial.clone())?;
            provider.update_firmware(local_path.clone())?;
            print_output(
                cli.json,
                &json!({ "ok": true, "localPath": local_path, "action": "update_firmware" }),
            )
        }
        Command::ImportCredentials {
            sony_app_folder,
            device_id_path,
            private_key_path,
        } => {
            let result =
                provider.import_credentials(sony_app_folder, device_id_path, private_key_path)?;
            print_output(cli.json, &result)
        }
        Command::Capabilities => {
            let capabilities = provider.detect_advanced_capabilities()?;
            print_output(cli.json, &capabilities)
        }
        Command::Logs { addr } => {
            if addr.is_some() {
                ensure_connected(&provider, addr, cli.serial.clone())?;
            }
            let logs = provider.read_logs()?;
            print_output(cli.json, &logs)
        }
    }
}

fn ensure_connected(
    provider: &ProviderRef,
    addr: Option<String>,
    serial: Option<String>,
) -> Result<()> {
    provider
        .connect(addr, serial, None)
        .context("connect failed")?;
    Ok(())
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum UsbSwitchModeArg {
    Auto,
    Ecm,
    Rndis,
}

impl From<UsbSwitchModeArg> for UsbSwitchMode {
    fn from(value: UsbSwitchModeArg) -> Self {
        match value {
            UsbSwitchModeArg::Auto => UsbSwitchMode::Auto,
            UsbSwitchModeArg::Ecm => UsbSwitchMode::Ecm,
            UsbSwitchModeArg::Rndis => UsbSwitchMode::Rndis,
        }
    }
}

fn list_entries_recursive(provider: &ProviderRef, root: &str) -> Result<Vec<RemoteEntry>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_string()];
    while let Some(path) = stack.pop() {
        let entries = provider.list_entries(path.clone())?;
        for entry in entries {
            if entry.entry_type == RemoteEntryType::Folder {
                stack.push(entry.path.clone());
            }
            out.push(entry);
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn stat_entry(provider: &ProviderRef, path: &str) -> Result<RemoteEntry> {
    let normalized = path.trim_end_matches('/');
    let parent = parent_path(normalized);
    let entries = provider.list_entries(parent.to_string())?;
    entries
        .into_iter()
        .find(|entry| entry.path == normalized)
        .with_context(|| format!("remote path not found: {path}"))
}

fn find_entries_by_name(
    provider: &ProviderRef,
    root: &str,
    needle: &str,
) -> Result<Vec<RemoteEntry>> {
    let query = needle.to_ascii_lowercase();
    let mut entries = list_entries_recursive(provider, root)?;
    entries.retain(|entry| entry.name.to_ascii_lowercase().contains(&query));
    Ok(entries)
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn parse_json_or_string(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

fn print_output(json_mode: bool, value: &impl Serialize) -> Result<()> {
    if json_mode {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        print_human(value)?;
    }
    Ok(())
}

fn print_human(value: &impl Serialize) -> Result<()> {
    let wire = serde_json::to_value(value)?;
    match wire {
        Value::Array(items) => {
            for item in items {
                println!("{}", serde_json::to_string_pretty(&item)?);
            }
        }
        other => println!("{}", serde_json::to_string_pretty(&other)?),
    }
    Ok(())
}

fn pair_begin(json_mode: bool, addr: String) -> Result<()> {
    let provider = RustNativeProvider::new();
    provider
        .begin_pair_for_address(&addr)
        .context("failed to start pairing")?;
    print_output(
        json_mode,
        &json!({ "ok": true, "addr": addr, "message": "PIN should now be visible on the DPT." }),
    )
}

fn pair_finish(json_mode: bool, pin: String) -> Result<()> {
    let provider = RustNativeProvider::new();
    let device = provider
        .pair(pin.trim().to_string())
        .context("pair failed")?;
    print_output(json_mode, &device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use digital_paper::Provider;
    use digital_paper::{
        AdvancedCapabilities, BatteryStatus, ConnectionLog, DeviceStatus, DeviceSummary,
        StorageStatus, TransportKind, UsbStatus, UsbStatusKind, UsbSwitchMode, WifiConfigInput,
        WifiNetwork, USB_FALLBACK_ADDR,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Default)]
    struct TestProvider {
        entries: HashMap<String, Vec<RemoteEntry>>,
    }

    impl TestProvider {
        fn with_entries(entries: &[(&str, Vec<RemoteEntry>)]) -> ProviderRef {
            let entries = entries
                .iter()
                .map(|(path, items)| ((*path).to_string(), items.clone()))
                .collect();
            Arc::new(Self { entries })
        }
    }

    fn unsupported<T>() -> Result<T> {
        Err(anyhow!("not implemented for test"))
    }

    impl Provider for TestProvider {
        fn discover_devices(&self) -> Result<Vec<DeviceSummary>> {
            unsupported()
        }

        fn list_all_entries(&self) -> Result<Vec<RemoteEntry>> {
            unsupported()
        }

        fn list_document_entries(&self) -> Result<Vec<RemoteEntry>> {
            unsupported()
        }

        fn connect(
            &self,
            _addr: Option<String>,
            _serial: Option<String>,
            _transport_hint: Option<TransportKind>,
        ) -> Result<DeviceSummary> {
            Ok(DeviceSummary {
                id: "test".into(),
                serial: None,
                name: "Test".into(),
                reachable_addrs: vec![],
                transport_kinds: vec![TransportKind::Wifi],
                paired: true,
            })
        }

        fn pair(&self, _pin: String) -> Result<DeviceSummary> {
            unsupported()
        }

        fn begin_pair_for_addr(&self, _addr: String) -> Result<()> {
            unsupported()
        }

        fn validate_access(&self, _root_path: String) -> Result<()> {
            unsupported()
        }

        fn list_entries(&self, path: String) -> Result<Vec<RemoteEntry>> {
            Ok(self.entries.get(&path).cloned().unwrap_or_default())
        }

        fn upload(&self, _local_path: String, _remote_path: String) -> Result<()> {
            unsupported()
        }

        fn download(&self, _remote_path: String, _local_path: String) -> Result<()> {
            unsupported()
        }

        fn delete(&self, _path: String) -> Result<()> {
            unsupported()
        }

        fn move_entry(&self, _src: String, _dst: String) -> Result<()> {
            unsupported()
        }

        fn rename_entry(&self, _path: String, _new_name: String) -> Result<()> {
            unsupported()
        }

        fn create_folder(&self, _path: String) -> Result<()> {
            unsupported()
        }

        fn copy_entry(&self, _src: String, _dst: String) -> Result<()> {
            unsupported()
        }

        fn path_exists(&self, _path: String) -> Result<bool> {
            unsupported()
        }

        fn path_is_folder(&self, _path: String) -> Result<bool> {
            unsupported()
        }

        fn device_info(&self) -> Result<DeviceStatus> {
            Ok(DeviceStatus {
                battery: BatteryStatus {
                    level_percent: Some(100),
                    charging: false,
                },
                storage: StorageStatus {
                    total_bytes: Some(1),
                    free_bytes: Some(1),
                },
                firmware_version: Some("1.0".into()),
                owner: Some("test".into()),
                wifi_enabled: true,
            })
        }

        fn register_info(&self, _addr: Option<String>) -> Result<Value> {
            unsupported()
        }

        fn battery_info(&self) -> Result<Value> {
            unsupported()
        }

        fn firmware_version(&self) -> Result<String> {
            unsupported()
        }

        fn mac_address(&self) -> Result<String> {
            unsupported()
        }

        fn api_version(&self, _addr: Option<String>) -> Result<String> {
            unsupported()
        }

        fn list_wifi(&self) -> Result<Vec<WifiNetwork>> {
            unsupported()
        }

        fn scan_wifi(&self) -> Result<Vec<WifiNetwork>> {
            unsupported()
        }

        fn add_wifi(&self, _ssid: String, _security: String, _passwd: String) -> Result<()> {
            unsupported()
        }

        fn add_wifi_full(&self, _config: WifiConfigInput) -> Result<()> {
            unsupported()
        }

        fn remove_wifi(&self, _ssid: String, _security: String) -> Result<()> {
            unsupported()
        }

        fn toggle_wifi(&self, _enabled: bool) -> Result<()> {
            unsupported()
        }

        fn get_config(&self) -> Result<Value> {
            unsupported()
        }

        fn get_config_value(&self, _key: String) -> Result<Value> {
            unsupported()
        }

        fn set_config(&self, _config: Value) -> Result<()> {
            unsupported()
        }

        fn set_config_value(&self, _key: String, _value: Value) -> Result<()> {
            unsupported()
        }

        fn set_datetime_now(&self) -> Result<()> {
            unsupported()
        }

        fn list_templates(&self) -> Result<Value> {
            unsupported()
        }

        fn upload_template(&self, _local_path: String, _remote_path: String) -> Result<()> {
            unsupported()
        }

        fn delete_template(&self, _template_name: String) -> Result<()> {
            unsupported()
        }

        fn display_document(&self, _document_id: String, _page: u32) -> Result<()> {
            unsupported()
        }

        fn take_screenshot(&self) -> Result<Vec<u8>> {
            unsupported()
        }

        fn ping(&self) -> Result<bool> {
            unsupported()
        }

        fn update_firmware(&self, _local_path: String) -> Result<()> {
            unsupported()
        }

        fn sync_folder(
            &self,
            _local_path: String,
            _remote_path: String,
            _dry_run: bool,
            _assume_yes: bool,
        ) -> Result<Value> {
            unsupported()
        }

        fn usb_status(&self) -> Result<UsbStatus> {
            Ok(UsbStatus {
                kind: UsbStatusKind::NoUsbHardware,
                tty_paths: Vec::new(),
                iface_names: Vec::new(),
                candidate_addrs: Vec::new(),
                endpoint_addr: None,
                message: "none".into(),
            })
        }

        fn usb_switch_mode(&self, _mode: UsbSwitchMode) -> Result<UsbStatus> {
            self.usb_status()
        }

        fn usb_recover(&self) -> Result<UsbStatus> {
            self.usb_status()
        }

        fn import_credentials(
            &self,
            _sony_app_folder: Option<String>,
            _device_id_path: Option<String>,
            _private_key_path: Option<String>,
        ) -> Result<DeviceSummary> {
            unsupported()
        }

        fn detect_advanced_capabilities(&self) -> Result<AdvancedCapabilities> {
            unsupported()
        }

        fn read_logs(&self) -> Result<Vec<ConnectionLog>> {
            unsupported()
        }
    }

    fn folder(path: &str) -> RemoteEntry {
        RemoteEntry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            entry_type: RemoteEntryType::Folder,
            size: None,
            modified_at: None,
        }
    }

    fn document(path: &str) -> RemoteEntry {
        RemoteEntry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            entry_type: RemoteEntryType::Document,
            size: Some(1),
            modified_at: Some("2026-03-13T00:00:00Z".into()),
        }
    }

    #[test]
    fn parent_path_handles_root_and_nested_paths() {
        assert_eq!(parent_path("Document"), "");
        assert_eq!(parent_path("Document/Daily"), "Document");
        assert_eq!(
            parent_path("Document/Daily/2026-03-13.pdf"),
            "Document/Daily"
        );
    }

    #[test]
    fn parse_json_or_string_preserves_plain_strings() {
        assert_eq!(parse_json_or_string("plain"), Value::String("plain".into()));
        assert_eq!(parse_json_or_string("true"), Value::Bool(true));
        assert_eq!(parse_json_or_string("42"), json!(42));
        assert_eq!(parse_json_or_string("{\"a\":1}"), json!({"a": 1}));
    }

    #[test]
    fn list_entries_recursive_returns_sorted_nested_entries() {
        let provider = TestProvider::with_entries(&[
            (
                "Document",
                vec![folder("Document/Daily"), document("Document/index.pdf")],
            ),
            (
                "Document/Daily",
                vec![document("Document/Daily/2026-03-13.pdf")],
            ),
        ]);

        let entries = list_entries_recursive(&provider, "Document").unwrap();

        let paths: Vec<_> = entries.into_iter().map(|entry| entry.path).collect();
        assert_eq!(
            paths,
            vec![
                "Document/Daily",
                "Document/Daily/2026-03-13.pdf",
                "Document/index.pdf",
            ]
        );
    }

    #[test]
    fn stat_entry_finds_direct_child_in_parent_listing() {
        let provider = TestProvider::with_entries(&[(
            "Document/Daily",
            vec![document("Document/Daily/2026-03-13.pdf")],
        )]);

        let entry = stat_entry(&provider, "Document/Daily/2026-03-13.pdf").unwrap();

        assert_eq!(entry.path, "Document/Daily/2026-03-13.pdf");
    }

    #[test]
    fn stat_entry_errors_for_missing_path() {
        let provider = TestProvider::with_entries(&[("Document", vec![])]);
        let err = stat_entry(&provider, "Document/missing.pdf").unwrap_err();
        assert!(err.to_string().contains("remote path not found"));
    }

    #[test]
    fn find_entries_by_name_is_case_insensitive() {
        let provider = TestProvider::with_entries(&[
            (
                "Document",
                vec![
                    folder("Document/Daily"),
                    document("Document/Other.pdf"),
                    document("Document/summary.PDF"),
                ],
            ),
            (
                "Document/Daily",
                vec![document("Document/Daily/Meeting Notes.pdf")],
            ),
        ]);

        let entries = find_entries_by_name(&provider, "Document", "meeting").unwrap();
        let paths: Vec<_> = entries.into_iter().map(|entry| entry.path).collect();

        assert_eq!(paths, vec!["Document/Daily/Meeting Notes.pdf"]);
    }

    #[test]
    fn clap_parses_workflow_and_parity_commands() {
        let cases = [
            vec!["digital-paper-cli", "list", "--recursive"],
            vec!["digital-paper-cli", "stat", "Document/foo.pdf"],
            vec!["digital-paper-cli", "find", "--name", "2026-03-13"],
            vec!["digital-paper-cli", "sync", "./out", "Document/Summaries"],
            vec![
                "digital-paper-cli",
                "sync",
                "./out",
                "Document/Summaries",
                "--dry-run",
            ],
            vec!["digital-paper-cli", "usb-status"],
            vec!["digital-paper-cli", "usb-switch", "--mode", "ecm"],
            vec!["digital-paper-cli", "usb-recover"],
            vec![
                "digital-paper-cli",
                "move",
                "Document/a.pdf",
                "Document/b.pdf",
            ],
            vec![
                "digital-paper-cli",
                "move-document",
                "Document/a.pdf",
                "Document/b.pdf",
            ],
            vec![
                "digital-paper-cli",
                "copy",
                "Document/a.pdf",
                "Document/b.pdf",
            ],
            vec![
                "digital-paper-cli",
                "copy-document",
                "Document/a.pdf",
                "Document/b.pdf",
            ],
            vec!["digital-paper-cli", "list-all"],
            vec!["digital-paper-cli", "list-documents"],
            vec!["digital-paper-cli", "exists", "Document/foo.pdf"],
            vec!["digital-paper-cli", "is-folder", "Document"],
            vec![
                "digital-paper-cli",
                "register-info",
                "--addr",
                USB_FALLBACK_ADDR,
            ],
            vec!["digital-paper-cli", "battery"],
            vec!["digital-paper-cli", "firmware-version"],
            vec!["digital-paper-cli", "mac-address"],
            vec![
                "digital-paper-cli",
                "api-version",
                "--addr",
                USB_FALLBACK_ADDR,
            ],
            vec!["digital-paper-cli", "list-wifi"],
            vec!["digital-paper-cli", "scan-wifi"],
            vec!["digital-paper-cli", "add-wifi", "ssid", "psk", "secret"],
            vec![
                "digital-paper-cli",
                "add-wifi",
                "ssid",
                "psk",
                "secret",
                "--dhcp",
                "false",
                "--static-address",
                "172.20.10.2",
                "--gateway",
                "172.20.10.1",
                "--network-mask",
                "24",
                "--dns1",
                "8.8.8.8",
                "--proxy",
                "false",
            ],
            vec!["digital-paper-cli", "remove-wifi", "ssid", "psk"],
            vec!["digital-paper-cli", "enable-wifi"],
            vec!["digital-paper-cli", "disable-wifi"],
            vec!["digital-paper-cli", "config-get", "owner"],
            vec!["digital-paper-cli", "config-set", "owner", "\"me\""],
            vec!["digital-paper-cli", "get-configuration", "./cfg.json"],
            vec!["digital-paper-cli", "set-configuration", "./cfg.json"],
            vec!["digital-paper-cli", "set-datetime"],
            vec!["digital-paper-cli", "list-templates"],
            vec![
                "digital-paper-cli",
                "upload-template",
                "./template.pdf",
                "Template/template.pdf",
            ],
            vec!["digital-paper-cli", "delete-template", "daily"],
            vec![
                "digital-paper-cli",
                "display-document",
                "doc-id",
                "--page",
                "2",
            ],
            vec!["digital-paper-cli", "screenshot", "/tmp/out.jpg"],
            vec!["digital-paper-cli", "ping"],
            vec!["digital-paper-cli", "update-firmware", "./FwUpdater.pkg"],
            vec![
                "digital-paper-cli",
                "import-credentials",
                "--device-id-path",
                "./deviceid.dat",
                "--private-key-path",
                "./privatekey.dat",
            ],
            vec!["digital-paper-cli", "capabilities"],
        ];

        for case in cases {
            Cli::try_parse_from(case).unwrap();
        }
    }
}
