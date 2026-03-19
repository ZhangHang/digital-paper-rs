use anyhow::Result;
use digital_paper_domain::{
    DeviceStatus, DeviceSummary, RemoteEntry, RemoteEntryType, DEFAULT_DEVICE_HOST,
};
use digital_paper_provider::{rust_native_provider, ProviderRef};
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
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
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
    error: Option<String>,
    busy_modal: Option<TransferModalState>,
    seen_refresh_nonce: u64,
    pending_auto_open: Option<DeviceSummary>,
    pending_auto_pair_addr: Option<String>,
    scanning: bool,
    last_scan_at: Instant,
}

impl LauncherView {
    const DEFAULT_ADDR: &'static str = DEFAULT_DEVICE_HOST;

    fn new(shared: Shared, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            shared,
            error: None,
            busy_modal: None,
            seen_refresh_nonce: 0,
            pending_auto_open: None,
            pending_auto_pair_addr: None,
            scanning: false,
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
        self.pending_auto_pair_addr = None;
        let provider = self.shared.lock().provider.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let addr = Self::DEFAULT_ADDR.to_string();
            let discovered = provider.connect(Some(addr.clone()), None, None);
            this.update(cx, |this, cx| {
                this.scanning = false;
                match discovered {
                    Ok(device) if device.paired => {
                        this.error = None;
                        this.pending_auto_open = Some(device);
                        this.pending_auto_pair_addr = None;
                    }
                    Ok(_) => {
                        this.error = None;
                        this.pending_auto_open = None;
                        this.pending_auto_pair_addr = Some(addr);
                    }
                    Err(err) => {
                        let message = err.to_string();
                        if message == "pairing required"
                            || message == "authentication failed"
                            || message == "device not found"
                        {
                            this.error = None;
                            this.pending_auto_open = None;
                            this.pending_auto_pair_addr = Some(addr);
                        } else {
                            this.error = Some(format!(
                                "Could not reach {}: {}",
                                Self::DEFAULT_ADDR,
                                message
                            ));
                            this.pending_auto_open = None;
                            this.pending_auto_pair_addr = None;
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

}

impl Render for LauncherView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_external_refresh(cx);
        cx.on_next_frame(window, |this, _window, cx| {
            if !this.scanning
                && this.pending_auto_open.is_none()
                && this.pending_auto_pair_addr.is_none()
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
        if let Some(addr) = self.pending_auto_pair_addr.take() {
            let shared = self.shared.clone();
            cx.on_next_frame(window, move |this, window, cx| {
                match open_add_device_window(shared.clone(), Some(addr.clone()), true, cx) {
                    Ok(_) => window.remove_window(),
                    Err(err) => {
                        this.error = Some(format!("Could not start pairing for {}: {}", addr, err));
                        cx.notify();
                    }
                }
            });
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .on_action(|_: &CloseWindowMenu, window, _| window.remove_window())
            .p_6()
            .gap_5()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Digital Paper"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Only {} is supported in launcher mode.",
                                Self::DEFAULT_ADDR
                            )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_lg()
                    .p_5()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_3()
                    .bg(cx.theme().secondary)
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .bg(cx.theme().accent)
                            .text_color(cx.theme().accent_foreground)
                            .child(if self.scanning { "Connecting" } else { "Ready" }),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if self.scanning {
                                "Connecting to device..."
                            } else {
                                "Open or pair your Digital Paper device."
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Launcher uses only {}. If credentials are missing, pairing starts automatically.",
                                Self::DEFAULT_ADDR
                            )),
                    )
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Target: {}", Self::DEFAULT_ADDR)),
                    ),
            )
            .when_some(self.error.clone(), |view, err| {
                view.child(
                    div()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().danger)
                        .bg(cx.theme().danger)
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().danger_foreground)
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
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .pt_4()
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
                                    .bg(cx.theme().accent)
                                    .text_color(cx.theme().accent_foreground)
                                    .text_sm()
                                    .child(format!("{row_count} items")),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mx_4()
                    .mb_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().list)
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
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(div().w(px((depth as f32) * 12.0)))
                                                    .child(
                                                        div()
                                                            .w(px(20.0))
                                                            .text_sm()
                                                            .child(icon_text),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .child(
                                                                wrap_name_for_ui(&entry.name),
                                                            ),
                                                    ),
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
                    .mx_4()
                    .mb_3()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
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

fn wrap_name_for_ui(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 16);
    for ch in name.chars() {
        out.push(ch);
        if matches!(ch, '_' | '-' | '.' | '/' | '，' | '。' | '：' | '；') {
            out.push('\u{200B}');
        }
    }
    out
}

fn default_device_addr() -> String {
    std::env::var("DPT_DEFAULT_ADDR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_DEVICE_HOST.to_string())
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
        let default_ip = initial_ip.unwrap_or_else(default_device_addr);
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
            .p_6()
            .gap_5()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
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
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .bg(cx.theme().secondary)
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
                        .bg(cx.theme().accent)
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
                        .bg(cx.theme().secondary)
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

fn render_busy_overlay(_cx: &mut App, modal: TransferModalState) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(gpui::transparent_black())
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(380.0))
                .p_5()
                .rounded_lg()
                .border_1()
                .border_color(_cx.theme().border)
                .bg(_cx.theme().popover)
                .text_color(_cx.theme().popover_foreground)
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
                        .text_color(_cx.theme().muted_foreground)
                        .child(modal.detail),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(_cx.theme().muted_foreground)
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
