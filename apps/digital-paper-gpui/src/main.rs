use anyhow::Result;
use digital_paper_domain::{DeviceStatus, DeviceSummary, RemoteEntry, RemoteEntryType};
use digital_paper_provider::{rust_native_provider, ProviderRef};
use if_addrs::IfAddr;
use gpui::{
    actions, div, prelude::FluentBuilder, px, size, App, AppContext, Application, Bounds,
    ClickEvent, Context, ExternalPaths, InteractiveElement, IntoElement, KeyBinding, Menu,
    MenuItem, ParentElement, Render, Styled, Window, WindowBounds, WindowOptions,
};
use gpui_component::{
    button::Button, button::ButtonVariants, init as init_components, input::Input,
    input::InputState, list::ListItem, menu::ContextMenuExt, menu::PopupMenuItem,
    scroll::ScrollableElement, ActiveTheme, Root,
};
use parking_lot::Mutex;
use reqwest::blocking::Client;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    net::Ipv4Addr,
    path::PathBuf,
    process::Command,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct SharedState {
    provider: ProviderRef,
    selected_device: Option<DeviceSummary>,
    selected_entry: Option<RemoteEntry>,
    pending_rename: bool,
    launcher_refresh_nonce: u64,
}

type Shared = Arc<Mutex<SharedState>>;

actions!(
    dpt_gpui,
    [
        OpenLauncherMenu,
        OpenAddDeviceMenu,
        RenameSelectionMenu,
        CloseWindowMenu,
        QuitApp
    ]
);

fn main() {
    let app = Application::new();
    let reopen_shared: Rc<RefCell<Option<Shared>>> = Rc::new(RefCell::new(None));
    {
        let reopen_shared = reopen_shared.clone();
        app.on_reopen(move |cx| {
            if cx.windows().is_empty() {
                if let Some(shared) = reopen_shared.borrow().clone() {
                    let _ = open_launcher_window(shared, cx);
                }
            }
        });
    }

    app.run(move |cx: &mut App| {
        init_components(cx);

        let provider = rust_native_provider();

        let shared = Arc::new(Mutex::new(SharedState {
            provider,
            selected_device: None,
            selected_entry: None,
            pending_rename: false,
            launcher_refresh_nonce: 0,
        }));
        *reopen_shared.borrow_mut() = Some(shared.clone());

        {
            let shared = shared.clone();
            cx.on_action(move |_: &OpenLauncherMenu, cx| {
                let _ = open_launcher_window(shared.clone(), cx);
            });
        }
        {
            let shared = shared.clone();
            cx.on_action(move |_: &OpenAddDeviceMenu, cx| {
                let _ = open_add_device_window(shared.clone(), None, false, cx);
            });
        }
        {
            let shared = shared.clone();
            cx.on_action(move |_: &RenameSelectionMenu, _cx| {
                shared.lock().pending_rename = true;
            });
        }
        cx.on_action(|_: &CloseWindowMenu, cx| {
            if let Some(window) = cx.active_window() {
                let _ = window.update(cx, |_, window, _| {
                    window.remove_window();
                });
            }
        });
        cx.on_action(|_: &QuitApp, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-n", OpenLauncherMenu, None),
            KeyBinding::new("cmd-shift-n", OpenAddDeviceMenu, None),
            KeyBinding::new("cmd-shift-r", RenameSelectionMenu, None),
            KeyBinding::new("cmd-w", CloseWindowMenu, None),
            KeyBinding::new("cmd-q", QuitApp, None),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "Digital Paper".into(),
                items: vec![
                    MenuItem::action("Close Window", CloseWindowMenu),
                    MenuItem::separator(),
                    MenuItem::action("Quit Digital Paper", QuitApp),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![MenuItem::action("Rename Selected", RenameSelectionMenu)],
            },
        ]);
        cx.set_dock_menu(vec![MenuItem::action("Quit Digital Paper", QuitApp)]);

        open_launcher_window(shared.clone(), cx).expect("failed to open launcher");
        cx.activate(true);
    });
}

struct LauncherView {
    shared: Shared,
    devices: Vec<DeviceSummary>,
    error: Option<String>,
    usb_hint: Option<String>,
    usb_candidate: Option<String>,
    usb_attached: bool,
    busy_modal: Option<TransferModalState>,
    seen_refresh_nonce: u64,
    pending_auto_open: Option<DeviceSummary>,
    scanning: bool,
    anim_tick: u64,
    last_scan_at: Instant,
}

