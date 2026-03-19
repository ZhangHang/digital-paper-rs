use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, size, EnterAlternateScreen, LeaveAlternateScreen},
};
use digital_paper_domain::{
    DeviceStatus, DeviceSummary, RemoteEntry, RemoteEntryType, UsbStatusKind, DEFAULT_DEVICE_HOST,
};
use digital_paper_provider::{rust_native_provider, ProviderRef};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::{Color, CrosstermBackend, Line, Modifier, Span, Style},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
    },
    Frame, Terminal,
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn main() -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = run_app(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let provider = rust_native_provider();
    let mut app = TuiApp::new(provider);
    app.scan_devices();

    while !app.should_quit {
        app.poll_background();
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        app.handle_key(key)?;
                    }
                }
                Event::Mouse(mouse) => app.handle_mouse(mouse)?,
                Event::Paste(text) => app.handle_paste(text)?,
                _ => {}
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
struct BrowserState {
    device: DeviceSummary,
    root_path: String,
    children_cache: HashMap<String, Vec<RemoteEntry>>,
    expanded_folders: HashSet<String>,
    status: Option<DeviceStatus>,
    message: Option<String>,
    selected_path: Option<String>,
    selected_row: usize,
    search_query: String,
    search_mode: bool,
    search_results: Option<Vec<(RemoteEntry, usize)>>,
    search_progress: Option<String>,
}

impl BrowserState {
    fn new(device: DeviceSummary) -> Self {
        Self {
            device,
            root_path: "Document".into(),
            children_cache: HashMap::new(),
            expanded_folders: HashSet::from(["Document".to_string()]),
            status: None,
            message: None,
            selected_path: None,
            selected_row: 0,
            search_query: String::new(),
            search_mode: false,
            search_results: None,
            search_progress: None,
        }
    }
}

enum Screen {
    Launcher,
    AddDevice,
    Browser,
}

enum AddField {
    Ip,
    Pin,
}

enum PromptKind {
    Rename { path: String, current_name: String },
    Delete { path: String },
    Import { folder: String },
    Export { entry: RemoteEntry },
}

#[derive(Clone)]
enum BrowserAction {
    OpenOrToggle,
    ImportInto(String),
    Export(RemoteEntry),
    RevealLocalCopy(RemoteEntry),
    Rename(RemoteEntry),
    Delete(RemoteEntry),
    Reload,
    Back,
}

#[derive(Clone)]
struct ActionButton {
    label: String,
    action: BrowserAction,
}

struct ContextMenuState {
    x: u16,
    y: u16,
    selected: usize,
    buttons: Vec<ActionButton>,
}

struct PromptState {
    title: String,
    detail: String,
    input: String,
    kind: PromptKind,
}

enum SearchUpdate {
    Progress {
        query: String,
        current_path: String,
        visited_folders: usize,
    },
    Finished {
        query: String,
        results: Vec<(RemoteEntry, usize)>,
        visited_folders: usize,
    },
    Failed {
        query: String,
        error: String,
    },
}

struct TuiApp {
    provider: ProviderRef,
    screen: Screen,
    should_quit: bool,
    devices: Vec<DeviceSummary>,
    launcher_index: usize,
    launcher_message: Option<String>,
    launcher_error: Option<String>,
    usb_hint: Option<String>,
    usb_candidate: Option<String>,
    usb_attached: bool,
    add_ip: String,
    add_pin: String,
    add_field: AddField,
    add_message: Option<String>,
    add_connected: Option<DeviceSummary>,
    browser: Option<BrowserState>,
    prompt: Option<PromptState>,
    context_menu: Option<ContextMenuState>,
    search_rx: Option<Receiver<SearchUpdate>>,
    last_browser_click: Option<(String, Instant)>,
    last_launcher_click: Option<(usize, Instant)>,
}

impl TuiApp {
    fn new(provider: ProviderRef) -> Self {
        Self {
            provider,
            screen: Screen::Launcher,
            should_quit: false,
            devices: Vec::new(),
            launcher_index: 0,
            launcher_message: None,
            launcher_error: None,
            usb_hint: None,
            usb_candidate: None,
            usb_attached: false,
            add_ip: default_device_addr(),
            add_pin: String::new(),
            add_field: AddField::Ip,
            add_message: None,
            add_connected: None,
            browser: None,
            prompt: None,
            context_menu: None,
            search_rx: None,
            last_browser_click: None,
            last_launcher_click: None,
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        match self.screen {
            Screen::Launcher => self.draw_launcher(frame),
            Screen::AddDevice => self.draw_add_device(frame),
            Screen::Browser => self.draw_browser(frame),
        }
        if let Some(prompt) = &self.prompt {
            draw_prompt(frame, prompt);
        }
        if let Some(menu) = &self.context_menu {
            draw_context_menu(frame, menu);
        }
    }

    fn poll_background(&mut self) {
        let mut disconnected = false;
        let mut updates = Vec::new();
        if let Some(rx) = &self.search_rx {
            loop {
                match rx.try_recv() {
                    Ok(update) => updates.push(update),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        for update in updates {
            self.apply_search_update(update);
        }
        if disconnected {
            self.search_rx = None;
        }
    }

    fn apply_search_update(&mut self, update: SearchUpdate) {
        let Some(browser) = self.browser.as_mut() else {
            return;
        };
        match update {
            SearchUpdate::Progress {
                query,
                current_path,
                visited_folders,
            } => {
                if query == browser.search_query {
                    browser.search_progress = Some(format!(
                        "Searching '{}'... {} folders scanned, now at {}",
                        query, visited_folders, current_path
                    ));
                }
            }
            SearchUpdate::Finished {
                query,
                results,
                visited_folders,
            } => {
                if query == browser.search_query {
                    browser.search_results = Some(results);
                    browser.search_progress = Some(format!(
                        "Search complete for '{}': {} folders scanned.",
                        query, visited_folders
                    ));
                    browser.selected_row = 0;
                }
                self.search_rx = None;
            }
            SearchUpdate::Failed { query, error } => {
                if query == browser.search_query {
                    browser.search_results = Some(Vec::new());
                    browser.search_progress = Some(format!(
                        "Search failed for '{}': {}",
                        query, error
                    ));
                }
                self.search_rx = None;
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }
        if self.context_menu.is_some() {
            return self.handle_context_menu_key(key);
        }
        match self.screen {
            Screen::Launcher => self.handle_launcher_key(key),
            Screen::AddDevice => self.handle_add_device_key(key),
            Screen::Browser => self.handle_browser_key(key),
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        if self.prompt.is_some() {
            return Ok(());
        }
        if self.context_menu.is_some() {
            return self.handle_context_menu_mouse(mouse);
        }
        match self.screen {
            Screen::Launcher => self.handle_launcher_mouse(mouse),
            Screen::AddDevice => Ok(()),
            Screen::Browser => self.handle_browser_mouse(mouse),
        }
    }

    fn handle_paste(&mut self, text: String) -> Result<()> {
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.input.push_str(&normalize_paste_text(&text));
            return Ok(());
        }
        match self.screen {
            Screen::Browser => {
                if let Some(browser) = self.browser.as_mut() {
                    if browser.search_mode {
                        browser.search_query.push_str(&normalize_paste_text(&text));
                        browser.selected_row = 0;
                        self.trigger_search_if_needed();
                        self.sync_browser_selection();
                    }
                }
            }
            Screen::AddDevice => match self.add_field {
                AddField::Ip => self.add_ip.push_str(&normalize_paste_text(&text)),
                AddField::Pin => self.add_pin.push_str(&normalize_paste_text(&text)),
            },
            Screen::Launcher => {}
        }
        Ok(())
    }

    fn handle_launcher_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('r') => self.scan_devices(),
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.devices.is_empty() {
                    self.launcher_index = (self.launcher_index + 1).min(self.devices.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.launcher_index = self.launcher_index.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(device) = self.devices.get(self.launcher_index).cloned() {
                    if device.paired {
                        self.open_browser(device);
                    } else {
                        self.add_ip = device
                            .reachable_addrs
                            .first()
                            .cloned()
                            .unwrap_or_else(|| self.add_ip.clone());
                        self.screen = Screen::AddDevice;
                        self.add_message = Some("Device detected but not paired yet.".into());
                    }
                }
            }
            KeyCode::Char('a') => {
                self.screen = Screen::AddDevice;
                self.add_message = None;
            }
            KeyCode::Char('u') => {
                self.recover_usb_network();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_launcher_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let device_count = self.devices.len();
        if device_count == 0 {
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.launcher_index = (self.launcher_index + 1).min(device_count.saturating_sub(1));
            }
            MouseEventKind::ScrollUp => {
                self.launcher_index = self.launcher_index.saturating_sub(1);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = launcher_hit_index(mouse.row, device_count) {
                    self.launcher_index = index;
                    if let Some(device) = self.devices.get(index).cloned() {
                        let now = Instant::now();
                        let activate = self
                            .last_launcher_click
                            .as_ref()
                            .map(|(last_index, last_at)| {
                                *last_index == index
                                    && now.duration_since(*last_at) <= Duration::from_millis(500)
                            })
                            .unwrap_or(false);
                        self.last_launcher_click = Some((index, now));
                        if activate {
                            if device.paired {
                                self.open_browser(device);
                            } else {
                                self.add_ip = device
                                    .reachable_addrs
                                    .first()
                                    .cloned()
                                    .unwrap_or_else(|| self.add_ip.clone());
                                self.screen = Screen::AddDevice;
                                self.add_message =
                                    Some("Device detected but not paired yet.".into());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_add_device_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.screen = Screen::Launcher;
                self.add_message = None;
            }
            KeyCode::Tab => {
                self.add_field = match self.add_field {
                    AddField::Ip => AddField::Pin,
                    AddField::Pin => AddField::Ip,
                };
            }
            KeyCode::BackTab => {
                self.add_field = match self.add_field {
                    AddField::Ip => AddField::Pin,
                    AddField::Pin => AddField::Ip,
                };
            }
            KeyCode::Backspace => match self.add_field {
                AddField::Ip => {
                    self.add_ip.pop();
                }
                AddField::Pin => {
                    self.add_pin.pop();
                }
            },
            KeyCode::Char('c') => self.connect_current_ip(),
            KeyCode::Char('p') => self.pair_current_pin(),
            KeyCode::Char('o') => {
                if let Some(device) = self.add_connected.clone() {
                    self.open_browser(device);
                } else {
                    self.connect_current_ip();
                    if let Some(device) = self.add_connected.clone() {
                        self.open_browser(device);
                    }
                }
            }
            KeyCode::Enter => match self.add_field {
                AddField::Ip => self.connect_current_ip(),
                AddField::Pin => self.pair_current_pin(),
            },
            KeyCode::Char(ch) => {
                if !ch.is_control() {
                    match self.add_field {
                        AddField::Ip => self.add_ip.push(ch),
                        AddField::Pin => self.add_pin.push(ch),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_browser_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.browser.is_none() {
            self.screen = Screen::Launcher;
            return Ok(());
        }
        if self.browser.as_ref().map(|b| b.search_mode).unwrap_or(false) {
            return self.handle_browser_search_key(key);
        }
        let rows = self.visible_nodes();
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc | KeyCode::Char('b') => {
                self.screen = Screen::Launcher;
                self.scan_devices();
            }
            KeyCode::Char('m') => self.open_browser_context_menu(None, None),
            KeyCode::Char('/') => {
                if let Some(browser) = self.browser.as_mut() {
                    browser.search_mode = true;
                    browser.message = Some(
                        "Recursive search mode. Type to filter, Enter to keep, Esc to exit."
                            .into(),
                    );
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(browser) = self.browser.as_mut() {
                    if !rows.is_empty() {
                        browser.selected_row = (browser.selected_row + 1).min(rows.len() - 1);
                        browser.selected_path = Some(rows[browser.selected_row].0.path.clone());
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(browser) = self.browser.as_mut() {
                    browser.selected_row = browser.selected_row.saturating_sub(1);
                    if let Some((entry, _)) = rows.get(browser.selected_row) {
                        browser.selected_path = Some(entry.path.clone());
                    }
                }
            }
            KeyCode::Char('r') => {
                if let Some(entry) = self.selected_browser_entry() {
                    self.prompt = Some(PromptState {
                        title: "Rename".into(),
                        detail: format!("Rename {}", entry.name),
                        input: entry.name.clone(),
                        kind: PromptKind::Rename {
                            path: entry.path,
                            current_name: entry.name,
                        },
                    });
                } else {
                    self.browser_message("Select a file or folder first.");
                }
            }
            KeyCode::Char('d') => {
                if let Some(entry) = self.selected_browser_entry() {
                    self.prompt = Some(PromptState {
                        title: "Delete".into(),
                        detail: format!("Type DELETE to remove {}", entry.name),
                        input: String::new(),
                        kind: PromptKind::Delete { path: entry.path },
                    });
                }
            }
            KeyCode::Char('i') => {
                let folder = self.target_folder();
                self.prompt = Some(PromptState {
                    title: "Import".into(),
                    detail: format!("Local file or folder to upload into {folder}"),
                    input: String::new(),
                    kind: PromptKind::Import { folder },
                });
            }
            KeyCode::Char('e') => {
                if let Some(entry) = self.selected_browser_entry() {
                    if entry.entry_type == RemoteEntryType::Folder {
                        self.browser_message("Export works for files only.");
                    } else {
                        self.prompt = Some(PromptState {
                            title: "Export".into(),
                            detail: format!("Save path for {}", entry.name),
                            input: entry.name.clone(),
                            kind: PromptKind::Export { entry },
                        });
                    }
                }
            }
            KeyCode::Char('o') => {
                if let Some(entry) = self.selected_browser_entry() {
                    self.open_entry(entry);
                }
            }
            KeyCode::Char('R') => self.reload_browser(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                if let Some(entry) = self.selected_browser_entry() {
                    if entry.entry_type == RemoteEntryType::Folder {
                        if let Some(browser) = self.browser.as_mut() {
                            if browser.expanded_folders.contains(&entry.path) {
                                browser.expanded_folders.remove(&entry.path);
                            } else {
                                browser.expanded_folders.insert(entry.path.clone());
                            }
                        }
                        let _ = self.load_children(&entry.path);
                    } else {
                        self.open_entry(entry);
                    }
                }
            }
            KeyCode::Left => {
                if let Some(entry) = self.selected_browser_entry() {
                    if entry.entry_type == RemoteEntryType::Folder {
                        if let Some(browser) = self.browser.as_mut() {
                            browser.expanded_folders.remove(&entry.path);
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_browser_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let row_count = self.visible_nodes().len();
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                if let Some(browser) = self.browser.as_mut() {
                    if row_count > 0 {
                        browser.selected_row =
                            (browser.selected_row + 1).min(row_count.saturating_sub(1));
                    }
                }
                self.sync_browser_selection();
            }
            MouseEventKind::ScrollUp => {
                if let Some(browser) = self.browser.as_mut() {
                    browser.selected_row = browser.selected_row.saturating_sub(1);
                }
                self.sync_browser_selection();
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let buttons = self.browser_action_buttons();
                if let Some(action) = browser_action_hit(mouse.column, mouse.row, &buttons) {
                    return self.run_browser_action(action);
                }
                if let Some(index) = browser_hit_index(mouse.row, row_count) {
                    let clicked = {
                        let rows = self.visible_nodes();
                        rows.get(index).map(|(entry, _)| entry.clone())
                    };
                    if let Some(entry) = clicked {
                        if let Some(browser) = self.browser.as_mut() {
                            browser.selected_row = index;
                            browser.selected_path = Some(entry.path.clone());
                        }
                        let now = Instant::now();
                        let activate = self
                            .last_browser_click
                            .as_ref()
                            .map(|(last_path, last_at)| {
                                last_path == &entry.path
                                    && now.duration_since(*last_at) <= Duration::from_millis(500)
                            })
                            .unwrap_or(false);
                        self.last_browser_click = Some((entry.path.clone(), now));
                        if activate {
                            if entry.entry_type == RemoteEntryType::Folder {
                                if let Some(browser) = self.browser.as_mut() {
                                    if browser.expanded_folders.contains(&entry.path) {
                                        browser.expanded_folders.remove(&entry.path);
                                    } else {
                                        browser.expanded_folders.insert(entry.path.clone());
                                    }
                                }
                                let _ = self.load_children(&entry.path);
                            } else {
                                self.open_entry(entry);
                            }
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(index) = browser_hit_index(mouse.row, row_count) {
                    let clicked = {
                        let rows = self.visible_nodes();
                        rows.get(index).map(|(entry, _)| entry.clone())
                    };
                    if let Some(entry) = clicked {
                        if let Some(browser) = self.browser.as_mut() {
                            browser.selected_row = index;
                            browser.selected_path = Some(entry.path.clone());
                        }
                        self.open_browser_context_menu(Some(mouse.column), Some(mouse.row));
                    }
                } else if browser_action_hit(
                    mouse.column,
                    mouse.row,
                    &self.browser_action_buttons(),
                )
                .is_some()
                {
                    self.open_browser_context_menu(Some(mouse.column), Some(mouse.row));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_browser_search_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(browser) = self.browser.as_mut() else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => {
                browser.search_mode = false;
                if browser.search_query.is_empty() {
                    browser.message = Some("Search closed.".into());
                    browser.search_results = None;
                    browser.search_progress = None;
                    self.search_rx = None;
                } else {
                    browser.message = Some(format!(
                        "Search kept: {} matching recursively under {}.",
                        browser.search_query, browser.root_path
                    ));
                }
            }
            KeyCode::Enter => {
                browser.search_mode = false;
                browser.message = if browser.search_query.is_empty() {
                    Some("Search cleared.".into())
                } else {
                    Some(format!("Filtered recursively by '{}'.", browser.search_query))
                };
            }
            KeyCode::Backspace => {
                browser.search_query.pop();
                browser.selected_row = 0;
            }
            KeyCode::Char(ch) => {
                if !ch.is_control() {
                    browser.search_query.push(ch);
                    browser.selected_row = 0;
                }
            }
            _ => {}
        }
        self.trigger_search_if_needed();
        let rows = self.visible_nodes();
        if let Some(browser) = self.browser.as_mut() {
            if let Some((entry, _)) = rows.first() {
                browser.selected_path = Some(entry.path.clone());
            } else {
                browser.selected_path = None;
            }
        }
        Ok(())
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(prompt) = self.prompt.as_mut() else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
            }
            KeyCode::Backspace => {
                prompt.input.pop();
            }
            KeyCode::Enter => {
                let prompt = self.prompt.take().expect("prompt exists");
                self.apply_prompt(prompt)?;
            }
            KeyCode::Char(ch) => {
                if !ch.is_control() {
                    prompt.input.push(ch);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_context_menu_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(menu) = self.context_menu.as_mut() else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => {
                self.context_menu = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !menu.buttons.is_empty() {
                    menu.selected = (menu.selected + 1).min(menu.buttons.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                menu.selected = menu.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                let action = menu.buttons.get(menu.selected).map(|b| b.action.clone());
                self.context_menu = None;
                if let Some(action) = action {
                    self.run_browser_action(action)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_context_menu_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let Some(menu) = self.context_menu.as_ref() else {
            return Ok(());
        };
        let area = context_menu_rect(menu);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = menu_hit_index(area, mouse.column, mouse.row, menu.buttons.len())
                {
                    let action = menu.buttons[index].action.clone();
                    self.context_menu = None;
                    self.run_browser_action(action)?;
                } else {
                    self.context_menu = None;
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.context_menu = None;
            }
            MouseEventKind::ScrollDown => {
                if let Some(menu) = self.context_menu.as_mut() {
                    if !menu.buttons.is_empty() {
                        menu.selected = (menu.selected + 1).min(menu.buttons.len() - 1);
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.selected = menu.selected.saturating_sub(1);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_prompt(&mut self, prompt: PromptState) -> Result<()> {
        match prompt.kind {
            PromptKind::Rename { path, current_name } => {
                let new_name = prompt.input.trim();
                if new_name.is_empty() || new_name == current_name {
                    self.browser_message("Rename cancelled.");
                    return Ok(());
                }
                self.provider
                    .rename_entry(path, new_name.to_string())
                    .context("rename failed")?;
                self.reload_browser();
                self.browser_message("Rename applied.");
            }
            PromptKind::Delete { path } => {
                if prompt.input.trim() != "DELETE" {
                    self.browser_message("Delete cancelled.");
                    return Ok(());
                }
                self.provider.delete(path).context("delete failed")?;
                self.reload_browser();
                self.browser_message("Deleted entry.");
            }
            PromptKind::Import { folder } => {
                let path = PathBuf::from(prompt.input.trim());
                let uploaded = import_path(&self.provider, &path, &folder)?;
                self.reload_browser();
                self.browser_message(format!("Imported {uploaded} file(s)."));
            }
            PromptKind::Export { entry } => {
                let path = PathBuf::from(prompt.input.trim());
                if path.as_os_str().is_empty() {
                    self.browser_message("Export cancelled.");
                    return Ok(());
                }
                self.provider
                    .download(entry.path, path.to_string_lossy().to_string())
                    .context("export failed")?;
                self.browser_message("File exported.");
            }
        }
        Ok(())
    }

    fn open_browser_context_menu(&mut self, x: Option<u16>, y: Option<u16>) {
        let buttons = self.browser_action_buttons();
        if buttons.is_empty() {
            return;
        }
        self.context_menu = Some(ContextMenuState {
            x: x.unwrap_or(8),
            y: y.unwrap_or(8),
            selected: 0,
            buttons,
        });
    }

    fn browser_action_buttons(&mut self) -> Vec<ActionButton> {
        let mut buttons = Vec::new();
        buttons.push(ActionButton {
            label: "Back".into(),
            action: BrowserAction::Back,
        });
        buttons.push(ActionButton {
            label: "Reload".into(),
            action: BrowserAction::Reload,
        });

        let target_folder = self.target_folder();
        buttons.push(ActionButton {
            label: "Import".into(),
            action: BrowserAction::ImportInto(target_folder),
        });

        if let Some(entry) = self.selected_browser_entry() {
            let is_folder = entry.entry_type == RemoteEntryType::Folder;
            buttons.insert(
                0,
                ActionButton {
                    label: if is_folder { "Toggle" } else { "Open" }.into(),
                    action: BrowserAction::OpenOrToggle,
                },
            );
            buttons.push(ActionButton {
                label: "Rename".into(),
                action: BrowserAction::Rename(entry.clone()),
            });
            buttons.push(ActionButton {
                label: "Delete".into(),
                action: BrowserAction::Delete(entry.clone()),
            });
            if !is_folder {
                buttons.push(ActionButton {
                    label: "Export".into(),
                    action: BrowserAction::Export(entry.clone()),
                });
                buttons.push(ActionButton {
                    label: "Reveal".into(),
                    action: BrowserAction::RevealLocalCopy(entry),
                });
            }
        }

        buttons
    }

    fn run_browser_action(&mut self, action: BrowserAction) -> Result<()> {
        match action {
            BrowserAction::OpenOrToggle => {
                if let Some(entry) = self.selected_browser_entry() {
                    if entry.entry_type == RemoteEntryType::Folder {
                        if let Some(browser) = self.browser.as_mut() {
                            if browser.expanded_folders.contains(&entry.path) {
                                browser.expanded_folders.remove(&entry.path);
                            } else {
                                browser.expanded_folders.insert(entry.path.clone());
                            }
                        }
                        let _ = self.load_children(&entry.path);
                    } else {
                        self.open_entry(entry);
                    }
                }
            }
            BrowserAction::ImportInto(folder) => {
                self.prompt = Some(PromptState {
                    title: "Import".into(),
                    detail: format!(
                        "Local file or folder to upload into {folder}. You can paste or drop a path here."
                    ),
                    input: String::new(),
                    kind: PromptKind::Import { folder },
                });
            }
            BrowserAction::Export(entry) => {
                self.prompt = Some(PromptState {
                    title: "Export".into(),
                    detail: format!(
                        "Save path for {}. You can paste or drop a destination path here.",
                        entry.name
                    ),
                    input: entry.name.clone(),
                    kind: PromptKind::Export { entry },
                });
            }
            BrowserAction::RevealLocalCopy(entry) => {
                self.reveal_local_copy(entry);
            }
            BrowserAction::Rename(entry) => {
                self.prompt = Some(PromptState {
                    title: "Rename".into(),
                    detail: format!("Rename {}", entry.name),
                    input: entry.name.clone(),
                    kind: PromptKind::Rename {
                        path: entry.path,
                        current_name: entry.name,
                    },
                });
            }
            BrowserAction::Delete(entry) => {
                self.prompt = Some(PromptState {
                    title: "Delete".into(),
                    detail: format!("Type DELETE to remove {}", entry.name),
                    input: String::new(),
                    kind: PromptKind::Delete { path: entry.path },
                });
            }
            BrowserAction::Reload => self.reload_browser(),
            BrowserAction::Back => {
                self.screen = Screen::Launcher;
                self.scan_devices();
            }
        }
        Ok(())
    }

    fn scan_devices(&mut self) {
        self.launcher_error = None;
        self.launcher_message = None;
        match self.provider.discover_devices() {
            Ok(devices) if !devices.is_empty() => {
                self.devices = devices
                    .into_iter()
                    .filter(|device| device.paired || !device.reachable_addrs.is_empty())
                    .collect();
                self.launcher_index = self.launcher_index.min(self.devices.len().saturating_sub(1));
                self.usb_candidate = None;
                self.usb_attached = false;
                self.usb_hint = None;
            }
            Ok(_) | Err(_) => {
                self.devices.clear();
                if let Ok(status) = self.provider.usb_status() {
                    self.usb_attached = !matches!(status.kind, UsbStatusKind::NoUsbHardware);
                    self.usb_candidate = status.endpoint_addr.clone();
                    self.usb_hint = Some(match status.kind {
                        UsbStatusKind::DptEndpointReachable => format!(
                            "{} Press 'u' to recover/recheck and start pairing.",
                            status.message
                        ),
                        UsbStatusKind::UsbSerialOnly => format!(
                            "{} Press 'u' to switch USB mode to network and retry.",
                            status.message
                        ),
                        UsbStatusKind::UsbNetworkVisible => format!(
                            "{} Press 'u' to retry endpoint recovery.",
                            status.message
                        ),
                        UsbStatusKind::NoUsbHardware => status.message,
                    });
                }
            }
        }
    }

    fn recover_usb_network(&mut self) {
        match self.provider.usb_recover() {
            Ok(status) => {
                self.usb_candidate = status.endpoint_addr.clone();
                self.launcher_message = Some(status.message.clone());
                if let Some(addr) = status.endpoint_addr {
                    self.add_ip = addr;
                    self.screen = Screen::AddDevice;
                    self.begin_pair_for_current_ip();
                } else {
                    self.scan_devices();
                }
            }
            Err(err) => {
                self.launcher_error = Some(format!("USB recover failed: {err}"));
            }
        }
    }

    fn begin_pair_for_current_ip(&mut self) {
        let ip = self.add_ip.trim().to_string();
        if ip.is_empty() {
            self.add_message = Some("Device IP is required.".into());
            return;
        }
        match self.provider.begin_pair_for_addr(ip.clone()) {
            Ok(_) => {
                self.add_message = Some(format!(
                    "The DPT should now show a PIN for {ip}. Enter it, then press 'p'."
                ));
            }
            Err(err) => {
                self.add_message = Some(format!("Could not start pairing: {err}"));
            }
        }
    }

    fn connect_current_ip(&mut self) {
        let ip = self.add_ip.trim().to_string();
        if ip.is_empty() {
            self.add_message = Some("Device IP is required.".into());
            return;
        }
        match self.provider.connect(Some(ip), None, None) {
            Ok(device) => {
                self.add_connected = Some(device.clone());
                self.add_message = Some(format!("Connected to {}", device.name));
            }
            Err(err) => {
                self.add_connected = None;
                self.add_message = Some(format!("Connect failed: {err}"));
            }
        }
    }

    fn pair_current_pin(&mut self) {
        let pin = self.add_pin.trim().to_string();
        if pin.is_empty() {
            self.add_message = Some("PIN is required.".into());
            return;
        }
        match self.provider.pair(pin) {
            Ok(device) => {
                self.add_connected = Some(device.clone());
                self.add_message = Some(format!("Paired with {}", device.name));
                self.scan_devices();
            }
            Err(err) => self.add_message = Some(format!("Pair failed: {err}")),
        }
    }

    fn open_browser(&mut self, device: DeviceSummary) {
        self.provider
            .connect(device.reachable_addrs.first().cloned(), None, None)
            .ok();
        let mut browser = BrowserState::new(device);
        self.browser = Some(browser.clone());
        self.screen = Screen::Browser;
        self.reload_browser();
        browser = self.browser.clone().unwrap_or(browser);
        let _ = browser;
    }

    fn reload_browser(&mut self) {
        if let Some(browser) = self.browser.as_mut() {
            browser.children_cache.clear();
            browser.status = self.provider.device_info().ok();
            browser.search_results = None;
            browser.search_progress = None;
            let root = browser.root_path.clone();
            match self.provider.list_entries(root.clone()) {
                Ok(entries) => {
                    browser.children_cache.insert(root, entries);
                    browser.message = None;
                }
                Err(err) => {
                    browser.message = Some(format!("Failed to load browser: {err}"));
                }
            }
        }
        self.trigger_search_if_needed();
        let rows = self.visible_nodes();
        if let Some(browser) = self.browser.as_mut() {
            if let Some((entry, _)) =
                rows.get(browser.selected_row.min(rows.len().saturating_sub(1)))
            {
                browser.selected_row = browser.selected_row.min(rows.len().saturating_sub(1));
                browser.selected_path = Some(entry.path.clone());
            }
        }
    }

    fn trigger_search_if_needed(&mut self) {
        let Some(browser) = self.browser.as_mut() else {
            return;
        };
        let query = browser.search_query.trim().to_string();
        if query.is_empty() {
            browser.search_results = None;
            browser.search_progress = None;
            self.search_rx = None;
            return;
        }

        browser.search_results = Some(Vec::new());
        browser.search_progress = Some(format!("Searching '{}'...", query));
        let root = browser.root_path.clone();
        let provider = self.provider.clone();
        self.search_rx = Some(spawn_search_job(provider, root, query));
    }

    fn load_children(&mut self, path: &str) -> Vec<RemoteEntry> {
        let Some(browser) = self.browser.as_mut() else {
            return Vec::new();
        };
        if let Some(entries) = browser.children_cache.get(path) {
            return entries.clone();
        }
        match self.provider.list_entries(path.to_string()) {
            Ok(entries) => {
                browser.children_cache.insert(path.to_string(), entries.clone());
                entries
            }
            Err(err) => {
                browser.message = Some(format!("Failed to load {path}: {err}"));
                Vec::new()
            }
        }
    }

    fn visible_nodes(&mut self) -> Vec<(RemoteEntry, usize)> {
        fn walk_tree(app: &mut TuiApp, path: &str, depth: usize, out: &mut Vec<(RemoteEntry, usize)>) {
            let entries = app.load_children(path);
            let expanded = app
                .browser
                .as_ref()
                .map(|browser| browser.expanded_folders.clone())
                .unwrap_or_default();
            for entry in entries {
                out.push((entry.clone(), depth));
                if entry.entry_type == RemoteEntryType::Folder && expanded.contains(&entry.path) {
                    walk_tree(app, &entry.path, depth + 1, out);
                }
            }
        }

        let Some(browser) = self.browser.as_ref() else {
            return Vec::new();
        };
        if !browser.search_query.trim().is_empty() {
            return browser.search_results.clone().unwrap_or_default();
        }
        let root = browser.root_path.clone();
        let mut out = Vec::new();
        walk_tree(self, &root, 0, &mut out);
        out
    }

    fn selected_browser_entry(&mut self) -> Option<RemoteEntry> {
        let rows = self.visible_nodes();
        let browser = self.browser.as_mut()?;
        let idx = browser.selected_row.min(rows.len().saturating_sub(1));
        rows.get(idx).map(|(entry, _)| entry.clone())
    }

    fn target_folder(&mut self) -> String {
        if let Some(entry) = self.selected_browser_entry() {
            if entry.entry_type == RemoteEntryType::Folder {
                return entry.path;
            }
            return parent_path(&entry.path).to_string();
        }
        self.browser
            .as_ref()
            .map(|browser| browser.root_path.clone())
            .unwrap_or_else(|| "Document".into())
    }

    fn open_entry(&mut self, entry: RemoteEntry) {
        if entry.entry_type == RemoteEntryType::Folder {
            self.browser_message("Open works for files only.");
            return;
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let temp_dir = std::env::temp_dir().join("dpt-manager-open");
        if let Err(err) = fs::create_dir_all(&temp_dir) {
            self.browser_message(format!("Open failed: {err}"));
            return;
        }
        let local_path = temp_dir.join(format!("{ts}-{}", entry.name));
        match self
            .provider
            .download(entry.path, local_path.to_string_lossy().to_string())
        {
            Ok(_) => match Command::new("open").arg(&local_path).status() {
                Ok(status) if status.success() => self.browser_message("Opened file locally."),
                Ok(status) => self.browser_message(format!(
                    "open exited with status {}",
                    status.code().unwrap_or(-1)
                )),
                Err(err) => self.browser_message(format!("open failed: {err}")),
            },
            Err(err) => self.browser_message(format!("Download failed: {err}")),
        }
    }

    fn reveal_local_copy(&mut self, entry: RemoteEntry) {
        if entry.entry_type == RemoteEntryType::Folder {
            self.browser_message("Reveal local copy works for files only.");
            return;
        }
        let temp_dir = std::env::temp_dir().join("dpt-manager-export");
        if let Err(err) = fs::create_dir_all(&temp_dir) {
            self.browser_message(format!("Reveal failed: {err}"));
            return;
        }
        let local_path = temp_dir.join(&entry.name);
        match self
            .provider
            .download(entry.path, local_path.to_string_lossy().to_string())
        {
            Ok(_) => match Command::new("open").args(["-R"]).arg(&local_path).status() {
                Ok(status) if status.success() => self.browser_message(
                    "Downloaded and revealed a local copy in Finder. Drag from Finder if needed.",
                ),
                Ok(status) => self.browser_message(format!(
                    "Reveal exited with status {}",
                    status.code().unwrap_or(-1)
                )),
                Err(err) => self.browser_message(format!("Reveal failed: {err}")),
            },
            Err(err) => self.browser_message(format!("Download failed: {err}")),
        }
    }

    fn browser_message(&mut self, message: impl Into<String>) {
        if let Some(browser) = self.browser.as_mut() {
            browser.message = Some(message.into());
        }
    }

    fn sync_browser_selection(&mut self) {
        let rows = self.visible_nodes();
        if let Some(browser) = self.browser.as_mut() {
            if rows.is_empty() {
                browser.selected_row = 0;
                browser.selected_path = None;
            } else {
                browser.selected_row = browser.selected_row.min(rows.len().saturating_sub(1));
                browser.selected_path = Some(rows[browser.selected_row].0.path.clone());
            }
        }
    }

    fn draw_launcher(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Digital Paper TUI",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from("Terminal-first Digital Paper manager with launcher, pairing, and library browsing."),
            ])
            .block(Block::default().borders(Borders::ALL).title("Launcher"))
            .wrap(Wrap { trim: true }),
            chunks[0],
        );

        if self.devices.is_empty() {
            let text = if let Some(addr) = &self.usb_candidate {
                format!("USB endpoint detected at {addr}. Press 'u' to recover/recheck before pairing.")
            } else if let Some(hint) = &self.usb_hint {
                hint.clone()
            } else {
                "Looking for your Digital Paper device. Press 'r' to rescan or 'a' for manual setup.".into()
            };
            frame.render_widget(
                Paragraph::new(text)
                    .block(Block::default().borders(Borders::ALL).title("Discovery"))
                    .alignment(Alignment::Left)
                    .wrap(Wrap { trim: true }),
                chunks[1],
            );
        } else {
            let items: Vec<ListItem> = self
                .devices
                .iter()
                .map(|device| {
                    let addr = device
                        .reachable_addrs
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "unknown".into());
                    let badge = if device.paired { "paired" } else { "detected" };
                    ListItem::new(vec![Line::from(vec![
                        Span::styled(
                            &device.name,
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                        Span::styled(format!("[{badge}]"), Style::default().fg(Color::Yellow)),
                        Span::raw("  "),
                        Span::styled(addr, Style::default().fg(Color::Gray)),
                    ])])
                })
                .collect();
            let mut state = ListState::default();
            state.select(Some(self.launcher_index.min(self.devices.len().saturating_sub(1))));
            frame.render_stateful_widget(
                List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("Devices"))
                    .highlight_style(Style::default().bg(Color::Blue).fg(Color::Black))
                    .highlight_symbol("› "),
                chunks[1],
                &mut state,
            );
        }

        frame.render_widget(
            Paragraph::new(
                self.launcher_message
                    .clone()
                    .or_else(|| self.launcher_error.clone())
                    .unwrap_or_else(|| "Keys: ↑↓ select  Enter open  a add device  u usb recover  r refresh  q quit".into()),
            )
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true }),
            chunks[2],
        );

        frame.render_widget(
            Paragraph::new(if self.usb_attached {
                "USB hardware detected"
            } else {
                "Wi-Fi path available"
            })
            .block(Block::default().borders(Borders::ALL).title("Transport"))
            .alignment(Alignment::Center),
            chunks[3],
        );
    }

    fn draw_add_device(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(8),
                Constraint::Length(4),
                Constraint::Min(4),
                Constraint::Length(3),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Add Device",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from("Wi-Fi is the most reliable path today. Connect first, then pair with the device PIN."),
            ])
            .block(Block::default().borders(Borders::ALL).title("Setup")),
            chunks[0],
        );

        let ip_style = if matches!(self.add_field, AddField::Ip) {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default()
        };
        let pin_style = if matches!(self.add_field, AddField::Pin) {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else {
            Style::default()
        };
        let form = vec![
            Line::from(vec![Span::styled("Device IP: ", Style::default().add_modifier(Modifier::BOLD)), Span::styled(self.add_ip.clone(), ip_style)]),
            Line::from(""),
            Line::from(vec![Span::styled("Pairing PIN: ", Style::default().add_modifier(Modifier::BOLD)), Span::styled(self.add_pin.clone(), pin_style)]),
            Line::from(""),
            Line::from("Keys: Tab switch field  c connect  p pair  o open library  Esc back"),
        ];
        frame.render_widget(
            Paragraph::new(form)
                .block(Block::default().borders(Borders::ALL).title("Fields"))
                .wrap(Wrap { trim: true }),
            chunks[1],
        );

        let connected = self
            .add_connected
            .as_ref()
            .map(|device| {
                format!(
                    "{} ({})",
                    device.name,
                    device.reachable_addrs.first().cloned().unwrap_or_default()
                )
            })
            .unwrap_or_else(|| "Not connected yet".into());
        frame.render_widget(
            Paragraph::new(connected)
                .block(Block::default().borders(Borders::ALL).title("Connected Device"))
                .wrap(Wrap { trim: true }),
            chunks[2],
        );

        frame.render_widget(
            Paragraph::new(
                self.add_message
                    .clone()
                    .unwrap_or_else(|| "Enter the device IP and press 'c' to connect.".into()),
            )
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true }),
            chunks[3],
        );

        frame.render_widget(
            Paragraph::new("GUI parity: connect, pair, open library")
                .block(Block::default().borders(Borders::ALL).title("Mode"))
                .alignment(Alignment::Center),
            chunks[4],
        );
    }

    fn draw_browser(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(5),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(area);

        let rows = self.visible_nodes();
        let buttons = self.browser_action_buttons();
        let browser = match self.browser.as_ref().cloned() {
            Some(browser) => browser,
            None => return,
        };

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    browser.device.name.clone(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{}  |  {} items  |  search {}{}",
                    browser.root_path,
                    rows.len(),
                    if browser.search_query.is_empty() {
                        "off".to_string()
                    } else {
                        browser.search_query.clone()
                    },
                    if browser.search_mode { " _" } else { "" }
                )),
                Line::from(
                    browser
                        .search_progress
                        .clone()
                        .unwrap_or_else(|| "Press '/' for recursive search.".into()),
                ),
            ])
            .block(Block::default().borders(Borders::ALL).title("Library")),
            vertical[0],
        );

        let items: Vec<ListItem> = if rows.is_empty() {
            vec![ListItem::new(Line::from(if browser.search_query.is_empty() {
                "This folder is empty. Press 'i' to import a file."
            } else {
                "No recursive matches. Press '/' to edit search or backspace inside search mode."
            }))]
        } else {
            rows.iter()
                .map(|(entry, depth)| {
                    let icon = match entry.entry_type {
                        RemoteEntryType::Folder => "▸",
                        RemoteEntryType::Document => "•",
                        RemoteEntryType::Unknown => "?",
                    };
                    let indent = "  ".repeat(*depth);
                    let meta = match (&entry.entry_type, entry.size) {
                        (RemoteEntryType::Folder, _) => "folder".into(),
                        (_, Some(bytes)) => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
                        _ => "file".into(),
                    };
                    ListItem::new(Line::from(vec![
                        Span::raw(indent),
                        Span::styled(icon, Style::default().fg(Color::Yellow)),
                        Span::raw(" "),
                        Span::raw(entry.name.clone()),
                        Span::raw("    "),
                        Span::styled(meta, Style::default().fg(Color::DarkGray)),
                    ]))
                })
                .collect()
        };
        let mut state = ListState::default();
        state.select(Some(browser.selected_row.min(items.len().saturating_sub(1))));
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title("Files"))
                .highlight_style(Style::default().bg(Color::Blue).fg(Color::Black))
                .highlight_symbol("› "),
            vertical[1],
            &mut state,
        );

        let status = browser.status.clone().unwrap_or(DeviceStatus {
            battery: digital_paper_domain::BatteryStatus {
                level_percent: None,
                charging: false,
            },
            storage: digital_paper_domain::StorageStatus {
                total_bytes: None,
                free_bytes: None,
            },
            firmware_version: None,
            owner: None,
            wifi_enabled: false,
        });
        let status_rows = vec![
            Row::new(vec![String::from("Battery"), status
                .battery
                .level_percent
                .map(|v| format!("{v}%"))
                .unwrap_or_else(|| "unknown".into())]),
            Row::new(vec![
                String::from("Wi-Fi"),
                if status.wifi_enabled {
                    String::from("on")
                } else {
                    String::from("off")
                },
            ]),
            Row::new(vec![
                String::from("Firmware"),
                status.firmware_version.unwrap_or_else(|| "unknown".into()),
            ]),
            Row::new(vec![
                String::from("Owner"),
                status.owner.unwrap_or_else(|| "unknown".into()),
            ]),
        ];
        frame.render_widget(
            Table::new(status_rows, [Constraint::Length(12), Constraint::Min(10)])
                .block(Block::default().borders(Borders::ALL).title("Device Footer")),
            vertical[2],
        );

        let action_line = render_action_buttons(&buttons);
        frame.render_widget(
            Paragraph::new(action_line)
                .block(Block::default().borders(Borders::ALL).title("Actions"))
                .wrap(Wrap { trim: false }),
            vertical[3],
        );

        frame.render_widget(
            Paragraph::new(
                browser
                    .message
                    .clone()
                    .unwrap_or_else(|| {
                        "Keys: ↑↓ move  Enter open/toggle  / search  right click menu  mouse click/select  wheel scroll  drag/paste path into prompts  q quit".into()
                    }),
            )
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true }),
            vertical[4],
        );
    }
}

fn default_device_addr() -> String {
    std::env::var("DPT_DEFAULT_ADDR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_DEVICE_HOST.to_string())
}

fn draw_prompt(frame: &mut Frame, prompt: &PromptState) {
    let area = centered_rect(70, 30, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                prompt.title.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(prompt.detail.clone()),
            Line::from(""),
            Line::from(Span::styled(
                prompt.input.clone(),
                Style::default().bg(Color::Yellow).fg(Color::Black),
            )),
            Line::from(""),
            Line::from("Enter confirms, Esc cancels, paste/drop path is supported"),
        ])
        .block(Block::default().borders(Borders::ALL).title("Prompt"))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_context_menu(frame: &mut Frame, menu: &ContextMenuState) {
    let area = context_menu_rect(menu);
    let items: Vec<ListItem> = menu
        .buttons
        .iter()
        .map(|button| ListItem::new(Line::from(button.label.clone())))
        .collect();
    let mut state = ListState::default();
    state.select(Some(menu.selected.min(menu.buttons.len().saturating_sub(1))));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Menu"))
            .highlight_style(Style::default().bg(Color::Blue).fg(Color::Black))
            .highlight_symbol("› "),
        area,
        &mut state,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn context_menu_rect(menu: &ContextMenuState) -> Rect {
    let width = menu
        .buttons
        .iter()
        .map(|button| button.label.len() as u16 + 4)
        .max()
        .unwrap_or(16)
        .max(16);
    let height = menu.buttons.len() as u16 + 2;
    Rect {
        x: menu.x.saturating_sub(1),
        y: menu.y.saturating_sub(1),
        width,
        height,
    }
}

fn render_action_buttons(buttons: &[ActionButton]) -> Line<'static> {
    let mut spans = Vec::new();
    for button in buttons {
        spans.push(Span::styled(
            format!("[{}]", button.label),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

fn normalize_paste_text(text: &str) -> String {
    text.trim().trim_matches('"').trim_matches('\'').to_string()
}

fn import_path(provider: &ProviderRef, local_path: &Path, remote_folder: &str) -> Result<usize> {
    if local_path.is_file() {
        let remote = format!(
            "{}/{}",
            remote_folder.trim_end_matches('/'),
            local_path.file_name().and_then(|v| v.to_str()).unwrap_or("upload.pdf")
        );
        provider.upload(local_path.to_string_lossy().to_string(), remote)?;
        return Ok(1);
    }
    if !local_path.is_dir() {
        anyhow::bail!("local path does not exist");
    }

    let mut count = 0usize;
    import_dir_recursive(provider, local_path, local_path, remote_folder, &mut count)?;
    Ok(count)
}

fn import_dir_recursive(
    provider: &ProviderRef,
    root: &Path,
    current: &Path,
    remote_folder: &str,
    count: &mut usize,
) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            import_dir_recursive(provider, root, &path, remote_folder, count)?;
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let remote = format!("{}/{}", remote_folder.trim_end_matches('/'), rel_str);
        provider.upload(path.to_string_lossy().to_string(), remote)?;
        *count += 1;
    }
    Ok(())
}

fn spawn_search_job(provider: ProviderRef, root: String, query: String) -> Receiver<SearchUpdate> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut visited_folders = 0usize;
        let mut results = Vec::new();
        let needle = query.to_ascii_lowercase();
        let result = search_walk(
            &provider,
            &root,
            0,
            &needle,
            &query,
            &tx,
            &mut visited_folders,
            &mut results,
        );
        match result {
            Ok(_) => {
                let _ = tx.send(SearchUpdate::Finished {
                    query,
                    results,
                    visited_folders,
                });
            }
            Err(err) => {
                let _ = tx.send(SearchUpdate::Failed {
                    query,
                    error: err.to_string(),
                });
            }
        }
    });
    rx
}

fn search_walk(
    provider: &ProviderRef,
    path: &str,
    depth: usize,
    needle: &str,
    query: &str,
    tx: &Sender<SearchUpdate>,
    visited_folders: &mut usize,
    out: &mut Vec<(RemoteEntry, usize)>,
) -> Result<bool> {
    *visited_folders += 1;
    let _ = tx.send(SearchUpdate::Progress {
        query: query.to_string(),
        current_path: path.to_string(),
        visited_folders: *visited_folders,
    });
    let entries = provider
        .list_entries(path.to_string())
        .with_context(|| format!("failed to list {path}"))?;
    let mut matched = false;

    for entry in entries {
        let entry_match = entry.name.to_ascii_lowercase().contains(needle);
        if entry.entry_type == RemoteEntryType::Folder {
            let mut child_matches = Vec::new();
            let descendant_match = search_walk(
                provider,
                &entry.path,
                depth + 1,
                needle,
                query,
                tx,
                visited_folders,
                &mut child_matches,
            )?;
            if entry_match || descendant_match {
                out.push((entry.clone(), depth));
                out.extend(child_matches);
                matched = true;
            }
        } else if entry_match {
            out.push((entry.clone(), depth));
            matched = true;
        }
    }

    Ok(matched)
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("")
}

fn launcher_hit_index(row: u16, item_count: usize) -> Option<usize> {
    list_hit_index(3, row, item_count)
}

fn browser_hit_index(row: u16, item_count: usize) -> Option<usize> {
    list_hit_index(3, row, item_count)
}

fn list_hit_index(rect_y: u16, row: u16, item_count: usize) -> Option<usize> {
    if item_count == 0 {
        return None;
    }
    let inner_top = rect_y.saturating_add(1);
    if row < inner_top {
        return None;
    }
    let index = usize::from(row - inner_top);
    (index < item_count).then_some(index)
}

fn browser_action_hit(column: u16, row: u16, buttons: &[ActionButton]) -> Option<BrowserAction> {
    let Ok((width, height)) = size() else {
        return None;
    };
    let outer = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(outer);
    let area = vertical[3];
    if row < area.y + 1 || row >= area.y + area.height.saturating_sub(1) {
        return None;
    }
    if column < area.x + 1 || column >= area.x + area.width.saturating_sub(1) {
        return None;
    }
    let mut start = area.x + 1;
    for button in buttons {
        let end = start + button.label.len() as u16 + 1;
        if column >= start && column <= end {
            return Some(button.action.clone());
        }
        start = end + 2;
    }
    None
}

fn menu_hit_index(area: Rect, column: u16, row: u16, item_count: usize) -> Option<usize> {
    if column < area.x || column >= area.x + area.width {
        return None;
    }
    list_hit_index(area.y, row, item_count)
}