impl LauncherView {
    fn new(shared: Shared, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            shared,
            devices: Vec::new(),
            error: None,
            usb_hint: None,
            usb_candidate: None,
            usb_attached: false,
            busy_modal: None,
            seen_refresh_nonce: 0,
            pending_auto_open: None,
            scanning: false,
            anim_tick: 0,
            last_scan_at: Instant::now() - Duration::from_secs(10),
        };
        this.request_scan(_cx);
        this
    }

    fn request_scan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.last_scan_at = Instant::now();
        self.error = None;
        self.usb_hint = None;
        self.usb_candidate = None;
        self.usb_attached = false;
        let provider = self.shared.lock().provider.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let discovered = provider.discover_devices();
            let usb = match &discovered {
                Ok(devices) if !devices.is_empty() => None,
                Ok(_) => Some(discover_usb_pair_target()),
                Err(err) if err.to_string() == "device not found" => Some(discover_usb_pair_target()),
                Err(_) => None,
            };
            let attached_usb = detect_attached_usb_dpt();
            this.update(cx, |this, cx| {
                this.scanning = false;
                match discovered {
                    Ok(devices) if !devices.is_empty() => {
                        let selected = devices
                            .iter()
                            .find(|device| device.paired)
                            .cloned()
                            .or_else(|| devices.first().cloned());
                        this.error = None;
                        this.usb_hint = None;
                        this.usb_candidate = None;
                        this.usb_attached = false;
                        this.devices = selected.into_iter().collect();
                        this.pending_auto_open =
                            this.devices.first().filter(|d| d.paired).cloned();
                    }
                    Ok(_) => {
                        match usb.unwrap_or(UsbPairDiscovery::NoDptEndpoint) {
                            UsbPairDiscovery::Found(addr) => {
                                this.error = None;
                                this.usb_hint = Some(
                                    "A Digital Paper device was found over USB. Start pairing to continue."
                                        .into(),
                                );
                                this.usb_candidate = Some(addr);
                                this.usb_attached = true;
                                this.devices.clear();
                                this.pending_auto_open = None;
                            }
                            UsbPairDiscovery::NoUsbInterface | UsbPairDiscovery::NoDptEndpoint => {
                                this.error = None;
                                this.usb_attached = attached_usb;
                                this.usb_hint = attached_usb.then_some(
                                    "Sony DPT-RP1 is attached over USB, but the USB network link is not active on this Mac."
                                        .into(),
                                );
                                this.usb_candidate = None;
                                this.devices.clear();
                                this.pending_auto_open = None;
                            }
                        }
                    }
                    Err(err) => {
                        let message = err.to_string();
                        if message == "device not found" {
                            match usb.unwrap_or(UsbPairDiscovery::NoDptEndpoint) {
                                UsbPairDiscovery::Found(addr) => {
                                    this.error = None;
                                    this.usb_hint = Some(
                                        "A Digital Paper device was found over USB. Start pairing to continue."
                                            .into(),
                                    );
                                    this.usb_candidate = Some(addr);
                                    this.usb_attached = true;
                                    this.devices.clear();
                                    this.pending_auto_open = None;
                                }
                                UsbPairDiscovery::NoUsbInterface
                                | UsbPairDiscovery::NoDptEndpoint => {
                                    this.error = None;
                                    this.usb_attached = attached_usb;
                                    this.usb_hint = attached_usb.then_some(
                                        "Sony DPT-RP1 is attached over USB, but the USB network link is not active on this Mac."
                                            .into(),
                                    );
                                    this.usb_candidate = None;
                                    this.devices.clear();
                                    this.pending_auto_open = None;
                                }
                            }
                        } else {
                            this.error = Some(message);
                            this.usb_hint = None;
                            this.usb_candidate = None;
                            this.usb_attached = false;
                            this.devices.clear();
                            this.pending_auto_open = None;
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn sync_external_refresh(&mut self, cx: &mut Context<Self>) {
        let nonce = self.shared.lock().launcher_refresh_nonce;
        if nonce == self.seen_refresh_nonce {
            return;
        }
        self.seen_refresh_nonce = nonce;
        self.request_scan(cx);
    }

    fn on_open_device(
        &mut self,
        device: DeviceSummary,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_open_device(device, window, cx);
    }

    fn begin_open_device(
        &mut self,
        device: DeviceSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy_modal = Some(TransferModalState {
            title: "Connecting".into(),
            detail: format!("Connecting to {}…", device.name),
        });
        let shared = self.shared.clone();
        cx.notify();
        cx.on_next_frame(window, move |this, window, cx| {
            let connected_device = {
                let mut shared_state = shared.lock();
                shared_state.selected_device = Some(device.clone());
                shared_state.provider.connect(
                    device.reachable_addrs.first().cloned(),
                    device.serial.clone(),
                    device.transport_kinds.first().cloned(),
                )
            };
            match connected_device {
                Ok(connected_device) if connected_device.paired => {
                    let validation = shared
                        .lock()
                        .provider
                        .validate_access("Document".to_string());
                    if let Err(err) = validation {
                        this.error = Some(format!(
                            "This address was discovered, but the DPT library could not be opened: {}",
                            err
                        ));
                        this.busy_modal = None;
                        cx.notify();
                        return;
                    }
                    if let Err(err) = open_browser_window(shared.clone(), connected_device, cx) {
                        this.error = Some(err.to_string());
                        this.busy_modal = None;
                        cx.notify();
                        return;
                    }
                    window.remove_window();
                }
                Ok(connected_device) => {
                    let addr = connected_device
                        .reachable_addrs
                        .first()
                        .cloned()
                        .unwrap_or_else(|| {
                            device
                                .reachable_addrs
                                .first()
                                .cloned()
                                .unwrap_or_default()
                        });
                    match open_add_device_window(shared.clone(), Some(addr.clone()), true, cx) {
                        Ok(_) => {
                            if let Some(window) = cx.active_window() {
                                let _ = window.update(cx, |_, window, _| {
                                    window.remove_window();
                                });
                            }
                        }
                        Err(err) => {
                            this.error = Some(format!(
                                "Could not start pairing for {}: {}",
                                addr, err
                            ));
                            this.busy_modal = None;
                            cx.notify();
                            return;
                        }
                    }
                }
                Err(err) => {
                    this.error = Some(err.to_string());
                    this.busy_modal = None;
                    cx.notify();
                    return;
                }
            }
            this.busy_modal = None;
            cx.notify();
        });
    }

    fn on_start_usb_pair(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(addr) = self.usb_candidate.clone() else {
            self.usb_hint = Some("No USB device is ready to pair.".into());
            cx.notify();
            return;
        };
        self.error = None;
        self.usb_hint = None;
        if let Err(err) = open_add_device_window(self.shared.clone(), Some(addr), true, cx) {
            self.error = Some(err.to_string());
            cx.notify();
        }
    }

    fn on_open_add_device(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        if let Err(err) = open_add_device_window(self.shared.clone(), None, false, cx) {
            self.error = Some(err.to_string());
            cx.notify();
        }
    }

    fn scanning_label(&self) -> &'static str {
        match self.anim_tick % 4 {
            0 => "Looking for your Digital Paper device",
            1 => "Looking for your Digital Paper device.",
            2 => "Looking for your Digital Paper device..",
            _ => "Looking for your Digital Paper device...",
        }
    }
}

impl Render for LauncherView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_external_refresh(cx);
        cx.on_next_frame(window, |this, _window, cx| {
            this.anim_tick = this.anim_tick.wrapping_add(1);
            if !this.scanning
                && this.pending_auto_open.is_none()
                && this.last_scan_at.elapsed() >= Duration::from_secs(3)
            {
                this.request_scan(cx);
            } else {
                cx.notify();
            }
        });
        if let Some(device) = self.pending_auto_open.take() {
            cx.on_next_frame(window, move |this, window, cx| {
                this.begin_open_device(device.clone(), window, cx);
            });
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .on_action(|_: &CloseWindowMenu, window, _| window.remove_window())
            .p_5()
            .gap_4()
            .bg(cx.theme().background)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Digital Paper"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Connect once, then drop straight into your document library.",
                            ),
                    ),
            )
            .child(if self.devices.is_empty() {
                div()
                    .flex_1()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .p_5()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_2()
                    .bg(gpui::hsla(0.58, 0.18, 0.16, 0.42))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if self.usb_candidate.is_some() {
                                "Digital Paper device found over USB."
                            } else if self.usb_attached {
                                "Digital Paper is attached over USB."
                            } else {
                                self.scanning_label()
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.usb_candidate.is_some() {
                                "Start pairing to use this device."
                            } else if self.usb_attached {
                                "The device is visible over USB hardware, but macOS has not exposed the USB network endpoint."
                            } else {
                                "Connect over Wi-Fi or USB. The scanner keeps looking automatically."
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                Button::new("open-add-device-empty")
                                    .primary()
                                    .label("Add Device Manually")
                                    .on_click(cx.listener(Self::on_open_add_device)),
                            )
                            .when(self.usb_candidate.is_some(), |view| {
                                view.child(
                                    Button::new("start-usb-pair")
                                        .outline()
                                        .label("Start Pairing")
                                        .on_click(cx.listener(Self::on_start_usb_pair)),
                                )
                            }),
                    )
            } else {
                div()
                    .flex_1()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .bg(gpui::hsla(0.58, 0.18, 0.16, 0.26))
                    .child(
                        div()
                            .px_2()
                            .pt_1()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Available Device"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Open the paired device below or add another one manually."),
                            ),
                    )
                    .children(self.devices.iter().enumerate().map(|(index, device)| {
                        let device_clone = device.clone();
                        ListItem::new(("open-device-row", index))
                            .selected(false)
                            .on_click(cx.listener(move |this, e, window, cx| {
                                this.on_open_device(device_clone.clone(), e, window, cx);
                            }))
                            .flex()
                            .flex_row()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .truncate()
                                            .child(device.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                device
                                                    .reachable_addrs
                                                    .first()
                                                    .cloned()
                                                    .unwrap_or_else(|| "unknown".into()),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(gpui::hsla(
                                        0.4,
                                        0.24,
                                        if device.paired { 0.34 } else { 0.24 },
                                        0.55,
                                    ))
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(if device.paired { "Paired" } else { "Detected" }),
                            )
                    }))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Shortcuts: Cmd-N launcher, Cmd-Shift-N manual setup"),
                    )
                    .child(
                        Button::new("open-add-device-footer")
                            .outline()
                            .compact()
                            .label("Add Device")
                            .on_click(cx.listener(Self::on_open_add_device)),
                    ),
            )
            .when_some(self.usb_hint.clone(), |view, msg| {
                view.child(
                    div()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(gpui::hsla(0.4, 0.24, 0.24, 0.2))
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(msg),
                )
            })
            .when_some(self.error.clone(), |view, err| {
                view.child(
                    div()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(gpui::hsla(0.0, 0.28, 0.22, 0.2))
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(err),
                )
            })
            .when_some(self.busy_modal.clone(), |view, modal| {
                view.child(render_busy_overlay(cx, modal))
            })
    }
}

struct BrowserView {
    shared: Shared,
    device: DeviceSummary,
    root_path: String,
    children_cache: HashMap<String, Vec<RemoteEntry>>,
    expanded_folders: HashSet<String>,
    status: Option<DeviceStatus>,
    message: Option<String>,
    selected_path: Option<String>,
    rename_target: Option<String>,
    rename_input: gpui::Entity<InputState>,
    transfer_modal: Option<TransferModalState>,
}

#[derive(Clone)]
struct TransferModalState {
    title: String,
    detail: String,
}

enum TransferOperation {
    Import {
        paths: Vec<PathBuf>,
        folder: String,
    },
    Export {
        entry: RemoteEntry,
        save_path: PathBuf,
    },
    Open {
        entry: RemoteEntry,
        local_path: PathBuf,
    },
}

struct TransferOutcome {
    message: Option<String>,
    should_reload: bool,
}

impl BrowserView {
    fn new(
        shared: Shared,
        device: DeviceSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("New name"));
        let mut this = Self {
            shared,
            device,
            root_path: "Document".into(),
            children_cache: HashMap::new(),
            expanded_folders: HashSet::from(["Document".to_string()]),
            status: None,
            message: None,
            selected_path: None,
            rename_target: None,
            rename_input,
            transfer_modal: None,
        };
        this.reload();
        this
    }

    fn load_children(&mut self, path: &str) -> Vec<RemoteEntry> {
        if let Some(entries) = self.children_cache.get(path) {
            return entries.clone();
        }
        let provider = self.shared.lock().provider.clone();
        let entries = match provider.list_entries(path.to_string()) {
            Ok(entries) => {
                self.message = None;
                entries
            }
            Err(err) => {
                self.message = Some(format!("Error: failed to load {}: {}", path, err));
                Vec::new()
            }
        };
        self.children_cache
            .insert(path.to_string(), entries.clone());
        entries
    }

    fn visible_nodes(&mut self) -> Vec<(RemoteEntry, usize)> {
        fn walk(
            this: &mut BrowserView,
            path: &str,
            depth: usize,
            out: &mut Vec<(RemoteEntry, usize)>,
        ) {
            let entries = this.load_children(path);
            for entry in entries {
                out.push((entry.clone(), depth));
                if entry.entry_type == RemoteEntryType::Folder
                    && this.expanded_folders.contains(&entry.path)
                {
                    walk(this, &entry.path, depth + 1, out);
                }
            }
        }

        let mut nodes = Vec::new();
        walk(self, &self.root_path.clone(), 0, &mut nodes);
        nodes
    }

    fn reload(&mut self) {
        self.children_cache.clear();
        let root = self.root_path.clone();
        let _ = self.load_children(&root);
        let provider = self.shared.lock().provider.clone();
        self.status = provider.device_info().ok();
    }

    fn maybe_handle_pending_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected_entry = {
            let mut shared = self.shared.lock();
            if !shared.pending_rename {
                return;
            }
            shared.pending_rename = false;
            shared.selected_entry.clone()
        };

        if let Some(entry) = selected_entry {
            self.on_begin_rename(entry.path, entry.name, window, cx);
        } else {
            self.message = Some("Select a file or folder first, then use Rename.".into());
            cx.notify();
        }
    }

    fn on_row_click(
        &mut self,
        entry: RemoteEntry,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_path = Some(entry.path.clone());
        self.shared.lock().selected_entry = Some(entry.clone());
        if entry.entry_type == RemoteEntryType::Folder {
            if self.expanded_folders.contains(&entry.path) {
                self.expanded_folders.remove(&entry.path);
            } else {
                self.expanded_folders.insert(entry.path.clone());
                let _ = self.load_children(&entry.path);
            }
        } else if event.click_count() >= 2 {
            self.on_open_entry(entry, window, cx);
            return;
        }
        cx.notify();
    }

    fn perform_transfer_sync(provider: ProviderRef, operation: TransferOperation) -> TransferOutcome {
        match operation {
            TransferOperation::Import { paths, folder } => {
                let mut uploaded = 0usize;
                let mut failed = 0usize;
                for path in paths {
                    if !path.is_file() {
                        continue;
                    }
                    let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
                        failed += 1;
                        continue;
                    };
                    let remote = format!("{}/{}", folder.trim_end_matches('/'), name);
                    match provider.upload(path.to_string_lossy().to_string(), remote) {
                        Ok(_) => uploaded += 1,
                        Err(_) => failed += 1,
                    }
                }
                if failed > 0 {
                    TransferOutcome {
                        message: Some(format!(
                            "Error: import completed with {} failed file(s).",
                            failed
                        )),
                        should_reload: uploaded > 0,
                    }
                } else if uploaded == 0 {
                    TransferOutcome {
                        message: Some("Error: no files were imported.".to_string()),
                        should_reload: false,
                    }
                } else {
                    TransferOutcome {
                        message: None,
                        should_reload: true,
                    }
                }
            }
            TransferOperation::Export { entry, save_path } => {
                match provider.download(entry.path, save_path.to_string_lossy().to_string()) {
                    Ok(_) => TransferOutcome {
                        message: None,
                        should_reload: false,
                    },
                    Err(err) => TransferOutcome {
                        message: Some(format!("Error: export failed: {err}")),
                        should_reload: false,
                    },
                }
            }
            TransferOperation::Open { entry, local_path } => {
                match provider.download(entry.path, local_path.to_string_lossy().to_string()) {
                    Ok(_) => {
                        let result = Command::new("open").arg(&local_path).status();
                        match result {
                            Ok(status) if status.success() => TransferOutcome {
                                message: None,
                                should_reload: false,
                            },
                            Ok(status) => TransferOutcome {
                                message: Some(format!(
                                    "Error: open failed with status {}",
                                    status.code().unwrap_or(-1)
                                )),
                                should_reload: false,
                            },
                            Err(err) => TransferOutcome {
                                message: Some(format!("Error: open failed: {err}")),
                                should_reload: false,
                            },
                        }
                    }
                    Err(err) => TransferOutcome {
                        message: Some(format!("Error: download failed: {err}")),
                        should_reload: false,
                    },
                }
            }
        }
    }

    fn start_transfer(
        &mut self,
        title: impl Into<String>,
        detail: impl Into<String>,
        operation: TransferOperation,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transfer_modal = Some(TransferModalState {
            title: title.into(),
            detail: detail.into(),
        });
        let provider = self.shared.lock().provider.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = BrowserView::perform_transfer_sync(provider, operation);
            this.update(cx, |this, cx| {
                if outcome.should_reload {
                    this.reload();
                }
                this.message = outcome.message;
                this.transfer_modal = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_external_drop_root(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target_folder();
        let files = paths.paths().to_vec();
        if files.is_empty() {
            return;
        }
        self.start_transfer(
            "Importing Files",
            format!("Uploading {} file(s) to {}…", files.len(), target),
            TransferOperation::Import {
                paths: files,
                folder: target,
            },
            window,
            cx,
        );
    }

    fn on_external_drop_folder(
        &mut self,
        folder: String,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files = paths.paths().to_vec();
        if files.is_empty() {
            return;
        }
        self.start_transfer(
            "Importing Files",
            format!("Uploading {} file(s) to {}…", files.len(), folder),
            TransferOperation::Import {
                paths: files,
                folder,
            },
            window,
            cx,
        );
    }

    fn drop_target_for_entry(&self, entry: &RemoteEntry) -> String {
        match entry.entry_type {
            RemoteEntryType::Folder => entry.path.clone(),
            _ => entry
                .path
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_string())
                .unwrap_or_else(|| self.root_path.clone()),
        }
    }

    fn target_folder(&self) -> String {
        if let Some(path) = &self.selected_path {
            if let Some(selected) = &self.shared.lock().selected_entry {
                if selected.path == *path && selected.entry_type == RemoteEntryType::Folder {
                    return selected.path.clone();
                }
            }
        }
        self.root_path.clone()
    }

    fn on_export_entry(&mut self, entry: RemoteEntry, window: &mut Window, cx: &mut Context<Self>) {
        if entry.entry_type == RemoteEntryType::Folder {
            self.message = Some("Error: export only supports files.".into());
            cx.notify();
            return;
        }
        let Some(save_path) = rfd::FileDialog::new()
            .set_file_name(&entry.name)
            .save_file()
        else {
            return;
        };
        let name = entry.name.clone();
        self.start_transfer(
            "Exporting File",
            format!("Saving {}…", name),
            TransferOperation::Export { entry, save_path },
            window,
            cx,
        );
    }

    fn on_open_entry(&mut self, entry: RemoteEntry, window: &mut Window, cx: &mut Context<Self>) {
        if entry.entry_type == RemoteEntryType::Folder {
            self.message = Some("Error: open works for files only.".into());
            cx.notify();
            return;
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let temp_dir = std::env::temp_dir().join("dpt-manager-open");
        if let Err(err) = fs::create_dir_all(&temp_dir) {
            self.message = Some(format!("Error: open failed: cannot create temp dir: {err}"));
            cx.notify();
            return;
        }
        let local_path = temp_dir.join(format!("{}-{}", ts, entry.name));
        let name = entry.name.clone();
        self.start_transfer(
            "Opening File",
            format!("Downloading {}…", name),
            TransferOperation::Open { entry, local_path },
            window,
            cx,
        );
    }

    fn on_begin_rename(
        &mut self,
        path: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rename_target = Some(path);
        self.rename_input
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.message = Some("Enter a new name, then click Apply Rename.".into());
        cx.notify();
    }

    fn on_apply_rename(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.rename_target.clone() else {
            return;
        };
        let new_name = self.rename_input.read(cx).value().trim().to_string();
        if new_name.is_empty() {
            self.message = Some("Error: rename failed: name cannot be empty".into());
            cx.notify();
            return;
        }

        let provider = self.shared.lock().provider.clone();
        self.transfer_modal = Some(TransferModalState {
            title: "Renaming".into(),
            detail: format!("Renaming to {}…", new_name),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = provider.rename_entry(path, new_name);
            this.update(cx, |this, cx| {
                this.transfer_modal = None;
                match result {
                    Ok(_) => {
                        this.rename_target = None;
                        this.reload();
                        this.message = None;
                    }
                    Err(err) => {
                        this.message = Some(format!("Error: rename failed: {err}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_delete_entry(&mut self, path: String, cx: &mut Context<Self>) {
        let provider = self.shared.lock().provider.clone();
        self.transfer_modal = Some(TransferModalState {
            title: "Deleting".into(),
            detail: "Removing the selected item…".into(),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = provider.delete(path);
            this.update(cx, |this, cx| {
                this.transfer_modal = None;
                match result {
                    Ok(_) => {
                        this.reload();
                        this.message = None;
                    }
                    Err(err) => {
                        this.message = Some(format!("Error: delete failed: {err}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn footer_text(&self) -> String {
        let info = if let Some(status) = &self.status {
            let battery = status
                .battery
                .level_percent
                .map(|percent| format!("Battery {}%", percent))
                .unwrap_or_else(|| "Battery unknown".to_string());
            format!(
                "{battery}, Wi-Fi {}, firmware {}, owner {}",
                if status.wifi_enabled { "on" } else { "off" },
                status
                    .firmware_version
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                status
                    .owner
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string())
            )
        } else {
            "Device info unavailable".to_string()
        };

        info
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&self.device.name);
        self.maybe_handle_pending_rename(window, cx);
        let tree_rows = self.visible_nodes();
        let row_count = tree_rows.len();
        let view_entity = cx.entity();

        div()
            .flex()
            .flex_col()
            .size_full()
            .on_action(|_: &CloseWindowMenu, window, _| window.remove_window())
            .on_drop(cx.listener(Self::on_external_drop_root))
            .bg(cx.theme().background)
            .child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .child(self.device.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(self.root_path.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(gpui::hsla(0.58, 0.18, 0.16, 0.36))
                                    .text_sm()
                                    .child(format!("{row_count} items")),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mx_3()
                    .mb_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(gpui::hsla(0.58, 0.18, 0.16, 0.2))
                    .overflow_y_scrollbar()
                    .flex()
                    .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .when(tree_rows.is_empty(), |view| {
                            view.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .items_center()
                                    .py_8()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("This folder is empty"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Drop PDFs here or create content from another folder."),
                                    ),
                            )
                        })
                        .children(tree_rows.into_iter().enumerate().map(
                            |(index, (entry, depth))| {
                                let row_id = ("entry", index);
                                let icon_text = match entry.entry_type {
                                    RemoteEntryType::Folder => "📁",
                                    _ => "📄",
                                };
                                let click_entry = entry.clone();
                                let entry_for_drop = entry.clone();
                                let row = ListItem::new(row_id)
                                    .selected(
                                        self.selected_path.as_deref() == Some(entry.path.as_str()),
                                    )
                                    .on_click(cx.listener(move |this, e, window, cx| {
                                        this.on_row_click(click_entry.clone(), e, window, cx);
                                    }))
                                    .context_menu({
                                        let view_entity = view_entity.clone();
                                        let entry_for_open = entry.clone();
                                        let entry_for_export = entry.clone();
                                        let target_path = entry.path.clone();
                                        let target_name = entry.name.clone();
                                        let target_entry = entry.clone();
                                        move |menu, _, _| {
                                            let mut menu = menu;
                                            if target_entry.entry_type != RemoteEntryType::Folder {
                                                menu = menu
                                                    .item(PopupMenuItem::new("Open").on_click({
                                                        let view_entity = view_entity.clone();
                                                        let entry_for_open = entry_for_open.clone();
                                                        move |_, window, app| {
                                                            view_entity.update(app, |this, cx| {
                                                                this.on_open_entry(
                                                                    entry_for_open.clone(),
                                                                    window,
                                                                    cx,
                                                                );
                                                            });
                                                        }
                                                    }))
                                                    .item(PopupMenuItem::new("Export…").on_click(
                                                        {
                                                            let view_entity = view_entity.clone();
                                                            let entry_for_export =
                                                                entry_for_export.clone();
                                                            move |_, window, app| {
                                                                view_entity.update(
                                                                    app,
                                                                    |this, cx| {
                                                                        this.on_export_entry(
                                                                            entry_for_export
                                                                                .clone(),
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    },
                                                                );
                                                            }
                                                        },
                                                    ));
                                            }
                                            menu.item(PopupMenuItem::new("Rename").on_click({
                                                let view_entity = view_entity.clone();
                                                let target_path = target_path.clone();
                                                let target_name = target_name.clone();
                                                move |_, window, app| {
                                                    view_entity.update(app, |this, cx| {
                                                        this.on_begin_rename(
                                                            target_path.clone(),
                                                            target_name.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                    });
                                                }
                                            }))
                                            .item(
                                                PopupMenuItem::new("Delete").on_click({
                                                    let view_entity = view_entity.clone();
                                                    let target_path = target_path.clone();
                                                    move |_, _, app| {
                                                        view_entity.update(app, |this, cx| {
                                                            this.on_delete_entry(
                                                                target_path.clone(),
                                                                cx,
                                                            );
                                                        });
                                                    }
                                                }),
                                            )
                                        }
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .justify_between()
                                            .w_full()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .gap_1()
                                                    .items_center()
                                                    .child(div().w(px((depth as f32) * 12.0)))
                                                    .child(
                                                        div()
                                                            .w(px(20.0))
                                                            .text_sm()
                                                            .child(icon_text),
                                                    )
                                                    .child(entry.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(match (&entry.entry_type, entry.size) {
                                                        (RemoteEntryType::Folder, _) => {
                                                            "Folder".to_string()
                                                        }
                                                        (_, Some(bytes)) if bytes > 0 => {
                                                            format!(
                                                                "{:.1} MB",
                                                                bytes as f64 / 1_048_576.0
                                                            )
                                                        }
                                                        _ => "File".to_string(),
                                                    }),
                                            ),
                                    );
                                let drop_target = self.drop_target_for_entry(&entry_for_drop);
                                div()
                                    .w_full()
                                    .on_drop(cx.listener(
                                        move |this, paths: &ExternalPaths, window, cx| {
                                            this.on_external_drop_folder(
                                                drop_target.clone(),
                                                paths,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .child(row)
                            },
                        )),
                ),
            )
            .when(self.rename_target.is_some(), |view| {
                view.child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .px_3()
                        .pb_1()
                        .child(Input::new(&self.rename_input))
                        .child(
                            Button::new("apply-rename")
                                .primary()
                                .label("Apply Rename")
                                .on_click(cx.listener(Self::on_apply_rename)),
                        ),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .mx_3()
                    .mb_3()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(gpui::hsla(0.58, 0.18, 0.16, 0.22))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        self.message
                            .clone()
                            .unwrap_or_else(|| self.footer_text()),
                    ),
            )
            .when_some(self.transfer_modal.clone(), |view, modal| {
                view.child(render_busy_overlay(cx, modal))
            })
    }
}

struct AddDeviceView {
    shared: Shared,
    message: Option<String>,
    ip_input: gpui::Entity<InputState>,
    pin_input: gpui::Entity<InputState>,
    connected: Option<DeviceSummary>,
    busy_modal: Option<TransferModalState>,
}

impl AddDeviceView {
    fn new(
        shared: Shared,
        initial_ip: Option<String>,
        auto_begin_pair: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_ip = initial_ip.unwrap_or_else(|| "192.168.1.92".to_string());
        let ip_input = cx.new(|cx| InputState::new(window, cx).default_value(default_ip));
        let pin_input = cx.new(|cx| InputState::new(window, cx).placeholder("PIN from device"));
        let mut this = Self {
            shared,
            message: None,
            ip_input,
            pin_input,
            connected: None,
            busy_modal: None,
        };
        if auto_begin_pair {
            this.begin_pair_for_current_ip(window, cx);
        }
        this
    }

    fn begin_pair_for_current_ip(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let ip = self.ip_input.read(cx).value().trim().to_string();
        if ip.is_empty() {
            self.message = Some("Device IP is required".into());
            cx.notify();
            return;
        }
        let provider = self.shared.lock().provider.clone();
        self.busy_modal = Some(TransferModalState {
            title: "Requesting PIN".into(),
            detail: format!("Asking {} to show a pairing PIN…", ip),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = provider.begin_pair_for_addr(ip.clone());
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.message = Some(
                            "The DPT should now show a PIN. Enter it here, then click Pair."
                                .into(),
                        );
                    }
                    Err(err) => {
                        this.message = Some(format!("Could not start pairing: {err}"));
                    }
                }
                this.busy_modal = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_connect(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ip = self.ip_input.read(cx).value().trim().to_string();
        let provider = self.shared.lock().provider.clone();
        self.busy_modal = Some(TransferModalState {
            title: "Connecting".into(),
            detail: format!("Connecting to {}…", ip),
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = provider.connect(Some(ip.clone()), None, None);
            this.update(cx, |this, cx| {
                match result {
                    Ok(device) => {
                        this.connected = Some(device.clone());
                        this.message = Some(format!("Connected to {}", device.name));
                    }
                    Err(err) => this.message = Some(format!("Connect failed: {err}")),
                }
                this.busy_modal = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_pair(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let pin = self.pin_input.read(cx).value().trim().to_string();
        if pin.is_empty() {
            self.message = Some("PIN is required".into());
            cx.notify();
            return;
        }
        let provider = self.shared.lock().provider.clone();
        self.busy_modal = Some(TransferModalState {
            title: "Pairing Device".into(),
            detail: "Completing the DPT pairing flow…".into(),
        });
        cx.notify();
        let shared = self.shared.clone();
        cx.spawn(async move |this, cx| {
            let result = provider.pair(pin.clone());
            this.update(cx, |this, cx| {
                match result {
                    Ok(device) => {
                        this.connected = Some(device.clone());
                        this.message = Some(format!("Paired with {}", device.name));
                        shared.lock().launcher_refresh_nonce += 1;
                    }
                    Err(err) => this.message = Some(format!("Pair failed: {err}")),
                }
                this.busy_modal = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_open_library(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.busy_modal = Some(TransferModalState {
            title: "Opening Library".into(),
            detail: "Preparing the device browser…".into(),
        });
        let shared = self.shared.clone();
        let already_connected = self.connected.clone();
        let ip = self.ip_input.read(cx).value().trim().to_string();
        cx.notify();
        cx.on_next_frame(window, move |this, window, cx| {
            let device = if let Some(device) = already_connected.clone() {
                device
            } else {
                let provider = shared.lock().provider.clone();
                match provider.connect(Some(ip.clone()), None, None) {
                    Ok(device) => device,
                    Err(err) => {
                        this.message = Some(format!("Open failed: {err}"));
                        this.busy_modal = None;
                        cx.notify();
                        return;
                    }
                }
            };

            {
                let mut shared_state = shared.lock();
                shared_state.selected_device = Some(device.clone());
                shared_state.launcher_refresh_nonce += 1;
            }
            this.busy_modal = None;
            if let Err(err) = open_browser_window(shared.clone(), device, cx) {
                this.message = Some(format!("Open failed: {err}"));
                cx.notify();
                return;
            }
            window.remove_window();
            cx.notify();
        });
    }
}

impl Render for AddDeviceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title("Add Device");
        div()
            .flex()
            .flex_col()
            .size_full()
            .on_action(|_: &CloseWindowMenu, window, _| window.remove_window())
            .p_5()
            .gap_4()
            .bg(cx.theme().background)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Add Device"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Use Wi-Fi for the most reliable path today. Connect first, then pair with the PIN shown on the device.",
                            ),
                    ),
            )
            .child(
                div()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .bg(gpui::hsla(0.58, 0.18, 0.16, 0.26))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Device IP"),
                    )
                    .child(Input::new(&self.ip_input))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Pairing PIN"),
                    )
                    .child(Input::new(&self.pin_input))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                Button::new("connect-ip")
                                    .primary()
                                    .label("Connect")
                                    .on_click(cx.listener(Self::on_connect)),
                            )
                            .child(
                                Button::new("pair-device")
                                    .outline()
                                    .label("Pair")
                                    .on_click(cx.listener(Self::on_pair)),
                            )
                            .child(
                                Button::new("open-library")
                                    .outline()
                                    .label("Open Library")
                                    .on_click(cx.listener(Self::on_open_library)),
                            ),
                    ),
            )
            .when_some(self.connected.clone(), |view, device| {
                view.child(
                    div()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(gpui::hsla(0.4, 0.24, 0.24, 0.2))
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Connected Device"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} ({})",
                                    device.name,
                                    device.reachable_addrs.first().cloned().unwrap_or_default()
                                )),
                        ),
                )
            })
            .when_some(self.message.clone(), |view, msg| {
                view.child(
                    div()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .px_3()
                        .py_2()
                        .text_sm()
                        .child(msg),
                )
            })
            .when_some(self.busy_modal.clone(), |view, modal| {
                view.child(render_busy_overlay(cx, modal))
            })
    }
}

fn render_busy_overlay(cx: &mut App, modal: TransferModalState) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(gpui::hsla(0.58, 0.18, 0.05, 0.52))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(380.0))
                .p_5()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .bg(gpui::hsla(0.58, 0.1, 0.1, 0.98))
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(modal.title),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(modal.detail),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Please wait while Digital Paper completes this step."),
                ),
        )
}

fn open_browser_window(shared: Shared, device: DeviceSummary, cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(540.0), px(560.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            is_resizable: true,
            window_min_size: Some(size(px(460.0), px(460.0))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title(&device.name);
            let view = cx.new(|cx| BrowserView::new(shared.clone(), device.clone(), window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    )?;
    Ok(())
}

fn open_launcher_window(shared: Shared, cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(560.0), px(420.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            is_resizable: true,
            window_min_size: Some(size(px(520.0), px(380.0))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title("Digital Paper");
            let view = cx.new(|cx| LauncherView::new(shared.clone(), window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    )?;
    Ok(())
}

fn open_add_device_window(
    shared: Shared,
    initial_ip: Option<String>,
    auto_begin_pair: bool,
    cx: &mut App,
) -> Result<()> {
    cx.open_window(
        WindowOptions {
            is_resizable: true,
            window_min_size: Some(size(px(640.0), px(420.0))),
            ..Default::default()
        },
        move |window, cx| {
            let view = cx.new(|cx| {
                AddDeviceView::new(
                    shared.clone(),
                    initial_ip.clone(),
                    auto_begin_pair,
                    window,
                    cx,
                )
            });
            cx.new(|cx| Root::new(view, window, cx))
        },
    )?;
    Ok(())
}

fn looks_like_usb_interface(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "en0"
        || lower.starts_with("awdl")
        || lower.starts_with("llw")
        || lower.starts_with("utun")
        || lower.starts_with("lo")
        || lower.starts_with("bridge")
        || lower.starts_with("ap")
        || lower.starts_with("anpi")
    {
        return false;
    }
    lower.starts_with("en")
        || lower.starts_with("eth")
        || lower.starts_with("usb")
        || lower.starts_with("rndis")
        || lower.starts_with("ecm")
        || lower.starts_with("wlanusb")
}

fn usb_hint_addrs() -> (bool, Vec<String>) {
    let ifaces = match if_addrs::get_if_addrs() {
        Ok(ifaces) => ifaces,
        Err(err) => {
            append_debug_log("gpui", &format!("Failed to enumerate interfaces: {err}"));
            return (false, Vec::new());
        }
    };
    let mut saw_usb_interface = false;
    let mut candidates = Vec::new();
    for iface in ifaces {
        if !looks_like_usb_interface(&iface.name) {
            continue;
        }
        let (ip, netmask) = match iface.addr {
            IfAddr::V4(v4) => (v4.ip, v4.netmask),
            IfAddr::V6(_) => continue,
        };
        append_debug_log(
            "gpui",
            &format!("USB-like interface {} ip={} mask={}", iface.name, ip, netmask),
        );
        let Some(iface_candidates) = usb_probe_candidates(ip, netmask) else {
            append_debug_log(
                "gpui",
                &format!("Skipping interface {} because subnet is not probeable", iface.name),
            );
            continue;
        };
        saw_usb_interface = true;
        append_debug_log(
            "gpui",
            &format!(
                "Interface {} produced {} candidate addresses",
                iface.name,
                iface_candidates.len()
            ),
        );
        candidates.extend(iface_candidates);
    }
    (saw_usb_interface, candidates)
}

fn usb_probe_candidates(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<Vec<String>> {
    if ip.is_loopback() {
        return None;
    }

    let oct = ip.octets();
    let is_private = oct[0] == 10
        || (oct[0] == 172 && (16..=31).contains(&oct[1]))
        || (oct[0] == 192 && oct[1] == 168);
    let is_link_local = oct[0] == 169 && oct[1] == 254;
    if !is_private && !is_link_local {
        return None;
    }

    let ip_u32 = u32::from(ip);
    let mask_u32 = u32::from(netmask);
    let network = ip_u32 & mask_u32;
    let broadcast = network | !mask_u32;
    let host_count = broadcast.saturating_sub(network).saturating_sub(1);

    let mut candidates = Vec::new();
    if host_count <= 64 {
        for host in (network.saturating_add(1))..broadcast {
            if host == ip_u32 {
                continue;
            }
            candidates.push(Ipv4Addr::from(host).to_string());
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
            candidates.push(candidate.to_string());
        }
    }

    Some(candidates)
}

fn probe_usb_pair_candidate(addr: &str) -> bool {
    let client = match Client::builder()
        .timeout(Duration::from_millis(900))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    let response = client
        .get(format!("http://{addr}:8080/register/information"))
        .send();

    match response {
        Ok(resp) => {
            let ok = resp.status().is_success();
            append_debug_log(
                "gpui",
                &format!("USB probe {} -> HTTP {} success={ok}", addr, resp.status()),
            );
            ok
        }
        Err(err) => {
            append_debug_log("gpui", &format!("USB probe {addr} failed: {err}"));
            false
        }
    }
}

enum UsbPairDiscovery {
    Found(String),
    NoUsbInterface,
    NoDptEndpoint,
}

fn discover_usb_pair_target() -> UsbPairDiscovery {
    let (saw_usb_interface, candidates) = usb_hint_addrs();
    append_debug_log(
        "gpui",
        &format!(
            "USB discovery saw_usb_interface={} candidate_count={}",
            saw_usb_interface,
            candidates.len()
        ),
    );
    if !saw_usb_interface {
        return UsbPairDiscovery::NoUsbInterface;
    }
    for addr in candidates {
        if probe_usb_pair_candidate(&addr) {
            return UsbPairDiscovery::Found(addr);
        }
    }
    UsbPairDiscovery::NoDptEndpoint
}

fn detect_attached_usb_dpt() -> bool {
    let output = Command::new("ioreg")
        .args(["-p", "IOUSB", "-w", "0", "-l"])
        .output();
    let Ok(output) = output else {
        append_debug_log("gpui", "Failed to run ioreg for USB hardware detection");
        return false;
    };
    if !output.status.success() {
        append_debug_log(
            "gpui",
            &format!("ioreg exited with status {}", output.status),
        );
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    let detected = stdout.contains("dpt-rp1")
        || stdout.contains("dpt_rp1")
        || (stdout.contains("sony") && stdout.contains("324650005030476"));
    append_debug_log(
        "gpui",
        &format!("USB hardware detection attached={detected}"),
    );
    detected
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
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

fn debug_log_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config/dpt/debug.log")
}

#[cfg(test)]
mod tests {
    use super::{looks_like_usb_interface, usb_probe_candidates};
    use std::net::Ipv4Addr;

    #[test]
    fn usb_interface_detection_accepts_common_names() {
        assert!(looks_like_usb_interface("en7"));
        assert!(looks_like_usb_interface("usb0"));
        assert!(looks_like_usb_interface("eth1"));
        assert!(looks_like_usb_interface("rndis0"));
    }

    #[test]
    fn usb_interface_detection_rejects_common_non_usb_names() {
        assert!(!looks_like_usb_interface("en0"));
        assert!(!looks_like_usb_interface("lo0"));
        assert!(!looks_like_usb_interface("bridge100"));
        assert!(!looks_like_usb_interface("utun4"));
    }

    #[test]
    fn usb_probe_candidates_allow_link_local_usb_subnets() {
        let candidates = usb_probe_candidates(
            Ipv4Addr::new(169, 254, 42, 23),
            Ipv4Addr::new(255, 255, 255, 0),
        )
        .expect("link-local subnet should be probed");

        assert!(candidates.contains(&"169.254.42.1".to_string()));
        assert!(candidates.contains(&"169.254.42.254".to_string()));
        assert!(!candidates.contains(&"169.254.42.23".to_string()));
    }
}
