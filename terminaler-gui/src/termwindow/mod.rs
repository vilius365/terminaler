#![allow(clippy::range_plus_one)]
use super::renderstate::*;
use super::utilsprites::RenderMetrics;
use crate::colorease::ColorEase;
use crate::frontend::{front_end, try_front_end};
use crate::inputmap::InputMap;
use crate::overlay::{
    confirm_close_pane, confirm_close_tab, confirm_close_window, confirm_quit_program, launcher,
    start_overlay, start_overlay_pane, CopyModeParams, CopyOverlay, LauncherArgs, LauncherFlags,
    QuickSelectOverlay,
};
use crate::resize_increment_calculator::ResizeIncrementCalculator;
use crate::scripting::guiwin::GuiWin;
use crate::scrollbar::*;
use crate::selection::Selection;
use crate::shapecache::*;
use crate::tabbar::{TabBarItem, TabBarState};
use crate::termwindow::background::{
    load_background_image, reload_background_image, LoadedBackgroundLayer,
};
use crate::termwindow::keyevent::{KeyTableArgs, KeyTableState};
use crate::termwindow::modal::Modal;
use crate::termwindow::render::paint::AllowImage;
use crate::termwindow::render::{
    CachedLineState, LineQuadCacheKey, LineQuadCacheValue, LineToEleShapeCacheKey,
    LineToElementShapeItem,
};
use crate::termwindow::webgpu::WebGpuState;
use ::terminaler_term::input::{ClickPosition, MouseButton as TMB};
use ::window::*;
use anyhow::{anyhow, ensure, Context};
use config::keyassignment::{
    Confirmation, KeyAssignment, LauncherActionArgs, PaneDirection, Pattern, PromptInputLine,
    QuickSelectArguments, RotationDirection, SpawnCommand, SplitSize,
};
use config::window::WindowLevel;
use config::{
    configuration, AudibleBell, ConfigHandle, Dimension, DimensionContext, FrontEndSelection,
    GeometryOrigin, GuiPosition, TermConfig, WindowCloseConfirmation,
};
use lfucache::*;
use mux::pane::{
    CachePolicy, CloseReason, Pane, PaneId, Pattern as MuxPattern, PerformAssignmentResult,
};
use mux::renderable::RenderableDimensions;
use mux::tab::{
    PositionedPane, PositionedSplit, SplitDirection, SplitRequest, SplitSize as MuxSplitSize, Tab,
    TabId,
};
use mux::window::WindowId as MuxWindowId;
use mux::{Mux, MuxNotification};
// STRIPPED: mux_lua removed; use local stub
use crate::scripting::guiwin::MuxPane;
#[cfg(windows)]
use chrono::Datelike as _;
use smol::channel::Sender;
use smol::Timer;
use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, HashSet, LinkedList};
use std::ops::Add;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::SequenceNo;
use terminaler_dynamic::Value;
use terminaler_font::FontConfiguration;
use terminaler_term::color::ColorPalette;
use terminaler_term::input::LastMouseClick;
use terminaler_term::{Alert, Progress, StableRowIndex, TerminalConfiguration, TerminalSize};

pub mod background;
pub mod box_model;
pub mod charselect;
pub mod clipboard;
pub mod keyevent;
pub mod modal;
mod mouseevent;
pub mod palette;
pub mod profile_dropdown;
pub mod paneselect;
mod prevcursor;
pub mod render;
pub mod resize;
mod selection;
pub mod spawn;
pub mod webgpu;
use crate::spawn::SpawnWhere;
use prevcursor::PrevCursorPos;

const ATLAS_SIZE: usize = 128;

lazy_static::lazy_static! {
    static ref WINDOW_CLASS: Mutex<String> = Mutex::new(terminaler_gui_subcommands::DEFAULT_WINDOW_CLASS.to_owned());
    static ref POSITION: Mutex<Option<GuiPosition>> = Mutex::new(None);
    /// Pane IDs with notifications muted — shared between TermWindow and frontend.
    /// TermWindow writes on toggle; frontend reads before firing toast notifications.
    pub static ref MUTED_PANES: Mutex<HashSet<PaneId>> = Mutex::new(HashSet::new());
}

pub const ICON_DATA: &'static [u8] = include_bytes!("../../../assets/icon/terminal.png");

pub fn set_window_position(pos: GuiPosition) {
    POSITION.lock().unwrap().replace(pos);
}

pub fn set_window_class(cls: &str) {
    *WINDOW_CLASS.lock().unwrap() = cls.to_owned();
}

pub fn get_window_class() -> String {
    WINDOW_CLASS.lock().unwrap().clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MouseCapture {
    UI,
    TerminalPane(PaneId),
}

/// Type used together with Window::notify to do something in the
/// context of the window-specific event loop
pub enum TermWindowNotif {
    InvalidateShapeCache,
    PerformAssignment {
        pane_id: PaneId,
        assignment: KeyAssignment,
        tx: Option<Sender<anyhow::Result<()>>>,
    },
    SetLeftStatus(String),
    SetRightStatus(String),
    GetDimensions(Sender<(Dimensions, WindowState)>),
    GetSelectionForPane {
        pane_id: PaneId,
        tx: Sender<String>,
    },
    GetEffectiveConfig(Sender<ConfigHandle>),
    FinishWindowEvent {
        name: String,
        again: bool,
    },
    GetConfigOverrides(Sender<terminaler_dynamic::Value>),
    SetConfigOverrides(terminaler_dynamic::Value),
    CancelOverlayForPane(PaneId),
    CancelOverlayForTab {
        tab_id: TabId,
        pane_id: Option<PaneId>,
    },
    MuxNotification(MuxNotification),
    EmitStatusUpdate,
    Apply(Box<dyn FnOnce(&mut TermWindow) + Send + Sync>),
    SwitchToMuxWindow(MuxWindowId),
    SetInnerSize {
        width: usize,
        height: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Debug)]
pub struct TabDragState {
    pub source_tab_idx: usize,
    /// The tab that was active before the drag started (drop target).
    pub dest_tab_id: TabId,
    pub target_pane: Option<PaneId>,
    pub target_zone: Option<DropZone>,
    pub start_coords: (isize, isize),
    pub threshold_exceeded: bool,
}

#[derive(Clone, Debug)]
pub struct PaneLongPress {
    pub pane_id: PaneId,
    pub revealed: bool,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum ClaudeStatus {
    Working,
    WaitingInput,
    Idle,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaudeSessionInfo {
    pub model: Option<String>,
    pub context_pct: Option<u8>,
    pub cost_usd: Option<f32>,
    pub duration_ms: Option<u64>,
    pub lines_added: Option<u32>,
    pub lines_removed: Option<u32>,
    pub worktree: Option<String>,
    pub status: Option<ClaudeStatus>,
}

/// Tracks cumulative Claude API usage costs across sessions.
/// Persisted to `claude-usage.json` in the config directory.
#[cfg(windows)]
pub struct ClaudeUsageTracker {
    daily_cost: f32,
    weekly_cost: f32,
    current_date: String,
    week_start: String,
    last_seen_costs: HashMap<mux::pane::PaneId, f32>,
    last_persist: Instant,
}

#[cfg(windows)]
impl ClaudeUsageTracker {
    fn new() -> Self {
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        let monday = now
            - chrono::Duration::days(now.weekday().num_days_from_monday() as i64);
        let week_start = monday.format("%Y-%m-%d").to_string();
        let mut tracker = Self {
            daily_cost: 0.0,
            weekly_cost: 0.0,
            current_date: today,
            week_start,
            last_seen_costs: HashMap::new(),
            last_persist: Instant::now(),
        };
        tracker.load();
        tracker
    }

    fn usage_file_path() -> std::path::PathBuf {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        std::path::PathBuf::from(appdata)
            .join("Terminaler")
            .join("claude-usage.json")
    }

    fn load(&mut self) {
        let path = Self::usage_file_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                let saved_date = json["date"].as_str().unwrap_or("");
                let saved_week = json["weekStart"].as_str().unwrap_or("");
                if saved_date == self.current_date {
                    self.daily_cost = json["dailyCost"].as_f64().unwrap_or(0.0) as f32;
                }
                if saved_week == self.week_start {
                    self.weekly_cost = json["weeklyCost"].as_f64().unwrap_or(0.0) as f32;
                }
            }
        }
    }

    fn persist(&mut self) {
        if self.last_persist.elapsed() < std::time::Duration::from_secs(30) {
            return;
        }
        self.last_persist = Instant::now();
        let json = serde_json::json!({
            "date": self.current_date,
            "weekStart": self.week_start,
            "dailyCost": self.daily_cost,
            "weeklyCost": self.weekly_cost,
        });
        let path = Self::usage_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap_or_default());
    }

    fn check_date_rollover(&mut self) {
        let now = chrono::Local::now();
        let today = now.format("%Y-%m-%d").to_string();
        if today != self.current_date {
            self.daily_cost = 0.0;
            self.current_date = today;
            let monday = now
                - chrono::Duration::days(now.weekday().num_days_from_monday() as i64);
            let new_week_start = monday.format("%Y-%m-%d").to_string();
            if new_week_start != self.week_start {
                self.weekly_cost = 0.0;
                self.week_start = new_week_start;
            }
            // Force persist on rollover
            self.last_persist = Instant::now() - std::time::Duration::from_secs(60);
        }
    }

    fn update_costs(&mut self, pane_claude_infos: &[(mux::pane::PaneId, f32)]) {
        self.check_date_rollover();
        for &(pane_id, cost) in pane_claude_infos {
            let prev = self.last_seen_costs.get(&pane_id).copied().unwrap_or(0.0);
            if cost > prev {
                let delta = cost - prev;
                self.daily_cost += delta;
                self.weekly_cost += delta;
            }
            self.last_seen_costs.insert(pane_id, cost);
        }
        self.persist();
    }
}

pub struct SidebarTabInfo {
    pub cwd_short: String,
    pub git_branch: Option<String>,
    pub pane_claude_info: HashMap<mux::pane::PaneId, ClaudeSessionInfo>,
}

/// Convert a ClaudeSessionInfo to a serde_json::Value for the WebView sidebar.
#[cfg(windows)]
fn claude_to_json(c: &ClaudeSessionInfo) -> serde_json::Value {
    serde_json::json!({
        "model": c.model,
        "contextPct": c.context_pct,
        "costUsd": c.cost_usd,
        "durationMs": c.duration_ms,
        "linesAdded": c.lines_added,
        "linesRemoved": c.lines_removed,
        "worktree": c.worktree,
        "status": c.status.map(|s| match s {
            ClaudeStatus::Working => "working",
            ClaudeStatus::WaitingInput => "waiting_input",
            ClaudeStatus::Idle => "idle",
            ClaudeStatus::Error => "error",
        }),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabSidebarItem {
    Tab { tab_idx: usize, active: bool },
    Pane { tab_idx: usize, pane_idx: usize },
    ClosePane { pane_id: usize },
    MuteNotifications { pane_id: usize },
    NewTabButton,
    ResizeHandle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UIItemType {
    TabBar(TabBarItem),
    CloseTab(usize),
    AboveScrollThumb,
    ScrollThumb,
    BelowScrollThumb,
    Split(PositionedSplit),
    ProfileDropdownItem(usize),
    TabSidebar(TabSidebarItem),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UIItem {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub item_type: UIItemType,
}

impl UIItem {
    pub fn hit_test(&self, x: isize, y: isize) -> bool {
        x >= self.x as isize
            && x <= (self.x + self.width) as isize
            && y >= self.y as isize
            && y <= (self.y + self.height) as isize
    }
}

#[derive(Clone, Default)]
pub struct SemanticZoneCache {
    seqno: SequenceNo,
    zones: Vec<StableRowIndex>,
}

pub struct OverlayState {
    pub pane: Arc<dyn Pane>,
    pub key_table_state: KeyTableState,
}

pub struct PaneState {
    /// If is_some(), the top row of the visible screen.
    /// Otherwise, the viewport is at the bottom of the
    /// scrollback.
    viewport: Option<StableRowIndex>,
    selection: Selection,
    /// If is_some(), rather than display the actual tab
    /// contents, we're overlaying a little internal application
    /// tab.  We'll also route input to it.
    pub overlay: Option<OverlayState>,

    bell_start: Option<Instant>,
    notification_start: Option<Instant>,
    pub notification_count: u32,
    /// When true, suppress Windows toast notifications and in-tab indicators for this pane
    pub notifications_muted: bool,
    pub mouse_terminal_coords: Option<(ClickPosition, StableRowIndex)>,
    /// Per-pane font scale factor (1.0 = default, >1.0 = larger, <1.0 = smaller)
    pub font_scale: f64,
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            viewport: None,
            selection: Selection::default(),
            overlay: None,
            bell_start: None,
            notification_start: None,
            notification_count: 0,
            notifications_muted: false,
            mouse_terminal_coords: None,
            font_scale: 1.0,
        }
    }
}

/// Data used when synchronously formatting pane and window titles
#[derive(Debug, Clone)]
pub struct TabInformation {
    pub tab_id: TabId,
    pub tab_index: usize,
    pub is_active: bool,
    pub is_last_active: bool,
    pub active_pane: Option<PaneInformation>,
    pub window_id: MuxWindowId,
    pub tab_title: String,
    pub has_notification: bool,
    /// True if any pane in this tab has notifications muted
    pub has_muted_pane: bool,
    /// Effective zoom percentage for the active pane (None when 100%)
    pub zoom_pct: Option<u16>,
}


/// Data used when synchronously formatting pane and window titles
#[derive(Debug, Clone)]
pub struct PaneInformation {
    pub pane_id: PaneId,
    pub pane_index: usize,
    pub is_active: bool,
    pub is_zoomed: bool,
    pub has_unseen_output: bool,
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    pub title: String,
    pub user_vars: HashMap<String, String>,
    pub progress: Progress,
}

// UserData impls for TabInformation and PaneInformation removed (Lua stripped).
// These structs are used as plain data carriers within the GUI code.

#[derive(Default)]
pub struct TabState {
    /// If is_some(), rather than display the actual tab
    /// contents, we're overlaying a little internal application
    /// tab.  We'll also route input to it.
    pub overlay: Option<OverlayState>,
}

/// Manages the state/queue of lua based event handlers.
/// We don't want to queue more than 1 event at a time,
/// so we use this enum to allow for at most 1 executing
/// and 1 pending event.
#[derive(Copy, Clone, Debug)]
enum EventState {
    /// The event is not running
    None,
    /// The event is running
    InProgress,
    /// The event is running, and we have another one ready to
    /// run once it completes
    InProgressWithQueued(Option<PaneId>),
}

pub struct TermWindow {
    pub window: Option<Window>,
    pub config: ConfigHandle,
    pub config_overrides: terminaler_dynamic::Value,
    os_parameters: Option<parameters::Parameters>,
    /// When we most recently received keyboard focus
    pub focused: Option<Instant>,
    fonts: Rc<FontConfiguration>,
    /// Window dimensions and dpi
    pub dimensions: Dimensions,
    pub window_state: WindowState,
    pub resizes_pending: usize,
    is_repaint_pending: bool,
    pending_scale_changes: LinkedList<resize::ScaleChange>,
    /// Terminal dimensions
    terminal_size: TerminalSize,
    pub mux_window_id: MuxWindowId,
    pub mux_window_id_for_subscriptions: Arc<Mutex<MuxWindowId>>,
    pub render_metrics: RenderMetrics,
    render_state: Option<RenderState>,
    input_map: InputMap,
    /// If is_some, the LEADER modifier is active until the specified instant.
    leader_is_down: Option<std::time::Instant>,
    dead_key_status: DeadKeyStatus,
    key_table_state: KeyTableState,
    show_tab_bar: bool,
    show_scroll_bar: bool,
    show_tab_sidebar: bool,
    tab_sidebar_width: u16,
    tab_bar: TabBarState,
    fancy_tab_bar: Option<box_model::ComputedElement>,
    tab_sidebar: Option<box_model::ComputedElement>,
    tab_sidebar_info: HashMap<TabId, SidebarTabInfo>,
    last_sidebar_info_poll: Instant,
    /// WebView2-based sidebar (Windows only). None = use GPU fallback.
    #[cfg(windows)]
    webview_sidebar: Option<crate::webview_sidebar::WebViewSidebar>,
    /// Tracks the last time each pane produced output (for idle detection).
    pane_last_output: HashMap<PaneId, Instant>,
    /// Panes for which we already fired a "Claude idle" notification.
    /// Cleared when the pane produces new output.
    claude_idle_notified: std::collections::HashSet<PaneId>,
    /// Previous claude_status per pane, for detecting transitions to WaitingInput.
    claude_prev_status: HashMap<PaneId, Option<ClaudeStatus>>,
    /// Cumulative Claude API usage tracker (persisted to disk).
    #[cfg(windows)]
    claude_usage_tracker: ClaudeUsageTracker,
    pub right_status: String,
    pub left_status: String,
    last_ui_item: Option<UIItem>,
    /// Tracks whether the current mouse-down event is part of click-focus.
    /// If so, we ignore mouse events until released
    is_click_to_focus_window: bool,
    last_mouse_coords: (usize, i64),
    window_drag_position: Option<MouseEvent>,
    current_mouse_event: Option<MouseEvent>,
    prev_cursor: PrevCursorPos,
    last_scroll_info: RenderableDimensions,

    tab_state: RefCell<HashMap<TabId, TabState>>,
    pane_state: RefCell<HashMap<PaneId, PaneState>>,
    semantic_zones: HashMap<PaneId, SemanticZoneCache>,
    /// Cached FontConfiguration + RenderMetrics for each unique per-pane font scale.
    /// Key = (font_scale * 100.0).round() as u32 to avoid float hashing.
    scaled_font_configs: HashMap<u32, (Rc<FontConfiguration>, RenderMetrics)>,

    window_background: Vec<LoadedBackgroundLayer>,

    current_modifier_and_leds: (Modifiers, KeyboardLedStatus),
    current_mouse_buttons: Vec<MousePress>,
    current_mouse_capture: Option<MouseCapture>,

    opengl_info: Option<String>,

    /// Keeps track of double and triple clicks
    last_mouse_click: Option<LastMouseClick>,

    /// The URL over which we are currently hovering
    current_highlight: Option<Arc<Hyperlink>>,

    quad_generation: usize,
    shape_generation: usize,
    shape_cache: RefCell<LfuCache<ShapeCacheKey, anyhow::Result<Rc<Vec<ShapedInfo>>>>>,
    line_to_ele_shape_cache: RefCell<LfuCache<LineToEleShapeCacheKey, LineToElementShapeItem>>,

    line_state_cache: RefCell<LfuCacheU64<Arc<CachedLineState>>>,
    next_line_state_id: u64,

    line_quad_cache: RefCell<LfuCache<LineQuadCacheKey, LineQuadCacheValue>>,

    last_status_call: Instant,
    cursor_blink_state: RefCell<ColorEase>,
    blink_state: RefCell<ColorEase>,
    rapid_blink_state: RefCell<ColorEase>,

    palette: Option<ColorPalette>,

    ui_items: Vec<UIItem>,
    dragging: Option<(UIItem, MouseEvent)>,
    pub tab_drag: Option<TabDragState>,
    pub pane_long_press: Option<PaneLongPress>,
    pub hovered_pane_id: Option<mux::pane::PaneId>,
    pub toast_expanded_for: Option<mux::pane::PaneId>,

    modal: RefCell<Option<Rc<dyn Modal>>>,

    /// Custom default profile action for the "+" button (set via dropdown).
    /// If None, falls back to SpawnTab(CurrentPaneDomain).
    pub default_profile_action: Option<KeyAssignment>,

    event_states: HashMap<String, EventState>,
    pub current_event: Option<Value>,
    has_animation: RefCell<Option<Instant>>,
    /// We use this to attempt to do something reasonable
    /// if we run out of texture space
    allow_images: AllowImage,
    scheduled_animation: RefCell<Option<Instant>>,

    created: Instant,

    pub last_frame_duration: Duration,
    last_fps_check_time: Instant,
    num_frames: usize,
    pub fps: f32,

    connection_name: String,

    web_server_handle: Option<terminaler_web::WebServerHandle>,

    gl: Option<Rc<glium::backend::Context>>,
    webgpu: Option<Rc<WebGpuState>>,
    config_subscription: Option<config::ConfigSubscription>,
}

impl TermWindow {
    fn load_os_parameters(&mut self) {
        if let Some(ref window) = self.window {
            self.os_parameters = match window.get_os_parameters(&self.config, self.window_state) {
                Ok(os_parameters) => os_parameters,
                Err(err) => {
                    log::warn!("Error while getting OS parameters: {:#}", err);
                    None
                }
            };
        }
    }

    fn close_requested(&mut self, window: &Window) {
        let mux = Mux::get();
        match self.config.window_close_confirmation {
            WindowCloseConfirmation::NeverPrompt => {
                // Save session state before closing
                crate::session::save_current_session();
                // Immediately kill the tabs and allow the window to close
                mux.kill_window(self.mux_window_id);
                window.close();
                front_end().forget_known_window(window);
            }
            WindowCloseConfirmation::AlwaysPrompt => {
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => {
                        mux.kill_window(self.mux_window_id);
                        window.close();
                        front_end().forget_known_window(window);
                        return;
                    }
                };

                let mux_window_id = self.mux_window_id;

                let can_close = mux
                    .get_window(mux_window_id)
                    .map_or(false, |w| w.can_close_without_prompting());
                if can_close {
                    mux.kill_window(self.mux_window_id);
                    window.close();
                    front_end().forget_known_window(window);
                    return;
                }
                let window = self.window.clone().unwrap();
                let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                    confirm_close_window(term, mux_window_id, window, tab_id)
                });
                self.assign_overlay(tab.tab_id(), overlay);
                promise::spawn::spawn(future).detach();

                // Don't close right now; let the close happen from
                // the confirmation overlay
            }
        }
    }

    fn focus_changed(&mut self, focused: bool, window: &Window) {
        log::trace!("Setting focus to {:?}", focused);
        self.focused = if focused { Some(Instant::now()) } else { None };
        self.quad_generation += 1;
        self.load_os_parameters();

        if self.focused.is_none() {
            self.last_mouse_click = None;
            self.current_mouse_buttons.clear();
            self.current_mouse_capture = None;
            self.is_click_to_focus_window = false;
            self.tab_drag = None;
            self.pane_long_press = None;

            for state in self.pane_state.borrow_mut().values_mut() {
                state.mouse_terminal_coords.take();
            }
        }

        // Reset the cursor blink phase
        self.prev_cursor.bump();

        // force cursor to be repainted
        window.invalidate();

        if let Some(pane) = self.get_active_pane_or_overlay() {
            pane.focus_changed(focused);
        }

        self.update_title();
        self.emit_window_event("window-focus-changed", None);
    }

    fn created(&mut self, ctx: RenderContext) -> anyhow::Result<()> {
        self.render_state = None;

        let render_info = ctx.renderer_info();
        self.opengl_info.replace(render_info.clone());

        match RenderState::new(ctx, &self.fonts, &self.render_metrics, ATLAS_SIZE) {
            Ok(render_state) => {
                log::debug!(
                    "OpenGL initialized! {} terminaler version: {}",
                    render_info,
                    config::terminaler_version(),
                );
                self.render_state.replace(render_state);
            }
            Err(err) => {
                log::error!("failed to create RenderState: {}", err);
            }
        }

        if self.render_state.is_none() {
            panic!("No OpenGL");
        }

        Ok(())
    }
}

impl TermWindow {
    pub async fn new_window(mux_window_id: MuxWindowId) -> anyhow::Result<()> {
        let config = configuration();
        let dpi = config.dpi.unwrap_or_else(|| ::window::default_dpi()) as usize;
        let fontconfig = Rc::new(FontConfiguration::new(Some(config.clone()), dpi)?);

        let mux = Mux::get();
        let size = match mux.get_active_tab_for_window(mux_window_id) {
            Some(tab) => tab.get_size(),
            None => {
                log::debug!("new_window has no tabs... yet?");
                Default::default()
            }
        };
        let physical_rows = size.rows as usize;
        let physical_cols = size.cols as usize;

        let render_metrics = RenderMetrics::new(&fontconfig)?;
        log::trace!("using render_metrics {:#?}", render_metrics);

        // Initially we have only a single tab, so take that into account
        // for the tab bar state.
        let show_tab_bar = config.enable_tab_bar && !config.hide_tab_bar_if_only_one_tab;
        let tab_bar_height = if show_tab_bar {
            Self::tab_bar_pixel_height_impl(&config, &fontconfig, &render_metrics)? as usize
        } else {
            0
        };

        let terminal_size = TerminalSize {
            rows: physical_rows,
            cols: physical_cols,
            pixel_width: (render_metrics.cell_size.width as usize * physical_cols),
            pixel_height: (render_metrics.cell_size.height as usize * physical_rows),
            dpi: dpi as u32,
        };

        if terminal_size != size {
            // DPI is different from the default assumed DPI when the mux
            // created the pty. We need to inform the kernel of the revised
            // pixel geometry now
            log::trace!(
                "Initial geometry was {:?} but dpi-adjusted geometry \
                        is {:?}; update the kernel pixel geometry for the ptys!",
                size,
                terminal_size,
            );
            if let Some(window) = mux.get_window(mux_window_id) {
                for tab in window.iter() {
                    tab.resize(terminal_size);
                }
            };
        }

        let h_context = DimensionContext {
            dpi: dpi as f32,
            pixel_max: terminal_size.pixel_width as f32,
            pixel_cell: render_metrics.cell_size.width as f32,
        };
        let padding_left = config.window_padding.left.evaluate_as_pixels(h_context) as usize;
        let padding_right = resize::effective_right_padding(&config, h_context) as usize;
        let v_context = DimensionContext {
            dpi: dpi as f32,
            pixel_max: terminal_size.pixel_height as f32,
            pixel_cell: render_metrics.cell_size.height as f32,
        };
        let padding_top = config.window_padding.top.evaluate_as_pixels(v_context) as usize;
        let padding_bottom = config.window_padding.bottom.evaluate_as_pixels(v_context) as usize;

        let mut dimensions = Dimensions {
            pixel_width: (terminal_size.pixel_width + padding_left + padding_right) as usize,
            pixel_height: ((terminal_size.rows * render_metrics.cell_size.height as usize)
                + padding_top
                + padding_bottom) as usize
                + tab_bar_height,
            dpi,
        };

        let border = Self::get_os_border_impl(&None, &config, &dimensions, &render_metrics);

        dimensions.pixel_height += (border.top + border.bottom).get() as usize;
        dimensions.pixel_width += (border.left + border.right).get() as usize;

        let window_background = load_background_image(&config, &dimensions, &render_metrics);

        log::trace!(
            "TermWindow::new_window called with mux_window_id {} {:?} {:?}",
            mux_window_id,
            terminal_size,
            dimensions
        );

        let render_state = None;

        let connection_name = Connection::get().unwrap().name();

        // Start web access server if enabled in config
        let web_server_handle = {
            let cfg = config::configuration();
            if let Some(ref web_config) = cfg.web_access {
                if web_config.enabled {
                    match terminaler_web::start_web_server(web_config.into()) {
                        Ok(handle) => {
                            log::info!("Web access server started from TermWindow");
                            Some(handle)
                        }
                        Err(e) => {
                            log::error!("Failed to start web access server: {:#}", e);
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        let myself = Self {
            created: Instant::now(),
            connection_name,
            web_server_handle,
            last_fps_check_time: Instant::now(),
            num_frames: 0,
            last_frame_duration: Duration::ZERO,
            fps: 0.,
            config_subscription: None,
            os_parameters: None,
            gl: None,
            webgpu: None,
            window: None,
            window_background,
            config: config.clone(),
            config_overrides: terminaler_dynamic::Value::default(),
            palette: None,
            focused: None,
            mux_window_id,
            mux_window_id_for_subscriptions: Arc::new(Mutex::new(mux_window_id)),
            fonts: Rc::clone(&fontconfig),
            render_metrics,
            dimensions,
            window_state: WindowState::default(),
            resizes_pending: 0,
            is_repaint_pending: false,
            pending_scale_changes: LinkedList::new(),
            terminal_size,
            render_state,
            input_map: InputMap::new(&config),
            leader_is_down: None,
            dead_key_status: DeadKeyStatus::None,
            show_tab_bar,
            show_scroll_bar: config.enable_scroll_bar,
            show_tab_sidebar: config.tab_sidebar_enabled,
            tab_sidebar_width: config.tab_sidebar_width,
            tab_bar: TabBarState::default(),
            fancy_tab_bar: None,
            tab_sidebar: None,
            tab_sidebar_info: HashMap::new(),
            last_sidebar_info_poll: Instant::now(),
            #[cfg(windows)]
            webview_sidebar: None,
            pane_last_output: HashMap::new(),
            claude_idle_notified: std::collections::HashSet::new(),
            claude_prev_status: HashMap::new(),
            #[cfg(windows)]
            claude_usage_tracker: ClaudeUsageTracker::new(),
            right_status: String::new(),
            left_status: String::new(),
            last_mouse_coords: (0, -1),
            window_drag_position: None,
            current_mouse_event: None,
            current_modifier_and_leds: Default::default(),
            prev_cursor: PrevCursorPos::new(),
            last_scroll_info: RenderableDimensions::default(),
            tab_state: RefCell::new(HashMap::new()),
            pane_state: RefCell::new(HashMap::new()),
            current_mouse_buttons: vec![],
            current_mouse_capture: None,
            last_mouse_click: None,
            current_highlight: None,
            quad_generation: 0,
            shape_generation: 0,
            shape_cache: RefCell::new(LfuCache::new(
                "shape_cache.hit.rate",
                "shape_cache.miss.rate",
                |config| config.shape_cache_size,
                &config,
            )),
            line_state_cache: RefCell::new(LfuCacheU64::new(
                "line_state_cache.hit.rate",
                "line_state_cache.miss.rate",
                |config| config.line_state_cache_size,
                &config,
            )),
            next_line_state_id: 0,
            line_quad_cache: RefCell::new(LfuCache::new(
                "line_quad_cache.hit.rate",
                "line_quad_cache.miss.rate",
                |config| config.line_quad_cache_size,
                &config,
            )),
            line_to_ele_shape_cache: RefCell::new(LfuCache::new(
                "line_to_ele_shape_cache.hit.rate",
                "line_to_ele_shape_cache.miss.rate",
                |config| config.line_to_ele_shape_cache_size,
                &config,
            )),
            last_status_call: Instant::now(),
            cursor_blink_state: RefCell::new(ColorEase::new(
                config.cursor_blink_rate,
                config.cursor_blink_ease_in,
                config.cursor_blink_rate,
                config.cursor_blink_ease_out,
                None,
            )),
            blink_state: RefCell::new(ColorEase::new(
                config.text_blink_rate,
                config.text_blink_ease_in,
                config.text_blink_rate,
                config.text_blink_ease_out,
                None,
            )),
            rapid_blink_state: RefCell::new(ColorEase::new(
                config.text_blink_rate_rapid,
                config.text_blink_rapid_ease_in,
                config.text_blink_rate_rapid,
                config.text_blink_rapid_ease_out,
                None,
            )),
            event_states: HashMap::new(),
            current_event: None,
            has_animation: RefCell::new(None),
            scheduled_animation: RefCell::new(None),
            allow_images: AllowImage::Yes,
            semantic_zones: HashMap::new(),
            scaled_font_configs: HashMap::new(),
            ui_items: vec![],
            dragging: None,
            tab_drag: None,
            pane_long_press: None,
            hovered_pane_id: None,
            toast_expanded_for: None,
            last_ui_item: None,
            is_click_to_focus_window: false,
            key_table_state: KeyTableState::default(),
            modal: RefCell::new(None),
            default_profile_action: None,
            opengl_info: None,
        };

        let tw = Rc::new(RefCell::new(myself));
        let tw_event = Rc::clone(&tw);

        let mut x = None;
        let mut y = None;
        let mut origin = GeometryOrigin::default();

        if let Some(position) = mux
            .get_window(mux_window_id)
            .and_then(|window| window.get_initial_position().clone())
            .or_else(|| POSITION.lock().unwrap().take())
        {
            x.replace(position.x);
            y.replace(position.y);
            origin = position.origin;
        }

        let geometry = RequestedWindowGeometry {
            width: Dimension::Pixels(dimensions.pixel_width as f32),
            height: Dimension::Pixels(dimensions.pixel_height as f32),
            x,
            y,
            origin,
        };
        log::trace!("{:?}", geometry);

        let window = Window::new_window(
            &get_window_class(),
            "terminaler",
            geometry,
            Some(&config),
            Rc::clone(&fontconfig),
            move |event, window| {
                let mut tw = tw_event.borrow_mut();
                if let Err(err) = tw.dispatch_window_event(event, window) {
                    log::error!("dispatch_window_event: {:#}", err);
                }
            },
        )
        .await?;
        tw.borrow_mut().window.replace(window.clone());

        Self::apply_icon(&window)?;

        let config_subscription = config::subscribe_to_config_reload({
            let window = window.clone();
            move || {
                window.notify(TermWindowNotif::Apply(Box::new(|tw| {
                    tw.config_was_reloaded()
                })));
                true
            }
        });

        let gl = match config.front_end {
            FrontEndSelection::WebGpu => None,
            _ => Some(window.enable_opengl().await?),
        };

        {
            let mut myself = tw.borrow_mut();
            let webgpu = match config.front_end {
                FrontEndSelection::WebGpu => Some(Rc::new(
                    WebGpuState::new(&window, dimensions, &config).await?,
                )),
                _ => None,
            };
            myself.config_subscription.replace(config_subscription);
            if config.use_resize_increments {
                window.set_resize_increments(
                    ResizeIncrementCalculator {
                        x: myself.render_metrics.cell_size.width as u16,
                        y: myself.render_metrics.cell_size.height as u16,
                        padding_left: padding_left,
                        padding_top: padding_top,
                        padding_right: padding_right,
                        padding_bottom: padding_bottom,
                        border: border,
                        tab_bar_height: tab_bar_height,
                    }
                    .into(),
                );
            }

            if let Some(gl) = gl {
                myself.gl.replace(Rc::clone(&gl));
                myself.created(RenderContext::Glium(Rc::clone(&gl)))?;
            }
            if let Some(webgpu) = webgpu {
                myself.webgpu.replace(Rc::clone(&webgpu));
                myself.created(RenderContext::WebGpu(Rc::clone(&webgpu)))?;
            }
            myself.load_os_parameters();
            window.show();
            #[cfg(windows)]
            myself.try_init_webview_sidebar(&window);
            myself.subscribe_to_pane_updates();
            myself.emit_window_event("window-config-reloaded", None);
            myself.emit_status_event();
        }

        crate::update::start_update_checker();
        front_end().record_known_window(window, mux_window_id);

        Ok(())
    }

    fn dispatch_window_event(
        &mut self,
        event: WindowEvent,
        window: &Window,
    ) -> anyhow::Result<bool> {
        log::debug!("{event:?}");
        match event {
            WindowEvent::Destroyed => {
                // Ensure that we cancel any overlays we had running, so
                // that the mux can empty out, otherwise the mux keeps
                // the TermWindow alive via the frontend even though
                // the window is gone and we'll linger forever.
                // <https://github.com/wez/wezterm/issues/3522>
                self.clear_all_overlays();
                Ok(false)
            }
            WindowEvent::CloseRequested => {
                self.close_requested(window);
                Ok(true)
            }
            WindowEvent::AppearanceChanged(appearance) => {
                log::debug!("Appearance is now {:?}", appearance);
                // This is a bit fugly; we get per-window notifications
                // for appearance changes which successfully updates the
                // per-window config, but we need to explicitly tell the
                // global config to reload, otherwise things that acces
                // the config via config::configuration() will see the
                // prior version of the config.
                // What's fugly about this is that we'll reload the
                // global config here once per window, which could
                // be nasty for folks with a lot of windows.
                // <https://github.com/wez/wezterm/issues/2295>
                config::reload();
                self.config_was_reloaded();
                Ok(true)
            }
            WindowEvent::PerformKeyAssignment(action) => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    self.perform_key_assignment(&pane, &action)?;
                    window.invalidate();
                }
                Ok(true)
            }
            WindowEvent::FocusChanged(focused) => {
                self.focus_changed(focused, window);
                Ok(true)
            }
            WindowEvent::MouseEvent(event) => {
                self.mouse_event_impl(event, window);
                Ok(true)
            }
            WindowEvent::MouseLeave => {
                self.mouse_leave_impl(window);
                Ok(true)
            }
            WindowEvent::Resized {
                dimensions,
                window_state,
                live_resizing,
            } => {
                self.resize(dimensions, window_state, window, live_resizing);
                Ok(true)
            }
            WindowEvent::SetInnerSizeCompleted => {
                self.resizes_pending -= 1;
                if self.is_repaint_pending {
                    self.is_repaint_pending = false;
                    if self.webgpu.is_some() {
                        self.do_paint_webgpu()?;
                    } else {
                        self.do_paint(window);
                    }
                }
                self.apply_pending_scale_changes();
                Ok(true)
            }
            WindowEvent::AdviseModifiersLedStatus(modifiers, leds) => {
                self.current_modifier_and_leds = (modifiers, leds);
                self.update_title();
                window.invalidate();
                Ok(true)
            }
            WindowEvent::RawKeyEvent(event) => {
                self.raw_key_event_impl(event, window);
                Ok(true)
            }
            WindowEvent::KeyEvent(event) => {
                self.key_event_impl(event, window);
                Ok(true)
            }
            WindowEvent::AdviseDeadKeyStatus(status) => {
                if self.config.debug_key_events {
                    log::info!("DeadKeyStatus now: {:?}", status);
                } else {
                    log::trace!("DeadKeyStatus now: {:?}", status);
                }
                self.dead_key_status = status;
                self.update_title();
                // Ensure that we repaint so that any composing
                // text is updated
                window.invalidate();
                Ok(true)
            }
            WindowEvent::NeedRepaint => {
                if self.resizes_pending > 0 {
                    self.is_repaint_pending = true;
                    Ok(true)
                } else if self.webgpu.is_some() {
                    self.do_paint_webgpu()
                } else {
                    Ok(self.do_paint(window))
                }
            }
            WindowEvent::Notification(item) => {
                if let Ok(notif) = item.downcast::<TermWindowNotif>() {
                    self.dispatch_notif(*notif, window)
                        .context("dispatch_notif")?;
                }
                Ok(true)
            }
            WindowEvent::DroppedString(text) => {
                let pane = match self.get_active_pane_or_overlay() {
                    Some(pane) => pane,
                    None => return Ok(true),
                };
                pane.send_paste(text.as_str())?;
                Ok(true)
            }
            WindowEvent::DroppedUrl(urls) => {
                let pane = match self.get_active_pane_or_overlay() {
                    Some(pane) => pane,
                    None => return Ok(true),
                };
                let urls = urls
                    .iter()
                    .map(|url| self.config.quote_dropped_files.escape(&url.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
                    + " ";
                pane.send_paste(urls.as_str())?;
                Ok(true)
            }
            WindowEvent::DroppedFile(paths) => {
                let pane = match self.get_active_pane_or_overlay() {
                    Some(pane) => pane,
                    None => return Ok(true),
                };
                let paths = paths
                    .iter()
                    .map(|path| {
                        self.config
                            .quote_dropped_files
                            .escape(&path.to_string_lossy())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
                    + " ";
                pane.send_paste(&paths)?;
                Ok(true)
            }
            WindowEvent::DraggedFile(_) => Ok(true),
        }
    }

    fn do_paint(&mut self, window: &Window) -> bool {
        let gl = match self.gl.as_ref() {
            Some(gl) => gl,
            None => return false,
        };

        if gl.is_context_lost() {
            log::error!("opengl context was lost; should reinit");
            window.close();
            front_end().forget_known_window(window);
            return false;
        }

        let mut frame = glium::Frame::new(
            Rc::clone(&gl),
            (
                self.dimensions.pixel_width as u32,
                self.dimensions.pixel_height as u32,
            ),
        );
        self.paint_impl(&mut RenderFrame::Glium(&mut frame));
        window.finish_frame(frame).is_ok()
    }

    fn do_paint_webgpu(&mut self) -> anyhow::Result<bool> {
        self.webgpu.as_mut().unwrap().resize(self.dimensions);
        match self.do_paint_webgpu_impl() {
            Ok(ok) => Ok(ok),
            Err(err) => {
                match err.downcast_ref::<wgpu::SurfaceError>() {
                    Some(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        self.webgpu.as_mut().unwrap().resize(self.dimensions);
                        return self.do_paint_webgpu_impl();
                    }
                    _ => {}
                }
                Err(err)
            }
        }
    }

    fn do_paint_webgpu_impl(&mut self) -> anyhow::Result<bool> {
        self.paint_impl(&mut RenderFrame::WebGpu);
        Ok(true)
    }

    fn dispatch_notif(&mut self, notif: TermWindowNotif, window: &Window) -> anyhow::Result<()> {
        fn chan_err<T>(e: smol::channel::TrySendError<T>) -> anyhow::Error {
            anyhow::anyhow!("{}", e)
        }

        match notif {
            TermWindowNotif::InvalidateShapeCache => {
                self.shape_generation += 1;
                self.shape_cache.borrow_mut().clear();
                self.invalidate_modal();
                window.invalidate();
            }
            TermWindowNotif::PerformAssignment {
                pane_id,
                assignment,
                tx,
            } => {
                let mux = Mux::get();
                let result = || -> anyhow::Result<()> {
                    // The CopyMode overlay doesn't exist in the mux, but aliases
                    // itself with the overlaid pane's pane_id.
                    // So we do a bit of fancy footwork here to resolve the overlay
                    // and use that if it has the same pane_id, but otherwise fall
                    // back to what we get from the mux.
                    // <https://github.com/wez/wezterm/issues/3209>
                    let active_pane = self
                        .get_active_pane_or_overlay()
                        .ok_or_else(|| anyhow!("there is no active pane!?"))?;
                    let pane = if active_pane.pane_id() == pane_id {
                        active_pane
                    } else {
                        mux.get_pane(pane_id)
                            .ok_or_else(|| anyhow!("pane id {} is not valid", pane_id))?
                    };
                    self.perform_key_assignment(&pane, &assignment)
                        .context("perform_key_assignment")?;
                    Ok(())
                }();
                window.invalidate();
                if let Some(tx) = tx {
                    tx.try_send(result).ok();
                }
            }
            TermWindowNotif::SetRightStatus(status) => {
                if status != self.right_status {
                    self.right_status = status;
                    self.update_title_post_status();
                } else {
                    self.schedule_next_status_update();
                }
            }
            TermWindowNotif::SetLeftStatus(status) => {
                if status != self.left_status {
                    self.left_status = status;
                    self.update_title_post_status();
                } else {
                    self.schedule_next_status_update();
                }
            }
            TermWindowNotif::GetDimensions(tx) => {
                tx.try_send((self.dimensions, self.window_state))
                    .map_err(chan_err)
                    .context("send GetDimensions response")?;
            }
            TermWindowNotif::GetEffectiveConfig(tx) => {
                tx.try_send(self.config.clone())
                    .map_err(chan_err)
                    .context("send GetEffectiveConfig response")?;
            }
            TermWindowNotif::FinishWindowEvent { name, again } => {
                self.finish_window_event(&name, again);
            }
            TermWindowNotif::GetConfigOverrides(tx) => {
                tx.try_send(self.config_overrides.clone())
                    .map_err(chan_err)
                    .context("send GetConfigOverrides response")?;
            }
            TermWindowNotif::SetConfigOverrides(value) => {
                if value != self.config_overrides {
                    self.config_overrides = value;
                    self.config_was_reloaded();
                }
            }
            TermWindowNotif::CancelOverlayForPane(pane_id) => {
                self.cancel_overlay_for_pane(pane_id);
            }
            TermWindowNotif::CancelOverlayForTab { tab_id, pane_id } => {
                self.cancel_overlay_for_tab(tab_id, pane_id);
            }
            TermWindowNotif::MuxNotification(n) => match n {
                MuxNotification::Alert {
                    alert: Alert::SetUserVar { name, value },
                    pane_id,
                } => {
                    self.emit_user_var_event(pane_id, name, value);
                }
                MuxNotification::WindowTitleChanged { .. }
                | MuxNotification::Alert {
                    alert:
                        Alert::OutputSinceFocusLost
                        | Alert::CurrentWorkingDirectoryChanged
                        | Alert::WindowTitleChanged(_)
                        | Alert::TabTitleChanged(_)
                        | Alert::IconTitleChanged(_)
                        | Alert::Progress(_),
                    ..
                } => {
                    self.update_title();
                }
                MuxNotification::Alert {
                    alert: Alert::PaletteChanged,
                    pane_id,
                } => {
                    // Shape cache includes color information, so
                    // ensure that we invalidate that as part of
                    // this overall invalidation for the palette
                    self.dispatch_notif(TermWindowNotif::InvalidateShapeCache, window)?;
                    self.mux_pane_output_event(pane_id);
                }
                MuxNotification::Alert {
                    alert: Alert::Bell,
                    pane_id,
                } => {
                    if !self.window_contains_pane(pane_id) {
                        return Ok(());
                    }

                    match self.config.audible_bell {
                        AudibleBell::SystemBeep => {
                            Connection::get().expect("on main thread").beep();
                        }
                        AudibleBell::Disabled => {}
                    }

                    log::trace!("Ding! (this is the bell) in pane {}", pane_id);
                    self.emit_window_event("bell", Some(pane_id));

                    let mut per_pane = self.pane_state(pane_id);
                    per_pane.bell_start.replace(Instant::now());
                    window.invalidate();
                }
                MuxNotification::Alert {
                    alert: Alert::ToastNotification { .. },
                    pane_id,
                } => {
                    if self.window_contains_pane(pane_id) {
                        let muted = self.pane_state(pane_id).notifications_muted;
                        if !muted {
                            let mut per_pane = self.pane_state(pane_id);
                            per_pane.notification_start.replace(Instant::now());
                            per_pane.notification_count += 1;
                        }
                    }
                    window.invalidate();
                    self.update_title();
                }
                MuxNotification::TabAddedToWindow {
                    window_id: _,
                    tab_id,
                } => {
                    let mux = Mux::get();
                    let mut size = self.terminal_size;
                    if let Some(tab) = mux.get_tab(tab_id) {
                        // If we attached to a remote domain and loaded in
                        // a tab async, we need to fixup its size, either
                        // by resizing it or resizes ourselves.
                        // The strategy here is to adjust both by taking
                        // the maximal size in both horizontal and vertical
                        // dimensions and applying that. In practice that
                        // means that a new local client will resize larger
                        // to adjust to the size of an existing client.
                        let tab_size = tab.get_size();
                        size.rows = size.rows.max(tab_size.rows);
                        size.cols = size.cols.max(tab_size.cols);

                        if size.rows != self.terminal_size.rows
                            || size.cols != self.terminal_size.cols
                            || size.pixel_width != self.terminal_size.pixel_width
                            || size.pixel_height != self.terminal_size.pixel_height
                        {
                            self.set_window_size(size, window)?;
                        } else if tab_size.dpi == 0 {
                            log::debug!("fixup dpi in newly added tab");
                            tab.resize(self.terminal_size);
                        }
                    }
                }
                MuxNotification::PaneOutput(pane_id) => {
                    self.pane_last_output.insert(pane_id, Instant::now());
                    self.claude_idle_notified.remove(&pane_id);
                    self.mux_pane_output_event(pane_id);
                }
                MuxNotification::WindowInvalidated(_) => {
                    window.invalidate();
                    self.update_title_post_status();
                }
                MuxNotification::WindowRemoved(_window_id) => {
                    // Handled by frontend
                }
                MuxNotification::AssignClipboard { .. } => {
                    // Handled by frontend
                }
                MuxNotification::SaveToDownloads { .. } => {
                    // Handled by frontend
                }
                MuxNotification::PaneFocused(_) => {
                    // Also handled by clientpane
                    self.update_title_post_status();
                }
                MuxNotification::TabResized(_) => {
                    // Also handled by terminaler-client
                    self.update_title_post_status();
                }
                MuxNotification::TabTitleChanged { .. } => {
                    self.update_title_post_status();
                }
                MuxNotification::PaneAdded(_)
                | MuxNotification::WorkspaceRenamed { .. }
                | MuxNotification::PaneRemoved(_)
                | MuxNotification::WindowWorkspaceChanged(_)
                | MuxNotification::ActiveWorkspaceChanged(_)
                | MuxNotification::Empty
                | MuxNotification::WindowCreated(_) => {}
            },
            TermWindowNotif::EmitStatusUpdate => {
                self.emit_status_event();
            }
            TermWindowNotif::GetSelectionForPane { pane_id, tx } => {
                let mux = Mux::get();
                let pane = mux
                    .get_pane(pane_id)
                    .ok_or_else(|| anyhow!("pane id {} is not valid", pane_id))?;

                tx.try_send(self.selection_text(&pane))
                    .map_err(chan_err)
                    .context("send GetSelectionForPane response")?;
            }
            TermWindowNotif::Apply(func) => {
                func(self);
            }
            TermWindowNotif::SwitchToMuxWindow(mux_window_id) => {
                self.mux_window_id = mux_window_id;
                *self.mux_window_id_for_subscriptions.lock().unwrap() = mux_window_id;

                self.clear_all_overlays();
                self.current_highlight.take();
                self.invalidate_fancy_tab_bar();
                self.invalidate_tab_sidebar();
                self.invalidate_modal();

                let mux = Mux::get();
                if let Some(window) = mux.get_window(self.mux_window_id) {
                    for tab in window.iter() {
                        tab.resize(self.terminal_size);
                    }
                };
                self.update_title();
                window.invalidate();
            }
            TermWindowNotif::SetInnerSize { width, height } => {
                self.set_inner_size(window, width, height);
            }
        }

        Ok(())
    }

    fn set_inner_size(&mut self, window: &Window, width: usize, height: usize) {
        self.resizes_pending += 1;
        window.set_inner_size(width, height);
    }

    /// Take care to remove our panes from the mux, otherwise
    /// we can leave the mux with no windows but some panes
    /// and it won't believe that we are empty.
    fn clear_all_overlays(&mut self) {
        let overlay_panes_to_cancel = self
            .pane_state
            .borrow()
            .iter()
            .filter_map(|(_, state)| state.overlay.as_ref().map(|overlay| overlay.pane.pane_id()))
            .collect::<Vec<_>>();

        for pane_id in overlay_panes_to_cancel {
            self.cancel_overlay_for_pane(pane_id);
        }

        let tab_overlays_to_cancel = self
            .tab_state
            .borrow()
            .iter()
            .filter_map(|(tab_id, state)| state.overlay.as_ref().map(|_| *tab_id))
            .collect::<Vec<_>>();

        for tab_id in tab_overlays_to_cancel {
            self.cancel_overlay_for_tab(tab_id, None);
        }

        self.pane_state.borrow_mut().clear();
        self.tab_state.borrow_mut().clear();
    }

    fn apply_icon(window: &Window) -> anyhow::Result<()> {
        let image = image::load_from_memory(ICON_DATA)?.into_rgba8();
        let (width, height) = image.dimensions();
        window.set_icon(Image::with_rgba32(
            width as usize,
            height as usize,
            width as usize * 4,
            image.as_raw(),
        ));
        Ok(())
    }

    fn schedule_status_update(&self) {
        if let Some(window) = self.window.as_ref() {
            window.notify(TermWindowNotif::EmitStatusUpdate);
        }
    }

    fn is_pane_visible(&mut self, pane_id: PaneId) -> bool {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return false,
        };

        let tab_id = tab.tab_id();
        if let Some(tab_overlay) = self
            .tab_state(tab_id)
            .overlay
            .as_ref()
            .map(|overlay| overlay.pane.clone())
        {
            return tab_overlay.pane_id() == pane_id;
        }

        tab.contains_pane(pane_id)
    }

    fn mux_pane_output_event(&mut self, pane_id: PaneId) {
        metrics::histogram!("mux.pane_output_event.rate").record(1.);
        if self.is_pane_visible(pane_id) {
            if let Some(ref win) = self.window {
                win.invalidate();
            }
        }
    }

    fn mux_pane_output_event_callback(
        n: MuxNotification,
        window: &Window,
        mux_window_id: MuxWindowId,
        dead: &Arc<AtomicBool>,
    ) -> bool {
        if dead.load(Ordering::Relaxed) {
            // Subscription cancelled asynchronously
            return false;
        }

        match n {
            MuxNotification::Alert {
                pane_id,
                alert:
                    Alert::OutputSinceFocusLost
                    | Alert::CurrentWorkingDirectoryChanged
                    | Alert::WindowTitleChanged(_)
                    | Alert::TabTitleChanged(_)
                    | Alert::IconTitleChanged(_)
                    | Alert::Progress(_)
                    | Alert::SetUserVar { .. }
                    | Alert::Bell,
            }
            | MuxNotification::PaneFocused(pane_id)
            | MuxNotification::PaneRemoved(pane_id)
            | MuxNotification::PaneOutput(pane_id) => {
                // Ideally we'd check to see if pane_id is part of this window,
                // but overlays may not be 100% associated with the window
                // in the mux and we don't want to lose the invalidation
                // signal for that case, so we just check window validity
                // here and propagate to the window event handler that
                // will then do the check with full context.
                let mux = Mux::get();
                if mux.get_window(mux_window_id).is_none() {
                    // Something inconsistent: cancel subscription
                    log::debug!(
                        "PaneOutput: wanted mux_window_id={} from mux, but \
                         was not found, cancel mux subscription",
                        mux_window_id
                    );
                    return false;
                }
                let _ = pane_id;
            }
            MuxNotification::PaneAdded(_pane_id) => {
                // If some other client spawns a pane inside this window, this
                // gives us an opportunity to attach it to the clipboard.
                let mux = Mux::get();
                return mux.get_window(mux_window_id).is_some();
            }
            MuxNotification::TabAddedToWindow { window_id, .. }
            | MuxNotification::WindowTitleChanged { window_id, .. }
            | MuxNotification::WindowInvalidated(window_id) => {
                if window_id != mux_window_id {
                    return true;
                }
            }
            MuxNotification::WindowRemoved(window_id) => {
                if window_id != mux_window_id {
                    return true;
                }
                // Set the window as dead to unsubscribe from further notifications
                dead.store(true, Ordering::Relaxed);
                return false;
            }
            MuxNotification::TabResized(tab_id)
            | MuxNotification::TabTitleChanged { tab_id, .. } => {
                let mux = Mux::get();
                if mux.window_containing_tab(tab_id) == Some(mux_window_id) {
                    // fall through
                } else {
                    return true;
                }
            }
            MuxNotification::Alert {
                alert: Alert::ToastNotification { .. },
                ..
            }
            | MuxNotification::AssignClipboard { .. }
            | MuxNotification::SaveToDownloads { .. }
            | MuxNotification::WindowCreated(_)
            | MuxNotification::ActiveWorkspaceChanged(_)
            | MuxNotification::WorkspaceRenamed { .. }
            | MuxNotification::Empty
            | MuxNotification::WindowWorkspaceChanged(_) => return true,
            MuxNotification::Alert {
                alert: Alert::PaletteChanged { .. },
                ..
            } => {
                // fall through
            }
        }

        window.notify(TermWindowNotif::MuxNotification(n));

        true
    }

    fn subscribe_to_pane_updates(&self) {
        let window = self.window.clone().expect("window to be valid on startup");
        let mux_window_id = Arc::clone(&self.mux_window_id_for_subscriptions);
        let mux = Mux::get();
        let dead = Arc::new(AtomicBool::new(false));
        mux.subscribe(move |n| {
            if dead.load(Ordering::Relaxed) {
                return false;
            }
            let mux_window_id = *mux_window_id.lock().unwrap();
            let window = window.clone();
            let dead = dead.clone();
            promise::spawn::spawn_into_main_thread(async move {
                Self::mux_pane_output_event_callback(n, &window, mux_window_id, &dead)
            })
            .detach();
            true
        });
    }

    fn emit_status_event(&mut self) {
        // Lua event dispatch removed; no-op
    }

    fn finish_window_event(&mut self, name: &str, _again: bool) {
        // Lua event dispatch removed; clear state
        self.event_states
            .entry(name.to_string())
            .and_modify(|s| *s = EventState::None);
    }

    pub fn emit_window_event(&mut self, _name: &str, _pane_id: Option<PaneId>) {
        // Lua event dispatch removed; no-op
    }

    fn check_for_dirty_lines_and_invalidate_selection(&mut self, pane: &Arc<dyn Pane>) {
        let dims = pane.get_dimensions();
        let viewport = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top);
        let visible_range = viewport..viewport + dims.viewport_rows as StableRowIndex;
        let seqno = self.selection(pane.pane_id()).seqno;
        let dirty = pane.get_changed_since(visible_range, seqno);

        if dirty.is_empty() {
            return;
        }
        if pane.downcast_ref::<CopyOverlay>().is_none()
            && pane.downcast_ref::<QuickSelectOverlay>().is_none()
        {
            // If any of the changed lines intersect with the
            // selection, then we need to clear the selection, but not
            // when the search overlay is active; the search overlay
            // marks lines as dirty to force invalidate them for
            // highlighting purpose but also manipulates the selection
            // and we want to allow it to retain the selection it made!

            let clear_selection =
                if let Some(selection_range) = self.selection(pane.pane_id()).range.as_ref() {
                    let selection_rows = selection_range.rows();
                    selection_rows.into_iter().any(|row| dirty.contains(row))
                } else {
                    false
                };

            if clear_selection {
                self.selection(pane.pane_id()).range.take();
                self.selection(pane.pane_id()).origin.take();
                self.selection(pane.pane_id()).seqno = pane.get_current_seqno();
            }
        }
    }
}

impl TermWindow {
    fn palette(&mut self) -> &ColorPalette {
        if self.palette.is_none() {
            self.palette
                .replace(config::TermConfig::new().color_palette());
        }
        self.palette.as_ref().unwrap()
    }

    pub fn config_was_reloaded(&mut self) {
        log::debug!(
            "config was reloaded, overrides: {:?}",
            self.config_overrides
        );
        self.key_table_state.clear_stack();
        self.connection_name = Connection::get().unwrap().name();
        let config = match config::overridden_config(&self.config_overrides) {
            Ok(config) => config,
            Err(err) => {
                log::error!(
                    "Failed to apply config overrides to window: {:#}: {:?}",
                    err,
                    self.config_overrides
                );
                configuration()
            }
        };
        self.config = config.clone();
        self.palette.take();

        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(window) => window,
            _ => return,
        };
        if window.len() == 1 {
            self.show_tab_bar = config.enable_tab_bar && !config.hide_tab_bar_if_only_one_tab;
        } else {
            self.show_tab_bar = config.enable_tab_bar;
        }
        *self.cursor_blink_state.borrow_mut() = ColorEase::new(
            config.cursor_blink_rate,
            config.cursor_blink_ease_in,
            config.cursor_blink_rate,
            config.cursor_blink_ease_out,
            None,
        );
        *self.blink_state.borrow_mut() = ColorEase::new(
            config.text_blink_rate,
            config.text_blink_ease_in,
            config.text_blink_rate,
            config.text_blink_ease_out,
            None,
        );
        *self.rapid_blink_state.borrow_mut() = ColorEase::new(
            config.text_blink_rate_rapid,
            config.text_blink_rapid_ease_in,
            config.text_blink_rate_rapid,
            config.text_blink_rapid_ease_out,
            None,
        );

        self.show_scroll_bar = config.enable_scroll_bar;
        self.shape_generation += 1;
        {
            let mut shape_cache = self.shape_cache.borrow_mut();
            shape_cache.update_config(&config);
            shape_cache.clear();
        }
        self.line_state_cache.borrow_mut().update_config(&config);
        self.line_quad_cache.borrow_mut().update_config(&config);
        self.line_to_ele_shape_cache
            .borrow_mut()
            .update_config(&config);
        self.fancy_tab_bar.take();
        self.invalidate_fancy_tab_bar();
        self.invalidate_tab_sidebar();
        self.invalidate_modal();
        self.input_map = InputMap::new(&config);
        self.leader_is_down = None;
        self.render_state.as_mut().map(|rs| rs.config_changed());
        let dimensions = self.dimensions;

        if let Err(err) = self.fonts.config_changed(&config) {
            log::error!("Failed to load font configuration: {:#}", err);
        }

        if let Some(window) = mux.get_window(self.mux_window_id) {
            let term_config: Arc<dyn TerminalConfiguration> =
                Arc::new(TermConfig::with_config(config.clone()));
            for tab in window.iter() {
                for pane in tab.iter_panes_ignoring_zoom() {
                    pane.pane.set_config(Arc::clone(&term_config));
                }
            }
            for state in self.pane_state.borrow().values() {
                if let Some(overlay) = &state.overlay {
                    overlay.pane.set_config(Arc::clone(&term_config));
                }
            }
            for state in self.tab_state.borrow().values() {
                if let Some(overlay) = &state.overlay {
                    overlay.pane.set_config(Arc::clone(&term_config));
                }
            }
        }

        // Clear cached per-pane scaled configs so they rebuild with new settings
        self.scaled_font_configs.clear();

        if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
            self.load_os_parameters();
            self.apply_scale_change(&dimensions, self.fonts.get_font_scale());
            self.apply_dimensions(&dimensions, None, &window);
            window.config_did_change(&config);
            window.invalidate();
        }

        // Do this after we've potentially adjusted scaling based on config/padding
        // and window size
        self.window_background = reload_background_image(
            &config,
            &self.window_background,
            &self.dimensions,
            &self.render_metrics,
        );

        self.invalidate_modal();
        self.emit_window_event("window-config-reloaded", None);
    }

    fn invalidate_modal(&mut self) {
        if let Some(modal) = self.get_modal() {
            modal.reconfigure(self);
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
    }

    pub fn cancel_modal(&self) {
        self.modal.borrow_mut().take();
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    pub fn set_modal(&self, modal: Rc<dyn Modal>) {
        self.modal.borrow_mut().replace(modal);
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    fn get_modal(&self) -> Option<Rc<dyn Modal>> {
        self.modal.borrow().as_ref().map(|m| Rc::clone(&m))
    }

    fn update_scrollbar(&mut self) {
        if !self.show_scroll_bar {
            return;
        }

        let tab = match self.get_active_pane_or_overlay() {
            Some(tab) => tab,
            None => return,
        };

        let render_dims = tab.get_dimensions();
        if render_dims == self.last_scroll_info {
            return;
        }

        self.last_scroll_info = render_dims;

        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Called by various bits of code to update the title bar.
    /// Let's also trigger the status event so that it can choose
    /// to update the right-status.
    fn update_title(&mut self) {
        self.schedule_status_update();
        self.update_title_impl();
    }

    fn window_contains_pane(&mut self, pane_id: PaneId) -> bool {
        let mux = Mux::get();

        let (_domain, window_id, _tab_id) = match mux.resolve_pane_id(pane_id) {
            Some(tuple) => tuple,
            None => return false,
        };

        return window_id == self.mux_window_id;
    }

    fn emit_user_var_event(&mut self, pane_id: PaneId, name: String, value: String) {
        if !self.window_contains_pane(pane_id) {
            return;
        }
        if name.starts_with("claude_") {
            // Reset the poll timer so update_sidebar_info() refreshes
            // the data immediately on the next paint frame, not after
            // the 1s throttle expires.
            self.last_sidebar_info_poll = std::time::Instant::now() - std::time::Duration::from_secs(2);
            self.invalidate_tab_sidebar();
        }
        // React immediately to claude_status → waiting_input instead of
        // waiting for the next poll cycle (up to 1s delay).
        if name == "claude_status" && value == "waiting_input" {
            self.check_claude_idle_notifications_now();
        }
        self.update_title();
    }

    /// Called by window:set_right_status after the status has
    /// been updated; let's update the bar
    pub fn update_title_post_status(&mut self) {
        self.update_title_impl();
    }

    fn update_title_impl(&mut self) {
        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(window) => window,
            _ => return,
        };
        let tabs = self.get_tab_information();
        let panes = self.get_pane_information();
        let active_tab = tabs.iter().find(|t| t.is_active).cloned();
        let active_pane = panes.iter().find(|p| p.is_active).cloned();

        let border = self.get_os_border();
        let tab_bar_height = self.tab_bar_pixel_height().unwrap_or(0.);
        let tab_bar_y = if self.config.tab_bar_at_bottom {
            ((self.dimensions.pixel_height as f32) - (tab_bar_height + border.bottom.get() as f32))
                .max(0.)
        } else {
            border.top.get() as f32
        };

        let tab_bar_height = self.tab_bar_pixel_height().unwrap_or(0.);

        let hovering_in_tab_bar = match &self.current_mouse_event {
            Some(event) => {
                let mouse_y = event.coords.y as f32;
                mouse_y >= tab_bar_y as f32 && mouse_y < tab_bar_y as f32 + tab_bar_height
            }
            None => false,
        };

        let new_tab_bar = TabBarState::new(
            self.dimensions.pixel_width / self.render_metrics.cell_size.width as usize,
            if hovering_in_tab_bar {
                Some(self.last_mouse_coords.0)
            } else {
                None
            },
            &tabs,
            &panes,
            self.config.resolved_palette.tab_bar.as_ref(),
            &self.config,
            &self.left_status,
            &self.right_status,
            self.web_server_handle.as_ref().map(|h| h.bind_address()),
        );
        if new_tab_bar != self.tab_bar {
            self.tab_bar = new_tab_bar;
            self.invalidate_fancy_tab_bar();
            self.invalidate_tab_sidebar();
            self.invalidate_modal();
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }

        let num_tabs = window.len();
        if num_tabs == 0 {
            return;
        }
        drop(window);

        // format-window-title Lua callback removed; use default title
        let title = if let (Some(pos), Some(tab)) = (active_pane, active_tab) {
            if num_tabs == 1 {
                format!("{}{}", if pos.is_zoomed { "[Z] " } else { "" }, pos.title)
            } else {
                format!(
                    "{}[{}/{}] {}",
                    if pos.is_zoomed { "[Z] " } else { "" },
                    tab.tab_index + 1,
                    num_tabs,
                    pos.title
                )
            }
        } else {
            "".to_string()
        };

        if let Some(window) = self.window.as_ref() {
            window.set_title(&title);

            let show_tab_bar = if num_tabs == 1 {
                self.config.enable_tab_bar && !self.config.hide_tab_bar_if_only_one_tab
            } else {
                self.config.enable_tab_bar
            };

            // If the number of tabs changed and caused the tab bar to
            // hide/show, then we'll need to resize things.  It is simplest
            // to piggy back on the config reloading code for that, so that
            // is what we're doing.
            if show_tab_bar != self.show_tab_bar {
                self.config_was_reloaded();
            }
        }
        self.schedule_next_status_update();
    }

    fn schedule_next_status_update(&mut self) {
        if let Some(window) = self.window.as_ref() {
            let now = Instant::now();
            if self.last_status_call <= now {
                let interval = Duration::from_millis(self.config.status_update_interval);
                let target = now + interval;
                self.last_status_call = target;

                let window = window.clone();
                promise::spawn::spawn(async move {
                    Timer::at(target).await;
                    window.notify(TermWindowNotif::EmitStatusUpdate);
                })
                .detach();
            }
        }
    }

    fn update_text_cursor(&mut self, pos: &PositionedPane) {
        if let Some(win) = self.window.as_ref() {
            let cursor = pos.pane.get_cursor_position();
            let top = pos.pane.get_dimensions().physical_top;
            let tab_bar_height = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
                self.tab_bar_pixel_height().unwrap()
            } else {
                0.0
            };
            let (padding_left, padding_top) = self.padding_left_top();

            let r = Rect::new(
                Point::new(
                    (((cursor.x + pos.left) as isize).max(0) * self.render_metrics.cell_size.width)
                        .add(padding_left as isize),
                    ((cursor.y + pos.top as isize - top).max(0)
                        * self.render_metrics.cell_size.height)
                        .add(tab_bar_height as isize)
                        .add(padding_top as isize),
                ),
                self.render_metrics.cell_size,
            );
            win.set_text_cursor_position(r);
        }
    }

    fn activate_window(&mut self, window_idx: usize) -> anyhow::Result<()> {
        let windows = front_end().gui_windows();
        if let Some(win) = windows.get(window_idx) {
            win.window.focus();
        }
        Ok(())
    }

    fn activate_window_relative(&mut self, delta: isize, wrap: bool) -> anyhow::Result<()> {
        let windows = front_end().gui_windows();
        let my_idx = windows
            .iter()
            .position(|w| Some(&w.window) == self.window.as_ref())
            .ok_or_else(|| anyhow!("I'm not in the window list!?"))?;

        let idx = my_idx as isize + delta;

        let idx = if wrap {
            let idx = if idx < 0 {
                windows.len() as isize + idx
            } else {
                idx
            };
            idx as usize % windows.len()
        } else {
            if idx < 0 {
                0
            } else if idx >= windows.len() as isize {
                windows.len().saturating_sub(1)
            } else {
                idx as usize
            }
        };

        if let Some(win) = windows.get(idx) {
            win.window.focus();
        }

        Ok(())
    }

    fn activate_tab(&mut self, tab_idx: isize) -> anyhow::Result<()> {
        let mux = Mux::get();
        let mut window = mux
            .get_window_mut(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        // This logic is coupled with the CliSubCommand::ActivateTab
        // logic in terminaler/src/main.rs. If you update this, update that!
        let max = window.len();

        let tab_idx = if tab_idx < 0 {
            max.saturating_sub(tab_idx.abs() as usize)
        } else {
            tab_idx as usize
        };

        if tab_idx < max {
            window.save_and_then_set_active(tab_idx);

            // Collect pane IDs for the newly active tab before dropping the window lock,
            // so we can clear notification badges for all panes in that tab.
            let active_pane_ids: Vec<PaneId> = window
                .get_active()
                .map(|tab| {
                    tab.iter_panes_ignoring_zoom()
                        .into_iter()
                        .map(|p| p.pane.pane_id())
                        .collect()
                })
                .unwrap_or_default();

            drop(window);

            for pane_id in active_pane_ids {
                self.pane_state(pane_id).notification_start = None;
                self.pane_state(pane_id).notification_count = 0;
            }

            if let Some(tab) = self.get_active_pane_or_overlay() {
                tab.focus_changed(true);
            }

            self.update_title();
            self.update_scrollbar();
        }
        Ok(())
    }

    fn activate_tab_relative(&mut self, delta: isize, wrap: bool) -> anyhow::Result<()> {
        let mux = Mux::get();
        let window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let max = window.len();
        ensure!(max > 0, "no more tabs");

        // This logic is coupled with the CliSubCommand::ActivateTab
        // logic in terminaler/src/main.rs. If you update this, update that!
        let active = window.get_active_idx() as isize;
        let tab = active + delta;
        let tab = if wrap {
            let tab = if tab < 0 { max as isize + tab } else { tab };
            (tab as usize % max) as isize
        } else {
            if tab < 0 {
                0
            } else if tab >= max as isize {
                max as isize - 1
            } else {
                tab
            }
        };
        drop(window);
        self.activate_tab(tab)
    }

    fn activate_last_tab(&mut self) -> anyhow::Result<()> {
        let mux = Mux::get();
        let window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let last_idx = window.get_last_active_idx();
        drop(window);
        match last_idx {
            Some(idx) => self.activate_tab(idx as isize),
            None => Ok(()),
        }
    }

    fn move_tab(&mut self, tab_idx: usize) -> anyhow::Result<()> {
        let mux = Mux::get();
        let mut window = mux
            .get_window_mut(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let max = window.len();
        ensure!(max > 0, "no more tabs");

        let active = window.get_active_idx();

        ensure!(tab_idx < max, "cannot move a tab out of range");

        let tab_inst = window.remove_by_idx(active);
        window.insert(tab_idx, &tab_inst);
        window.set_active_without_saving(tab_idx);

        drop(window);
        self.update_title();
        self.update_scrollbar();

        Ok(())
    }

    fn show_input_selector(&mut self, args: &config::keyassignment::InputSelector) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        // Ignore any current overlay: we're going to cancel it out below
        // and we don't want this new one to reference that cancelled pane
        let pane = match self.get_active_pane_no_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let args = args.clone();

        let gui_win = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::selector::selector(term, args, gui_win, pane)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    fn show_prompt_input_line(&mut self, args: &PromptInputLine) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let args = args.clone();

        let gui_win = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::prompt::show_line_prompt_overlay(term, args, gui_win, pane)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    fn show_confirmation(&mut self, args: &Confirmation) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let args = args.clone();

        let gui_win = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::confirm::show_confirmation_overlay(term, args, gui_win, pane)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    fn show_debug_overlay(&mut self) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let gpu_info = if self.webgpu.is_some() {
            "WebGPU".to_string()
        } else if self.gl.is_some() {
            "OpenGL".to_string()
        } else {
            "Software".to_string()
        };

        let config = &self.config;
        let snapshot = crate::overlay::debug::DebugSnapshot {
            fps: self.fps,
            last_frame_ms: self.last_frame_duration.as_secs_f64() * 1000.0,
            window_id: self.mux_window_id,
            terminal_size: (self.terminal_size.cols, self.terminal_size.rows),
            pixel_size: (self.terminal_size.pixel_width, self.terminal_size.pixel_height),
            dpi: self.terminal_size.dpi,
            font_name: format!("{:?}", config.font),
            font_size: config.font_size,
            gpu_info,
            config_file: config::CONFIG_DIRS
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unknown>".into()),
        };

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::debug::debug_overlay(term, snapshot)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    /// Extract the last meaningful text from a pane (for notification context).
    /// Reads the last N lines from the terminal, strips empty lines, and returns
    /// a trimmed excerpt suitable for a notification body.
    fn extract_pane_last_text(pane_id: PaneId, max_lines: usize) -> Option<String> {
        let mux = Mux::get();
        let pane = mux.get_pane(pane_id)?;
        let dims = pane.get_dimensions();

        // Read the last `max_lines * 2` rows to have enough material after filtering
        let fetch_rows = (max_lines * 2).min(dims.scrollback_rows);
        let end = dims.scrollback_rows as StableRowIndex;
        let start = end.saturating_sub(fetch_rows as StableRowIndex);

        let (_first, lines) = pane.get_lines(start..end);

        // Collect non-empty lines as plain text (reverse to get most recent first)
        let mut text_lines: Vec<String> = Vec::new();
        for line in lines.iter().rev() {
            let text = line.as_str().trim().to_string();
            if !text.is_empty() {
                text_lines.push(text);
                if text_lines.len() >= max_lines {
                    break;
                }
            }
        }

        if text_lines.is_empty() {
            return None;
        }

        // Reverse back to chronological order
        text_lines.reverse();

        // Strip common Claude UI chrome (prompt markers, spinners, etc.)
        let cleaned: Vec<&str> = text_lines
            .iter()
            .map(|l| l.as_str())
            // Skip lines that are just prompt characters or spinners
            .filter(|l| {
                !l.chars().all(|c| "❯›>$#%⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏─│╭╮╰╯ ".contains(c))
            })
            .collect();

        if cleaned.is_empty() {
            return None;
        }

        // Truncate each line for notification display
        let result: Vec<String> = cleaned
            .iter()
            .map(|l| {
                if l.chars().count() > 120 {
                    let truncated: String = l.chars().take(119).collect();
                    format!("{}…", truncated)
                } else {
                    l.to_string()
                }
            })
            .collect();

        Some(result.join("\n"))
    }

    /// Extract the Claude prompt/question from the terminal when status is WaitingInput.
    /// Reads the last ~12 lines, filters out Claude UI chrome (spinner, keybinding hints,
    /// box-drawing, option markers), and returns the meaningful prompt text.
    fn extract_claude_prompt(pane_id: PaneId) -> Option<String> {
        let mux = Mux::get();
        let pane = mux.get_pane(pane_id)?;
        let dims = pane.get_dimensions();

        let fetch_rows = 30usize.min(dims.scrollback_rows);
        let end = dims.scrollback_rows as StableRowIndex;
        let start = end.saturating_sub(fetch_rows as StableRowIndex);

        let (_first, lines) = pane.get_lines(start..end);

        let is_chrome = |line: &str| -> bool {
            let l = line.trim();
            if l.is_empty() {
                return true;
            }
            // Spinner-only or prompt-only lines
            if l.chars()
                .all(|c| "❯›>$#%⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏─│╭╮╰╯┌┐└┘├┤┬┴┼ ".contains(c))
            {
                return true;
            }
            // Claude keybinding hints (including wrapped fragments from narrow panes)
            if l.contains("Esc to cancel")
                || l.contains("Tab to amend")
                || l.contains("ctrl+e to explain")
                || l == "to explain"
                || l == "to cancel"
                || l == "to amend"
                || l == "explain"
            {
                return true;
            }
            // Tree connectors and tool invocation lines — match any variant:
            // └ (U+2514), ╰ (U+2570), ⎿ (U+23BF), ├ (U+251C), ╭ (U+256D)
            // ● (U+25CF), ○ (U+25CB)
            // Also catch "Running" in short lines (tree child status)
            if l.starts_with("\u{2514}")
                || l.starts_with("\u{2570}")
                || l.starts_with("\u{23bf}")  // ⎿ (Claude Code tree connector)
                || l.starts_with("\u{251c}")
                || l.starts_with("\u{256d}")
                || l.starts_with("\u{25cf}")
                || l.starts_with("\u{25cb}")
                || (l.len() < 30 && l.contains("Running"))
            {
                return true;
            }
            false
        };

        // Strip option markers (❯, bullet numbers) but keep the text
        let clean_option = |line: &str| -> String {
            let l = line.trim();
            // "❯ 1. Yes" → "1. Yes", "  2. No" → "2. No"
            let l = l.trim_start_matches(|c: char| "❯› ".contains(c));
            l.to_string()
        };

        let mut text_lines: Vec<String> = Vec::new();
        for line in lines.iter().rev() {
            let text = line.as_str().trim().to_string();
            if is_chrome(&text) {
                continue;
            }
            text_lines.push(clean_option(&text));
            if text_lines.len() >= 5 {
                break;
            }
        }

        if text_lines.is_empty() {
            return None;
        }

        text_lines.reverse();

        let truncated: Vec<String> = text_lines
            .iter()
            .map(|l| {
                if l.chars().count() > 100 {
                    let t: String = l.chars().take(99).collect();
                    format!("{}…", t)
                } else {
                    l.clone()
                }
            })
            .collect();

        // Merge consecutive numbered options onto one line
        // e.g. ["1. Yes", "2. No"] → ["1. Yes  |  2. No"]
        let is_option = |s: &str| {
            let s = s.trim().as_bytes();
            s.len() >= 3 && s[0].is_ascii_digit() && s[1] == b'.' && s[2] == b' '
        };
        let mut merged: Vec<String> = Vec::new();
        let mut option_buf: Vec<String> = Vec::new();
        for line in &truncated {
            if is_option(line) {
                option_buf.push(line.clone());
            } else {
                if !option_buf.is_empty() {
                    merged.push(option_buf.join("  |  "));
                    option_buf.clear();
                }
                merged.push(line.clone());
            }
        }
        if !option_buf.is_empty() {
            merged.push(option_buf.join("  |  "));
        }

        Some(merged.join("\n"))
    }

    /// Build a toast notification message. When there's a question, show only the
    /// question — Windows toasts are capped at ~4 body lines so metadata would
    /// push the options off-screen.
    fn claude_notification_message(
        _info: Option<&ClaudeSessionInfo>,
        _cwd: &str,
        question: Option<&str>,
    ) -> String {
        match question {
            Some(q) => q.to_string(),
            None => "Awaiting your input".to_string(),
        }
    }

    /// Build a Slack mrkdwn notification message with context.
    fn claude_slack_message(
        info: Option<&ClaudeSessionInfo>,
        cwd: &str,
        question: Option<&str>,
    ) -> String {
        let mut lines = Vec::new();
        if let Some(q) = question {
            lines.push(q.to_string());
        } else {
            lines.push("Awaiting your input".to_string());
        }
        if let Some(info) = info {
            if let Some(cost) = info.cost_usd {
                lines.push(format!("• Cost: ${:.2}", cost));
            }
        }
        if !cwd.is_empty() {
            lines.push(format!("• Project: {}", cwd));
        }
        lines.join("\n")
    }

    /// Fire notification via toast + Slack.
    fn fire_claude_notification(
        &self,
        info: Option<&ClaudeSessionInfo>,
        cwd: &str,
        question: Option<&str>,
    ) {
        let title = match info.and_then(|i| i.model.as_deref()) {
            Some(model) => format!("Claude ({})", model),
            None => "Claude Code".to_string(),
        };
        let message = Self::claude_notification_message(info, cwd, question);
        let slack_url = self.config.slack_notification_webhook.clone();
        let slack_message = Self::claude_slack_message(info, cwd, question);
        let title_clone = title.clone();

        terminaler_toast_notification::persistent_toast_notification(&title, &message);

        if let Some(url) = slack_url {
            std::thread::spawn(move || {
                terminaler_toast_notification::slack::send_notification_sync(
                    &url,
                    &title_clone,
                    &slack_message,
                );
            });
        }
    }

    /// Check if any Claude pane has gone idle and fire a Windows + Slack notification.
    /// Called every paint frame; internally throttled to check every 1 second.
    ///
    /// Two detection modes:
    /// 1. User-var based (precise): fires on `claude_status` transition to `WaitingInput`
    /// 2. Idle timeout fallback: fires after 3s idle for Claude panes without user vars
    pub fn check_claude_idle_notifications(&mut self) {
        static LAST_CHECK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let last = LAST_CHECK.load(std::sync::atomic::Ordering::Relaxed);
        if now_ms.saturating_sub(last) < 1000 {
            return;
        }
        LAST_CHECK.store(now_ms, std::sync::atomic::Ordering::Relaxed);
        self.check_claude_idle_notifications_inner();
    }

    /// Immediate variant — called from the SetUserVar event handler
    /// when claude_status transitions to waiting_input, bypassing throttle.
    pub fn check_claude_idle_notifications_now(&mut self) {
        self.check_claude_idle_notifications_inner();
    }

    fn check_claude_idle_notifications_inner(&mut self) {
        use mux::pane::CachePolicy;

        let idle_threshold = Duration::from_secs(3);
        let now = Instant::now();

        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(w) => w,
            None => return,
        };

        // Collect Claude pane info from all tabs
        struct ClaudePaneInfo {
            pane_id: PaneId,
            has_user_vars: bool,
            status: Option<ClaudeStatus>,
            session_info: Option<ClaudeSessionInfo>,
            cwd: String,
        }
        let mut claude_panes = vec![];

        for tab in window.iter() {
            let tab_id = tab.tab_id();
            for pos in tab.iter_panes_ignoring_zoom() {
                let pane = &pos.pane;
                let process_name = pane.get_foreground_process_name(CachePolicy::AllowStale);
                let pane_title = pane.get_title();
                let user_vars = pane.copy_user_vars();
                let has_claude_vars = user_vars.keys().any(|k| k.starts_with("claude_"));

                // Detect Claude via:
                // 1. Foreground process name
                // 2. Pane title
                // 3. Full process tree
                // 4. Active user vars (needed for WSL — Windows process tree
                //    only shows wslhost.exe, real Claude process is in Linux VM)
                let is_claude_proc = |name: &str| -> bool {
                    let basename = std::path::Path::new(name)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(name)
                        .to_lowercase();
                    let basename = basename.strip_suffix(".exe").unwrap_or(&basename);
                    matches!(basename, "claude" | "claude-code")
                };
                let has_active_claude_vars = user_vars.contains_key("claude_status");
                let is_claude = process_name.as_deref().map_or(false, &is_claude_proc)
                    || {
                        let lower = pane_title.to_lowercase();
                        lower.contains("claude code")
                            || lower.contains("claude-code")
                            || lower.starts_with("claude ")
                            || lower == "claude"
                    }
                    || pane
                        .get_process_names_in_tree(CachePolicy::AllowStale)
                        .iter()
                        .any(|n| is_claude_proc(n))
                    || has_active_claude_vars;

                if !is_claude {
                    continue;
                }

                let status = user_vars.get("claude_status").map(|s| match s.as_str() {
                    "working" => ClaudeStatus::Working,
                    "waiting_input" => ClaudeStatus::WaitingInput,
                    "idle" => ClaudeStatus::Idle,
                    "error" => ClaudeStatus::Error,
                    _ => ClaudeStatus::Working,
                });

                // Get CWD from sidebar info if available
                let cwd = self
                    .tab_sidebar_info
                    .get(&tab_id)
                    .map(|info| info.cwd_short.clone())
                    .unwrap_or_default();

                let session_info = if has_claude_vars {
                    Some(ClaudeSessionInfo {
                        model: user_vars.get("claude_model").cloned(),
                        context_pct: user_vars
                            .get("claude_context_pct")
                            .and_then(|v| v.parse().ok()),
                        cost_usd: user_vars.get("claude_cost").and_then(|v| v.parse().ok()),
                        duration_ms: user_vars
                            .get("claude_duration_ms")
                            .and_then(|v| v.parse().ok()),
                        lines_added: user_vars
                            .get("claude_lines_added")
                            .and_then(|v| v.parse().ok()),
                        lines_removed: user_vars
                            .get("claude_lines_removed")
                            .and_then(|v| v.parse().ok()),
                        worktree: user_vars.get("claude_worktree").cloned(),
                        status,
                    })
                } else {
                    None
                };

                claude_panes.push(ClaudePaneInfo {
                    pane_id: pane.pane_id(),
                    has_user_vars: has_claude_vars,
                    status,
                    session_info,
                    cwd,
                });
            }
        }
        drop(window);

        for pane_info in &claude_panes {
            let pane_id = pane_info.pane_id;
            let is_muted = self.pane_state(pane_id).notifications_muted;

            if pane_info.has_user_vars {
                // Phase 1: User-var based detection (precise)
                // Fire notification on transition TO WaitingInput
                let prev = self.claude_prev_status.get(&pane_id).copied().flatten();
                let curr = pane_info.status;

                if curr == Some(ClaudeStatus::WaitingInput) && prev != Some(ClaudeStatus::WaitingInput) {
                    if !self.claude_idle_notified.contains(&pane_id) && !is_muted {
                        self.claude_idle_notified.insert(pane_id);
                        // Extract prompt from terminal scrollback (has chrome filtering).
                        // Fall back to claude_question user var if extraction fails.
                        let question: Option<String> =
                            Self::extract_claude_prompt(pane_id).or_else(|| {
                                let mux = Mux::get();
                                mux.get_pane(pane_id).and_then(|p| {
                                    let vars = p.copy_user_vars();
                                    vars.get("claude_question").cloned()
                                })
                            });
                        log::info!(
                            "Claude pane {} status → WaitingInput — firing notification (question: {:?})",
                            pane_id,
                            question,
                        );
                        // Set notification state on PaneState so tab sidebar shows
                        // the flashing indicator until the tab is activated
                        {
                            let mut ps = self.pane_state(pane_id);
                            ps.notification_start.replace(Instant::now());
                            ps.notification_count += 1;
                        }
                        // Force sidebar rebuild so it picks up the new notification_start
                        self.invalidate_tab_sidebar();
                        self.fire_claude_notification(
                            pane_info.session_info.as_ref(),
                            &pane_info.cwd,
                            question.as_deref(),
                        );
                    }
                }

                // Clear notified flag when status moves away from WaitingInput
                if curr != Some(ClaudeStatus::WaitingInput) {
                    self.claude_idle_notified.remove(&pane_id);
                }

                self.claude_prev_status.insert(pane_id, curr);
            } else {
                // Phase 2: Idle timeout fallback (for Claude without user vars)
                if self.claude_idle_notified.contains(&pane_id) || is_muted {
                    continue;
                }
                if let Some(last_output) = self.pane_last_output.get(&pane_id) {
                    let idle_duration = now.duration_since(*last_output);
                    if idle_duration >= idle_threshold {
                        self.claude_idle_notified.insert(pane_id);
                        {
                            let mut ps = self.pane_state(pane_id);
                            ps.notification_start.replace(Instant::now());
                            ps.notification_count += 1;
                        }
                        self.invalidate_tab_sidebar();
                        log::info!(
                            "Claude pane {} idle for {:.1}s — firing notification",
                            pane_id,
                            idle_duration.as_secs_f64(),
                        );
                        self.fire_claude_notification(None, &pane_info.cwd, None);
                    }
                }
            }
        }
    }

    fn show_tab_navigator(&mut self) {
        let mux = Mux::get();
        let active_tab_idx = match mux.get_window(self.mux_window_id) {
            Some(mux_window) => mux_window.get_active_idx(),
            None => return,
        };
        let title = "Tab Navigator".to_string();
        let args = LauncherActionArgs {
            title: Some(title),
            flags: LauncherFlags::TABS,
            help_text: None,
            fuzzy_help_text: None,
            alphabet: None,
        };
        self.show_launcher_impl(args, active_tab_idx);
    }

    fn show_snap_layout_picker(&mut self) {
        let title = "Snap Layout".to_string();
        let args = LauncherActionArgs {
            title: Some(title),
            flags: LauncherFlags::LAYOUTS | LauncherFlags::FUZZY,
            help_text: None,
            fuzzy_help_text: None,
            alphabet: None,
        };
        self.show_launcher_impl(args, 0);
    }

    fn apply_snap_layout(&mut self, name: &str) {
        let layout = match terminaler_layout::find_builtin(name) {
            Some(l) => l,
            None => {
                log::error!("Unknown snap layout: {}", name);
                return;
            }
        };

        let splits = terminaler_layout::collect_splits(&layout.root);
        if splits.is_empty() {
            return;
        }

        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let initial_pane_id = match tab.get_active_pane() {
            Some(p) => p.pane_id(),
            None => return,
        };
        drop(tab);
        drop(mux);

        let term_config = Arc::new(TermConfig::with_config(self.config.clone()));

        // Apply all splits in a single async task so each split completes
        // before the next begins, and we can target the correct pane by ID.
        promise::spawn::spawn(async move {
            let mux = Mux::get();
            // Track pane IDs in creation order so pane_index lookups work.
            let mut pane_ids: Vec<PaneId> = vec![initial_pane_id];

            for split in &splits {
                let target_pane_id = match pane_ids.get(split.pane_index) {
                    Some(&id) => id,
                    None => {
                        log::error!(
                            "snap layout: invalid pane_index {} (have {} panes)",
                            split.pane_index,
                            pane_ids.len()
                        );
                        break;
                    }
                };

                let direction = match split.direction {
                    terminaler_layout::SplitDirection::Horizontal => {
                        SplitDirection::Horizontal
                    }
                    terminaler_layout::SplitDirection::Vertical => {
                        SplitDirection::Vertical
                    }
                };
                // ratio is the first child's share; Percent is the size of the
                // new (second) child, so invert.
                let percent = ((1.0 - split.ratio) * 100.0) as u8;

                let (new_pane, _size) = match mux
                    .split_pane(
                        target_pane_id,
                        SplitRequest {
                            direction,
                            target_is_second: true,
                            size: MuxSplitSize::Percent(percent),
                            top_level: false,
                        },
                        mux::domain::SplitSource::Spawn {
                            command: None,
                            command_dir: None,
                        },
                        config::keyassignment::SpawnTabDomain::CurrentPaneDomain,
                    )
                    .await
                {
                    Ok(result) => result,
                    Err(err) => {
                        log::error!("snap layout split failed: {:#}", err);
                        break;
                    }
                };
                new_pane.set_config(term_config.clone());
                pane_ids.push(new_pane.pane_id());
            }
        })
        .detach();
    }

    pub fn apply_snap_layout_to_pane(&mut self, pane_id: PaneId, name: &str) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        // Activate the target pane so apply_snap_layout splits from it
        let panes = tab.iter_panes();
        if let Some(pos) = panes.iter().find(|p| p.pane.pane_id() == pane_id) {
            tab.set_active_idx(pos.index);
        }
        drop(tab);
        drop(mux);
        self.apply_snap_layout(name);
    }

    fn show_workspace_picker(&mut self) {
        let title = "Workspaces".to_string();
        let args = LauncherActionArgs {
            title: Some(title),
            flags: LauncherFlags::WORKSPACES | LauncherFlags::FUZZY,
            help_text: None,
            fuzzy_help_text: None,
            alphabet: None,
        };
        self.show_launcher_impl(args, 0);
    }

    fn toggle_remote_access(&mut self) {
        if let Some(handle) = self.web_server_handle.take() {
            handle.shutdown_nonblocking();
            log::info!("Remote access stopped");
        } else {
            let cfg = config::configuration();
            let web_config = cfg
                .web_access
                .as_ref()
                .cloned()
                .unwrap_or_default();
            match terminaler_web::start_web_server((&web_config).into()) {
                Ok(handle) => {
                    log::info!("Remote access started");
                    self.web_server_handle = Some(handle);
                }
                Err(e) => {
                    log::error!("Failed to start remote access: {:#}", e);
                }
            }
        }
        self.update_title_post_status();
    }

    fn copy_remote_url_to_clipboard(&self) {
        let url_path = if let Some(ref dir) = *config::PORTABLE_DIR {
            dir.join("web-url")
        } else if cfg!(windows) {
            let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(appdata)
                .join("Terminaler")
                .join("web-url")
        } else {
            dirs_next::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from(".config"))
                .join("terminaler")
                .join("web-url")
        };
        match std::fs::read_to_string(&url_path) {
            Ok(url) => {
                self.copy_to_clipboard(
                    config::keyassignment::ClipboardCopyDestination::Clipboard,
                    url.trim().to_string(),
                );
                log::info!("Remote access URL copied to clipboard");
            }
            Err(e) => {
                log::warn!("Could not read remote URL file: {:#}", e);
            }
        }
    }

    fn show_launcher(&mut self) {
        let title = "Launcher".to_string();
        let args = LauncherActionArgs {
            title: Some(title),
            flags: LauncherFlags::LAUNCH_MENU_ITEMS
                | LauncherFlags::WORKSPACES
                | LauncherFlags::DOMAINS
                | LauncherFlags::KEY_ASSIGNMENTS
                | LauncherFlags::COMMANDS,
            help_text: None,
            fuzzy_help_text: None,
            alphabet: None,
        };
        self.show_launcher_impl(args, 0);
    }

    fn show_launcher_impl(&mut self, args: LauncherActionArgs, initial_choice_idx: usize) {
        let mux_window_id = self.mux_window_id;
        let window = self.window.as_ref().unwrap().clone();

        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let domain_id_of_current_pane = tab
            .get_active_pane()
            .expect("tab has no panes!")
            .domain_id();
        let pane_id = pane.pane_id();
        let tab_id = tab.tab_id();
        let title = args.title.unwrap();
        let flags = args.flags;
        let help_text = args.help_text.unwrap_or(
            "Select an item and press Enter=launch  \
             Esc=cancel  /=filter"
                .to_string(),
        );
        let fuzzy_help_text = args
            .fuzzy_help_text
            .unwrap_or("Fuzzy matching: ".to_string());

        let config = &self.config;
        let alphabet = args.alphabet.unwrap_or(config.launcher_alphabet.clone());

        promise::spawn::spawn(async move {
            let args = LauncherArgs::new(
                &title,
                flags,
                mux_window_id,
                pane_id,
                domain_id_of_current_pane,
                &help_text,
                &fuzzy_help_text,
                &alphabet,
            )
            .await;

            let win = window.clone();
            win.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                let mux = Mux::get();
                if let Some(tab) = mux.get_tab(tab_id) {
                    let window = window.clone();
                    let (overlay, future) =
                        start_overlay(term_window, &tab, move |_tab_id, term| {
                            launcher(args, term, window, initial_choice_idx)
                        });

                    term_window.assign_overlay(tab_id, overlay);
                    promise::spawn::spawn(future).detach();
                }
            })));
        })
        .detach();
    }

    /// Returns the Prompt semantic zones
    fn get_semantic_prompt_zones(&mut self, pane: &Arc<dyn Pane>) -> &[StableRowIndex] {
        let cache = self
            .semantic_zones
            .entry(pane.pane_id())
            .or_insert_with(SemanticZoneCache::default);

        let seqno = pane.get_current_seqno();
        if cache.seqno != seqno {
            let zones = pane.get_semantic_zones().unwrap_or_else(|_| vec![]);
            let mut zones: Vec<StableRowIndex> = zones
                .into_iter()
                .filter_map(|zone| {
                    if zone.semantic_type == terminaler_term::SemanticType::Prompt {
                        Some(zone.start_y)
                    } else {
                        None
                    }
                })
                .collect();
            // dedup to avoid issues where both left and right prompts are
            // defined: we only care if there were 1+ prompts on a line,
            // not about how many prompts are on a line.
            // <https://github.com/wez/wezterm/issues/1121>
            zones.dedup();
            cache.zones = zones;
            cache.seqno = seqno;
        }
        &cache.zones
    }

    fn scroll_to_prompt(&mut self, amount: isize, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        let dims = pane.get_dimensions();
        let position = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top);
        let zone = {
            let zones = self.get_semantic_prompt_zones(&pane);
            let idx = match zones.binary_search(&position) {
                Ok(idx) | Err(idx) => idx,
            };
            let idx = ((idx as isize) + amount).max(0) as usize;
            zones.get(idx).cloned()
        };
        if let Some(zone) = zone {
            self.set_viewport(pane.pane_id(), Some(zone), dims);
        }

        if let Some(win) = self.window.as_ref() {
            win.invalidate();
        }
        Ok(())
    }

    fn scroll_by_page(&mut self, amount: f64, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        let dims = pane.get_dimensions();
        let position = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top) as f64
            + (amount * dims.viewport_rows as f64);
        self.set_viewport(pane.pane_id(), Some(position as isize), dims);
        if let Some(win) = self.window.as_ref() {
            win.invalidate();
        }
        Ok(())
    }

    fn scroll_by_current_event_wheel_delta(&mut self, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        if let Some(event) = &self.current_mouse_event {
            let amount = match event.kind {
                MouseEventKind::VertWheel(amount) => -amount,
                _ => return Ok(()),
            };
            self.scroll_by_line(amount.into(), pane)?;
        }
        Ok(())
    }

    fn scroll_by_line(&mut self, amount: isize, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        let dims = pane.get_dimensions();
        let position = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top)
            .saturating_add(amount);
        self.set_viewport(pane.pane_id(), Some(position), dims);
        if let Some(win) = self.window.as_ref() {
            win.invalidate();
        }
        Ok(())
    }

    fn move_tab_relative(&mut self, delta: isize) -> anyhow::Result<()> {
        let mux = Mux::get();
        let window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let max = window.len();
        ensure!(max > 0, "no more tabs");

        let active = window.get_active_idx();
        let tab = active as isize + delta;
        let tab = if tab < 0 {
            0usize
        } else if tab >= max as isize {
            max - 1
        } else {
            tab as usize
        };

        drop(window);
        self.move_tab(tab)
    }

    pub fn perform_key_assignment(
        &mut self,
        pane: &Arc<dyn Pane>,
        assignment: &KeyAssignment,
    ) -> anyhow::Result<PerformAssignmentResult> {
        use KeyAssignment::*;

        if let Some(modal) = self.get_modal() {
            if modal.perform_assignment(assignment, self) {
                return Ok(PerformAssignmentResult::Handled);
            }
        }

        match pane.perform_assignment(assignment) {
            PerformAssignmentResult::Unhandled => {}
            result => return Ok(result),
        }

        let window = self.window.as_ref().map(|w| w.clone());

        match assignment {
            ActivateKeyTable {
                name,
                timeout_milliseconds,
                replace_current,
                one_shot,
                until_unknown,
                prevent_fallback,
            } => {
                anyhow::ensure!(
                    self.input_map.has_table(name),
                    "ActivateKeyTable: no key_table named {}",
                    name
                );
                self.key_table_state.activate(KeyTableArgs {
                    name,
                    timeout_milliseconds: *timeout_milliseconds,
                    replace_current: *replace_current,
                    one_shot: *one_shot,
                    until_unknown: *until_unknown,
                    prevent_fallback: *prevent_fallback,
                });
                self.update_title();
            }
            PopKeyTable => {
                self.key_table_state.pop();
                self.update_title();
            }
            ClearKeyTableStack => {
                self.key_table_state.clear_stack();
                self.update_title();
            }
            Multiple(actions) => {
                for a in actions {
                    self.perform_key_assignment(pane, a)?;
                }
            }
            SpawnTab(spawn_where) => {
                self.spawn_tab(spawn_where);
            }
            SpawnWindow => {
                self.spawn_command(&SpawnCommand::default(), SpawnWhere::NewWindow);
            }
            SpawnCommandInNewTab(spawn) => {
                self.spawn_command(spawn, SpawnWhere::NewTab);
            }
            SpawnCommandInNewWindow(spawn) => {
                self.spawn_command(spawn, SpawnWhere::NewWindow);
            }
            SplitHorizontal(spawn) => {
                log::trace!("SplitHorizontal {:?}", spawn);
                self.spawn_command(
                    spawn,
                    SpawnWhere::SplitPane(SplitRequest {
                        direction: SplitDirection::Horizontal,
                        target_is_second: true,
                        size: MuxSplitSize::Percent(50),
                        top_level: false,
                    }),
                );
            }
            SplitVertical(spawn) => {
                log::trace!("SplitVertical {:?}", spawn);
                self.spawn_command(
                    spawn,
                    SpawnWhere::SplitPane(SplitRequest {
                        direction: SplitDirection::Vertical,
                        target_is_second: true,
                        size: MuxSplitSize::Percent(50),
                        top_level: false,
                    }),
                );
            }
            ToggleFullScreen => {
                self.window.as_ref().unwrap().toggle_fullscreen();
            }
            ToggleAlwaysOnTop => {
                let window = self.window.clone().unwrap();
                let current_level = self.window_state.as_window_level();

                match current_level {
                    WindowLevel::AlwaysOnTop => {
                        window.set_window_level(WindowLevel::Normal);
                    }
                    WindowLevel::AlwaysOnBottom | WindowLevel::Normal => {
                        window.set_window_level(WindowLevel::AlwaysOnTop);
                    }
                }
            }
            ToggleAlwaysOnBottom => {
                let window = self.window.clone().unwrap();
                let current_level = self.window_state.as_window_level();

                match current_level {
                    WindowLevel::AlwaysOnBottom => {
                        window.set_window_level(WindowLevel::Normal);
                    }
                    WindowLevel::AlwaysOnTop | WindowLevel::Normal => {
                        window.set_window_level(WindowLevel::AlwaysOnBottom);
                    }
                }
            }
            SetWindowLevel(level) => {
                let window = self.window.clone().unwrap();
                window.set_window_level(level.clone());
            }
            CopyTo(dest) => {
                let text = self.selection_text(pane);
                self.copy_to_clipboard(*dest, text);
            }
            CopyTextTo { text, destination } => {
                self.copy_to_clipboard(*destination, text.clone());
            }
            PasteFrom(source) => {
                self.paste_from_clipboard(pane, *source);
            }
            ActivateTabRelative(n) => {
                self.activate_tab_relative(*n, true)?;
            }
            ActivateTabRelativeNoWrap(n) => {
                self.activate_tab_relative(*n, false)?;
            }
            ActivateLastTab => self.activate_last_tab()?,
            DecreaseFontSize => {
                let pane_id = pane.pane_id();
                let current = self.pane_font_scale(pane_id);
                let new_scale = (current / 1.1).max(0.5);
                self.pane_state(pane_id).font_scale = new_scale;
                self.resize_pane_for_font_scale(pane_id);
                self.shape_generation += 1;
                self.shape_cache.borrow_mut().clear();
                self.invalidate_tab_sidebar();
                if let Some(w) = window.as_ref() {
                    w.invalidate();
                }
            }
            IncreaseFontSize => {
                let pane_id = pane.pane_id();
                let current = self.pane_font_scale(pane_id);
                let new_scale = (current * 1.1).min(3.0);
                self.pane_state(pane_id).font_scale = new_scale;
                self.resize_pane_for_font_scale(pane_id);
                self.shape_generation += 1;
                self.shape_cache.borrow_mut().clear();
                self.invalidate_tab_sidebar();
                if let Some(w) = window.as_ref() {
                    w.invalidate();
                }
            }
            ResetFontSize => {
                let pane_id = pane.pane_id();
                let had_scale = {
                    let mut ps = self.pane_state(pane_id);
                    let had = (ps.font_scale - 1.0).abs() > 0.001;
                    ps.font_scale = 1.0;
                    had
                };
                if had_scale {
                    self.resize_pane_for_font_scale(pane_id);
                    self.shape_generation += 1;
                    self.shape_cache.borrow_mut().clear();
                }
                self.invalidate_tab_sidebar();
                if let Some(w) = window.as_ref() {
                    w.invalidate();
                }
            }
            ResetFontAndWindowSize => {
                if let Some(w) = window.as_ref() {
                    self.reset_font_and_window_size(&w)?
                }
            }
            TogglePaneNotifications => {
                let pane_id = pane.pane_id();
                let muted = {
                    let mut ps = self.pane_state(pane_id);
                    ps.notifications_muted = !ps.notifications_muted;
                    if ps.notifications_muted {
                        ps.notification_start = None;
                        ps.notification_count = 0;
                    }
                    ps.notifications_muted
                };
                // Sync with the global static so frontend.rs can check it
                {
                    let mut set = MUTED_PANES.lock().unwrap();
                    if muted {
                        set.insert(pane_id);
                    } else {
                        set.remove(&pane_id);
                    }
                }
                log::info!("Pane {} notifications muted: {}", pane_id, muted);
                self.invalidate_tab_sidebar();
                if let Some(w) = window.as_ref() {
                    w.invalidate();
                }
            }
            ActivateTab(n) => {
                self.activate_tab(*n)?;
            }
            ActivateWindow(n) => {
                self.activate_window(*n)?;
            }
            ActivateWindowRelative(n) => {
                self.activate_window_relative(*n, true)?;
            }
            ActivateWindowRelativeNoWrap(n) => {
                self.activate_window_relative(*n, false)?;
            }
            SendString(s) => pane.writer().write_all(s.as_bytes())?,
            SendKey(key) => {
                use keyevent::Key;
                let mods = key.mods;
                if let Key::Code(key) = self.win_key_code_to_termwiz_key_code(
                    &key.key.resolve(self.config.key_map_preference),
                ) {
                    pane.key_down(key, mods)?;
                }
            }
            Hide => {
                if let Some(w) = window.as_ref() {
                    w.hide();
                }
            }
            Show => {
                if let Some(w) = window.as_ref() {
                    w.show();
                }
            }
            CloseCurrentTab { confirm } => self.close_current_tab(*confirm),
            CloseCurrentPane { confirm } => self.close_current_pane(*confirm),
            Nop | DisableDefaultAssignment => {}
            ReloadConfiguration => config::reload(),
            MoveTab(n) => self.move_tab(*n)?,
            MoveTabRelative(n) => self.move_tab_relative(*n)?,
            ScrollByPage(n) => self.scroll_by_page(**n, pane)?,
            ScrollByLine(n) => self.scroll_by_line(*n, pane)?,
            ScrollByCurrentEventWheelDelta => self.scroll_by_current_event_wheel_delta(pane)?,
            ScrollToPrompt(n) => self.scroll_to_prompt(*n, pane)?,
            ScrollToTop => self.scroll_to_top(pane),
            ScrollToBottom => self.scroll_to_bottom(pane),
            ShowTabNavigator => self.show_tab_navigator(),
            ShowDebugOverlay => self.show_debug_overlay(),
            ShowLauncher => self.show_launcher(),
            ShowLauncherArgs(args) => {
                let title = args.title.clone().unwrap_or("Launcher".to_string());
                let args = LauncherActionArgs {
                    title: Some(title),
                    flags: args.flags,
                    help_text: args.help_text.clone(),
                    fuzzy_help_text: args.fuzzy_help_text.clone(),
                    alphabet: args.alphabet.clone(),
                };
                self.show_launcher_impl(args, 0);
            }
            HideApplication => {
                let con = Connection::get().expect("call on gui thread");
                con.hide_application();
            }
            QuitApplication => {
                let mux = Mux::get();
                let config = &self.config;
                log::info!("QuitApplication over here (window)");

                match config.window_close_confirmation {
                    WindowCloseConfirmation::NeverPrompt => {
                        let con = Connection::get().expect("call on gui thread");
                        con.terminate_message_loop();
                    }
                    WindowCloseConfirmation::AlwaysPrompt => {
                        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                            Some(tab) => tab,
                            None => anyhow::bail!("no active tab!?"),
                        };

                        let window = self.window.clone().unwrap();
                        let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                            confirm_quit_program(term, window, tab_id)
                        });
                        self.assign_overlay(tab.tab_id(), overlay);
                        promise::spawn::spawn(future).detach();
                    }
                }
            }
            SelectTextAtMouseCursor(mode) => self.select_text_at_mouse_cursor(*mode, pane),
            ExtendSelectionToMouseCursor(mode) => {
                self.extend_selection_at_mouse_cursor(*mode, pane)
            }
            ClearSelection => {
                self.clear_selection(pane);
            }
            StartWindowDrag => {
                self.window_drag_position = self.current_mouse_event.clone();
            }
            OpenLinkAtMouseCursor => {
                self.do_open_link_at_mouse_cursor(pane);
            }
            EmitEvent(name) => {
                self.emit_window_event(name, None);
            }
            CompleteSelectionOrOpenLinkAtMouseCursor(dest) => {
                let text = self.selection_text(pane);
                if !text.is_empty() {
                    self.copy_to_clipboard(*dest, text);
                    let window = self.window.as_ref().unwrap();
                    window.invalidate();
                } else {
                    self.do_open_link_at_mouse_cursor(pane);
                }
            }
            CompleteSelection(dest) => {
                let text = self.selection_text(pane);
                if !text.is_empty() {
                    self.copy_to_clipboard(*dest, text);
                    let window = self.window.as_ref().unwrap();
                    window.invalidate();
                }
            }
            ClearScrollback(erase_mode) => {
                pane.erase_scrollback(*erase_mode);
                let window = self.window.as_ref().unwrap();
                window.invalidate();
            }
            Search(pattern) => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    let mut replace_current = false;
                    if let Some(existing) = pane.downcast_ref::<CopyOverlay>() {
                        let mut params = existing.get_params();
                        params.editing_search = true;
                        if !pattern.is_empty() {
                            params.pattern = self.resolve_search_pattern(pattern.clone(), &pane);
                        }
                        existing.apply_params(params);
                        replace_current = true;
                    } else {
                        let search = CopyOverlay::with_pane(
                            self,
                            &pane,
                            CopyModeParams {
                                pattern: self.resolve_search_pattern(pattern.clone(), &pane),
                                editing_search: true,
                            },
                        )?;
                        self.assign_overlay_for_pane(pane.pane_id(), search);
                    }
                    self.pane_state(pane.pane_id())
                        .overlay
                        .as_mut()
                        .map(|overlay| {
                            overlay.key_table_state.activate(KeyTableArgs {
                                name: "search_mode",
                                timeout_milliseconds: None,
                                replace_current,
                                one_shot: false,
                                until_unknown: false,
                                prevent_fallback: false,
                            });
                        });
                }
            }
            QuickSelect => {
                if let Some(pane) = self.get_active_pane_no_overlay() {
                    let qa = QuickSelectOverlay::with_pane(
                        self,
                        &pane,
                        &QuickSelectArguments::default(),
                    );
                    self.assign_overlay_for_pane(pane.pane_id(), qa);
                }
            }
            QuickSelectArgs(args) => {
                if let Some(pane) = self.get_active_pane_no_overlay() {
                    let qa = QuickSelectOverlay::with_pane(self, &pane, args);
                    self.assign_overlay_for_pane(pane.pane_id(), qa);
                }
            }
            ActivateCopyMode => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    let mut replace_current = false;
                    if let Some(existing) = pane.downcast_ref::<CopyOverlay>() {
                        let mut params = existing.get_params();
                        params.editing_search = false;
                        existing.apply_params(params);
                        replace_current = true;
                    } else {
                        let copy = CopyOverlay::with_pane(
                            self,
                            &pane,
                            CopyModeParams {
                                pattern: MuxPattern::default(),
                                editing_search: false,
                            },
                        )?;
                        self.assign_overlay_for_pane(pane.pane_id(), copy);
                    }
                    self.pane_state(pane.pane_id())
                        .overlay
                        .as_mut()
                        .map(|overlay| {
                            overlay.key_table_state.activate(KeyTableArgs {
                                name: "copy_mode",
                                timeout_milliseconds: None,
                                replace_current,
                                one_shot: false,
                                until_unknown: false,
                                prevent_fallback: false,
                            });
                        });
                }
            }
            AdjustPaneSize(direction, amount) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };

                let tab_id = tab.tab_id();

                if self.tab_state(tab_id).overlay.is_none() {
                    tab.adjust_pane_size(*direction, *amount);
                }
            }
            ActivatePaneByIndex(index) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };

                let tab_id = tab.tab_id();

                if self.tab_state(tab_id).overlay.is_none() {
                    let panes = tab.iter_panes();
                    if panes.iter().position(|p| p.index == *index).is_some() {
                        tab.set_active_idx(*index);
                    }
                }
            }
            ActivatePaneDirection(direction) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };

                let tab_id = tab.tab_id();

                if self.tab_state(tab_id).overlay.is_none() {
                    tab.activate_pane_direction(*direction);
                }
            }
            TogglePaneZoomState => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };
                tab.toggle_zoom();
            }
            SetPaneZoomState(zoomed) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };
                tab.set_zoomed(*zoomed);
            }
            SwitchWorkspaceRelative(delta) => {
                let mux = Mux::get();
                let workspace = mux.active_workspace();
                let workspaces = mux.iter_workspaces();
                let idx = workspaces.iter().position(|w| *w == workspace).unwrap_or(0);
                let new_idx = idx as isize + delta;
                let new_idx = if new_idx < 0 {
                    workspaces.len() as isize + new_idx
                } else {
                    new_idx
                };
                let new_idx = new_idx as usize % workspaces.len();
                if let Some(w) = workspaces.get(new_idx) {
                    front_end().switch_workspace(w);
                }
            }
            SwitchToWorkspace { name, spawn } => {
                let activity = crate::Activity::new();
                let mux = Mux::get();
                let name = name
                    .as_ref()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| mux.generate_workspace_name());
                let switcher = crate::frontend::WorkspaceSwitcher::new(&name);
                mux.set_active_workspace(&name);

                if mux.iter_windows_in_workspace(&name).is_empty() {
                    let spawn = spawn.as_ref().map(|s| s.clone()).unwrap_or_default();
                    let size = self.terminal_size;
                    let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
                    let src_window_id = self.mux_window_id;

                    promise::spawn::spawn(async move {
                        if let Err(err) = crate::spawn::spawn_command_internal(
                            spawn,
                            SpawnWhere::NewWindow,
                            size,
                            Some(src_window_id),
                            term_config,
                        )
                        .await
                        {
                            log::error!("Failed to spawn: {:#}", err);
                        }
                        switcher.do_switch();
                        drop(activity);
                    })
                    .detach();
                } else {
                    switcher.do_switch();
                }
            }
            DetachDomain(domain) => {
                let domain = Mux::get().resolve_spawn_tab_domain(Some(pane.pane_id()), domain)?;
                domain.detach()?;
            }
            AttachDomain(domain) => {
                let window = self.mux_window_id;
                let domain = domain.to_string();
                let dpi = self.dimensions.dpi as u32;

                promise::spawn::spawn(async move {
                    let mux = Mux::get();
                    let domain = mux
                        .get_domain_by_name(&domain)
                        .ok_or_else(|| anyhow!("{} is not a valid domain name", domain))?;
                    domain.attach(Some(window)).await?;

                    let have_panes_in_domain = mux
                        .iter_panes()
                        .iter()
                        .any(|p| p.domain_id() == domain.domain_id());

                    if !have_panes_in_domain {
                        let config = config::configuration();
                        let _tab = domain
                            .spawn(
                                config.initial_size(
                                    dpi,
                                    Some(crate::cell_pixel_dims(&config, dpi as f64)?),
                                ),
                                None,
                                None,
                                window,
                            )
                            .await?;
                    }

                    Result::<(), anyhow::Error>::Ok(())
                })
                .detach();
            }
            CopyMode(_) => {
                // NOP here; handled by the overlay directly
            }
            RotatePanes(direction) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };
                match direction {
                    RotationDirection::Clockwise => tab.rotate_clockwise(),
                    RotationDirection::CounterClockwise => tab.rotate_counter_clockwise(),
                }
            }
            SplitPane(split) => {
                log::trace!("SplitPane {:?}", split);
                self.spawn_command(
                    &split.command,
                    SpawnWhere::SplitPane(SplitRequest {
                        direction: match split.direction {
                            PaneDirection::Down | PaneDirection::Up => SplitDirection::Vertical,
                            PaneDirection::Left | PaneDirection::Right => {
                                SplitDirection::Horizontal
                            }
                            PaneDirection::Next | PaneDirection::Prev => {
                                log::error!(
                                    "Invalid direction {:?} for SplitPane",
                                    split.direction
                                );
                                return Ok(PerformAssignmentResult::Handled);
                            }
                        },
                        target_is_second: match split.direction {
                            PaneDirection::Down | PaneDirection::Right => true,
                            PaneDirection::Up | PaneDirection::Left => false,
                            PaneDirection::Next | PaneDirection::Prev => unreachable!(),
                        },
                        size: match split.size {
                            SplitSize::Percent(n) => MuxSplitSize::Percent(n),
                            SplitSize::Cells(n) => MuxSplitSize::Cells(n),
                        },
                        top_level: split.top_level,
                    }),
                );
            }
            PaneSelect(args) => {
                let modal = crate::termwindow::paneselect::PaneSelector::new(self, args);
                self.set_modal(Rc::new(modal));
            }
            CharSelect(args) => {
                let modal = crate::termwindow::charselect::CharSelector::new(self, args);
                self.set_modal(Rc::new(modal));
            }
            ResetTerminal => {
                pane.perform_actions(vec![termwiz::escape::Action::Esc(
                    termwiz::escape::Esc::Code(termwiz::escape::EscCode::FullReset),
                )]);
            }
            OpenUri(link) => {
                // STRIPPED: terminaler_open_url removed; open URL via OS shell command
                log::info!("OpenUri: {}", link);
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &link])
                        .spawn();
                }
            }
            ActivateCommandPalette => {
                let modal = crate::termwindow::palette::CommandPalette::new(self);
                self.set_modal(Rc::new(modal));
            }
            PromptInputLine(args) => self.show_prompt_input_line(args),
            InputSelector(args) => self.show_input_selector(args),
            Confirmation(args) => self.show_confirmation(args),
            SnapLayoutPicker => {
                log::info!("SnapLayoutPicker: showing layout picker");
                self.show_snap_layout_picker();
            }
            ApplySnapLayout(name) => {
                log::info!("ApplySnapLayout: {}", name);
                self.apply_snap_layout(name);
            }
            SwitchWorkspace(name) => {
                front_end().switch_workspace(name);
            }
            WorkspacePicker => {
                log::info!("WorkspacePicker: showing workspace picker");
                self.show_workspace_picker();
            }
            ToggleRemoteAccess => {
                self.toggle_remote_access();
            }
            ToggleTabSidebar => {
                self.show_tab_sidebar = !self.show_tab_sidebar;
                #[cfg(windows)]
                if let Some(ref wv) = self.webview_sidebar {
                    wv.set_visible(self.show_tab_sidebar);
                }
                self.invalidate_tab_sidebar();
                self.invalidate_fancy_tab_bar();
                if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
                    self.apply_dimensions(&self.dimensions.clone(), None, &window);
                    window.invalidate();
                }
            }
        };
        Ok(PerformAssignmentResult::Handled)
    }

    // ── WebView sidebar helpers ──────────────────────────────────────

    /// Initialize the WebView2 sidebar child window.
    #[cfg(windows)]
    fn try_init_webview_sidebar(&mut self, window: &Window) {
        if !self.show_tab_sidebar {
            return;
        }

        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let hwnd = match window.window_handle() {
            Ok(handle) => match handle.as_raw() {
                RawWindowHandle::Win32(h) => h.hwnd.get() as isize,
                _ => return,
            },
            Err(_) => return,
        };

        let (x, y, w, h) = self.compute_sidebar_geometry();

        match crate::webview_sidebar::WebViewSidebar::new(
            hwnd,
            x,
            y,
            w,
            h,
        ) {
            Ok(wv) => {
                log::info!("WebView sidebar initialized");
                self.webview_sidebar = Some(wv);
            }
            Err(e) => {
                log::warn!("WebView2 unavailable, using GPU sidebar: {:#}", e);
            }
        }
    }

    /// Compute sidebar pixel geometry: (x, y, width, height).
    /// Leaves a 6px strip on the inner edge for the resize handle.
    #[cfg(windows)]
    fn compute_sidebar_geometry(&self) -> (i32, i32, u32, u32) {
        let border = self.get_os_border();
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.) as i32
        } else {
            0
        };
        let handle_width = 6u32; // resize handle strip exposed to parent
        let sidebar_width = (self.tab_sidebar_width as u32).saturating_sub(handle_width);
        let y = border.top.get() as i32 + tab_bar_height + 7;
        let height = (self.dimensions.pixel_height as i32 - y).max(0) as u32;

        let x = match self.config.tab_sidebar_position {
            config::TabSidebarPosition::Left => border.left.get() as i32,
            config::TabSidebarPosition::Right => {
                // Push WebView inward, leaving handle_width on the left edge
                self.dimensions.pixel_width as i32
                    - self.tab_sidebar_width as i32
                    - border.right.get() as i32
                    + handle_width as i32
            }
        };

        (x, y, sidebar_width, height)
    }

    /// Handle an IPC action message from the WebView sidebar JS.
    #[cfg(windows)]
    fn handle_sidebar_ipc(&mut self, msg: &str) {
        let action: serde_json::Value = match serde_json::from_str(msg) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("Invalid sidebar IPC: {}: {}", msg, e);
                return;
            }
        };

        let action_type = action["type"].as_str().unwrap_or("");
        if action_type != "refocus" {
            log::info!("sidebar IPC: {}", msg);
        }
        match action_type {
            "activate_tab" => {
                if let Some(idx) = action["tabIdx"].as_u64() {
                    self.activate_tab(idx as isize).ok();
                }
            }
            "close_tab" => {
                if let Some(idx) = action["tabIdx"].as_u64() {
                    // Activate the tab first, then close it
                    self.activate_tab(idx as isize).ok();
                    self.close_current_tab(true);
                }
            }
            "close_pane" => {
                if let Some(pane_id) = action["paneId"].as_u64() {
                    let mux = mux::Mux::get();
                    mux.remove_pane(pane_id as PaneId);
                }
            }
            "activate_pane" => {
                if let Some(tab_idx) = action["tabIdx"].as_u64() {
                    self.activate_tab(tab_idx as isize).ok();
                    // Activate the specific pane within the tab
                    if let Some(pane_idx) = action["paneIdx"].as_u64() {
                        let mux = mux::Mux::get();
                        if let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id)
                        {
                            let panes = tab.iter_panes_ignoring_zoom();
                            if let Some(pp) = panes.get(pane_idx as usize) {
                                tab.set_active_idx(pp.index);
                            }
                        }
                    }
                }
            }
            "new_tab" => {
                use config::keyassignment::SpawnTabDomain;
                self.spawn_tab(&SpawnTabDomain::CurrentPaneDomain);
            }
            "toggle_mute" => {
                if let Some(pane_id) = action["paneId"].as_u64() {
                    let pane_id = pane_id as PaneId;
                    let muted = {
                        let mut ps = self.pane_state(pane_id);
                        ps.notifications_muted = !ps.notifications_muted;
                        if ps.notifications_muted {
                            ps.notification_start = None;
                            ps.notification_count = 0;
                        }
                        ps.notifications_muted
                    };
                    {
                        let mut set = MUTED_PANES.lock().unwrap();
                        if muted {
                            set.insert(pane_id);
                        } else {
                            set.remove(&pane_id);
                        }
                    }
                    log::info!("Pane {} notifications muted: {} (via sidebar)", pane_id, muted);
                    self.invalidate_tab_sidebar();
                    if let Some(w) = self.window.as_ref() {
                        w.invalidate();
                    }
                }
            }
            "reset_zoom" => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    let pane_id = pane.pane_id();
                    let had_scale = {
                        let mut ps = self.pane_state(pane_id);
                        let had = (ps.font_scale - 1.0).abs() > 0.001;
                        ps.font_scale = 1.0;
                        had
                    };
                    if had_scale {
                        self.resize_pane_for_font_scale(pane_id);
                        self.shape_generation += 1;
                        self.shape_cache.borrow_mut().clear();
                    }
                    self.invalidate_tab_sidebar();
                    if let Some(w) = self.window.as_ref() {
                        w.invalidate();
                    }
                }
                log::info!("Zoom reset to 100% (via sidebar)");
            }
            "refocus" | _ if action_type.is_empty() => {}
            _ => {
                log::trace!("Unknown sidebar IPC action: {}", msg);
            }
        }

        // Always refocus the parent window after any sidebar interaction
        // to prevent WebView2 from keeping keyboard focus
        if let Some(ref wv) = self.webview_sidebar {
            wv.refocus_parent();
        }
    }

    /// Serialize current sidebar state as JSON for the WebView.
    #[cfg(windows)]
    pub fn serialize_sidebar_state(&mut self) -> String {
        let mux = mux::Mux::get();
        let mux_window = match mux.get_window(self.mux_window_id) {
            Some(w) => w,
            None => return "{}".to_string(),
        };
        let active_tab_id = mux
            .get_active_tab_for_window(self.mux_window_id)
            .map(|t| t.tab_id());

        let tabs: Vec<serde_json::Value> = mux_window
            .iter()
            .enumerate()
            .map(|(tab_idx, tab)| {
                let tab_id = tab.tab_id();
                let is_active = active_tab_id == Some(tab_id);
                let title = tab.get_title();
                let info = self.tab_sidebar_info.get(&tab_id);
                let panes = tab.iter_panes_ignoring_zoom();
                let has_multiple_panes = panes.len() > 1;

                let has_notification = self
                    .pane_state_for_tab(tab_id)
                    .map_or(false, |ps| ps.notification_start.is_some());
                let notif_count = self
                    .pane_state_for_tab(tab_id)
                    .map_or(0u32, |ps| ps.notification_count);

                // Single-pane Claude info at tab level
                let single_claude = if !has_multiple_panes {
                    info.and_then(|i| {
                        if i.pane_claude_info.len() == 1 {
                            i.pane_claude_info.values().next()
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };

                let claude_json = single_claude.map(|c| claude_to_json(c));

                let pane_entries: Vec<serde_json::Value> = if has_multiple_panes {
                    panes
                        .iter()
                        .map(|pp| {
                            let pane = &pp.pane;
                            let pane_id = pane.pane_id();
                            let pane_title = pane.get_title();
                            let pane_cwd = pane
                                .get_current_working_dir(mux::pane::CachePolicy::AllowStale)
                                .and_then(|u| {
                                    if u.scheme() == "file" {
                                        Some(u.path().to_string())
                                    } else {
                                        None
                                    }
                                });
                            let pane_claude =
                                info.and_then(|i| i.pane_claude_info.get(&pane_id));
                            let pane_has_notif = {
                                let states = self.pane_state.borrow();
                                states.get(&pane_id).map_or(false, |ps| ps.notification_start.is_some())
                            };

                            serde_json::json!({
                                "paneId": pane_id,
                                "paneIdx": pp.index,
                                "title": pane_title,
                                "cwdShort": pane_cwd,
                                "isActive": pp.is_active,
                                "hasNotification": pane_has_notif,
                                "claudeInfo": pane_claude.map(|c| claude_to_json(c)),
                            })
                        })
                        .collect()
                } else {
                    vec![]
                };

                // Per-pane muted state (active pane)
                let active_pane_muted = panes
                    .iter()
                    .find(|p| p.is_active)
                    .map(|p| self.pane_state(p.pane.pane_id()).notifications_muted)
                    .unwrap_or(false);
                let active_pane_id = panes
                    .iter()
                    .find(|p| p.is_active)
                    .map(|p| p.pane.pane_id());

                // Zoom percentage for active tab
                let zoom_pct = if is_active {
                    panes
                        .iter()
                        .find(|p| p.is_active)
                        .map(|p| {
                            let pane_scale = self.pane_state(p.pane.pane_id()).font_scale;
                            let global_scale = self.fonts.get_font_scale();
                            let pct = (pane_scale * global_scale * 100.0).round() as u16;
                            pct
                        })
                        .filter(|&pct| pct != 100)
                } else {
                    None
                };

                serde_json::json!({
                    "tabIdx": tab_idx,
                    "tabId": tab_id,
                    "title": title,
                    "cwdShort": info.map(|i| i.cwd_short.as_str()).unwrap_or(""),
                    "gitBranch": info.and_then(|i| i.git_branch.as_deref()),
                    "isActive": is_active,
                    "hasNotification": has_notification,
                    "notificationCount": notif_count,
                    "notificationsMuted": active_pane_muted,
                    "activePaneId": active_pane_id,
                    "zoomPct": zoom_pct,
                    "panes": pane_entries,
                    "claudeInfo": claude_json,
                })
            })
            .collect();

        // Aggregate Claude stats across all tabs/panes
        let claude_stats = {
            let mut active: u32 = 0;
            let mut working: u32 = 0;
            let mut waiting: u32 = 0;
            let mut idle: u32 = 0;
            let mut errored: u32 = 0;
            let mut session_cost: f32 = 0.0;
            let mut session_dur: u64 = 0;
            let mut total_added: u32 = 0;
            let mut total_removed: u32 = 0;
            let mut ctx_sum: u32 = 0;
            let mut ctx_count: u32 = 0;
            let mut models: Vec<String> = Vec::new();
            let mut cost_pairs: Vec<(mux::pane::PaneId, f32)> = Vec::new();

            for info in self.tab_sidebar_info.values() {
                for (pane_id, c) in &info.pane_claude_info {
                    if let Some(st) = c.status {
                        active += 1;
                        match st {
                            ClaudeStatus::Working => working += 1,
                            ClaudeStatus::WaitingInput => waiting += 1,
                            ClaudeStatus::Idle => idle += 1,
                            ClaudeStatus::Error => errored += 1,
                        }
                        session_cost += c.cost_usd.unwrap_or(0.0);
                        session_dur += c.duration_ms.unwrap_or(0);
                        total_added += c.lines_added.unwrap_or(0);
                        total_removed += c.lines_removed.unwrap_or(0);
                        if let Some(pct) = c.context_pct {
                            ctx_sum += pct as u32;
                            ctx_count += 1;
                        }
                        if let Some(ref m) = c.model {
                            if !models.contains(m) {
                                models.push(m.clone());
                            }
                        }
                        if let Some(cost) = c.cost_usd {
                            cost_pairs.push((*pane_id, cost));
                        }
                    }
                }
            }

            // Update cumulative usage tracker
            #[cfg(windows)]
            self.claude_usage_tracker.update_costs(&cost_pairs);

            let has_data = active > 0;
            #[cfg(windows)]
            let has_data = has_data || self.claude_usage_tracker.daily_cost > 0.0;

            if has_data {
                #[cfg(windows)]
                let daily_budget = self.config.claude_daily_budget_usd;
                #[cfg(windows)]
                let weekly_budget = self.config.claude_weekly_budget_usd;
                #[cfg(not(windows))]
                let daily_budget: Option<f32> = None;
                #[cfg(not(windows))]
                let weekly_budget: Option<f32> = None;

                #[cfg(windows)]
                let (daily_spent, weekly_spent) = (
                    self.claude_usage_tracker.daily_cost,
                    self.claude_usage_tracker.weekly_cost,
                );
                #[cfg(not(windows))]
                let (daily_spent, weekly_spent): (f32, f32) = (0.0, 0.0);

                Some(serde_json::json!({
                    "activeSessions": active,
                    "working": working,
                    "waitingInput": waiting,
                    "idle": idle,
                    "error": errored,
                    "totalCostUsd": session_cost,
                    "totalDurationMs": session_dur,
                    "totalLinesAdded": total_added,
                    "totalLinesRemoved": total_removed,
                    "avgContextPct": if ctx_count > 0 { Some(ctx_sum / ctx_count) } else { None::<u32> },
                    "models": models,
                    "dailySpent": daily_spent,
                    "dailyBudget": daily_budget,
                    "weeklySpent": weekly_spent,
                    "weeklyBudget": weekly_budget,
                    "dailyRemaining": daily_budget.map(|b| (b - daily_spent).max(0.0)),
                    "weeklyRemaining": weekly_budget.map(|b| (b - weekly_spent).max(0.0)),
                }))
            } else {
                None
            }
        };

        let position = match self.config.tab_sidebar_position {
            config::TabSidebarPosition::Left => "left",
            config::TabSidebarPosition::Right => "right",
        };

        serde_json::json!({
            "tabs": tabs,
            "sidebarPosition": position,
            "claudeStats": claude_stats,
        })
        .to_string()
    }

    fn do_open_link_at_mouse_cursor(&self, pane: &Arc<dyn Pane>) {
        // They clicked on a link, so let's open it!
        // We need to ensure that we spawn the `open` call outside of the context
        // of our window loop; on Windows it can cause a panic due to
        // triggering our WndProc recursively.
        // We get that assurance for free as part of the async dispatch that we
        // perform below; here we allow the user to define an `open-uri` event
        // handler that can bypass the normal `open_url` functionality.
        if let Some(link) = self.current_highlight.as_ref().cloned() {
            // open-uri Lua event removed; always use default OS open behavior
            let uri = link.uri().to_string();
            promise::spawn::spawn(async move {
                log::info!("clicking {}", uri);
                // STRIPPED: terminaler_open_url removed; open URL via OS shell command
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &uri])
                        .spawn();
                }
                anyhow::Result::<()>::Ok(())
            })
            .detach();
        }
    }
    fn close_current_pane(&mut self, confirm: bool) {
        let mux_window_id = self.mux_window_id;
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let pane = match tab.get_active_pane() {
            Some(p) => p,
            None => return,
        };

        let pane_id = pane.pane_id();
        if confirm && !pane.can_close_without_prompting(CloseReason::Pane) {
            let window = self.window.clone().unwrap();
            let (overlay, future) = start_overlay_pane(self, &pane, move |pane_id, term| {
                confirm_close_pane(pane_id, term, mux_window_id, window)
            });
            self.assign_overlay_for_pane(pane_id, overlay);
            promise::spawn::spawn(future).detach();
        } else {
            mux.remove_pane(pane_id);
        }
    }

    fn close_pane_by_id(&mut self, pane_id: mux::pane::PaneId, confirm: bool) {
        let mux = Mux::get();
        let pane = match mux.get_pane(pane_id) {
            Some(p) => p,
            None => return,
        };
        if confirm && !pane.can_close_without_prompting(CloseReason::Pane) {
            let mux_window_id = self.mux_window_id;
            let window = self.window.clone().unwrap();
            let (overlay, future) = start_overlay_pane(self, &pane, move |pane_id, term| {
                confirm_close_pane(pane_id, term, mux_window_id, window)
            });
            self.assign_overlay_for_pane(pane_id, overlay);
            promise::spawn::spawn(future).detach();
        } else {
            mux.remove_pane(pane_id);
        }
    }

    fn close_specific_tab(&mut self, tab_idx: usize, confirm: bool) {
        let mux = Mux::get();
        let mux_window_id = self.mux_window_id;
        let mux_window = match mux.get_window(mux_window_id) {
            Some(w) => w,
            None => return,
        };

        let tab = match mux_window.get_by_idx(tab_idx) {
            Some(tab) => Arc::clone(tab),
            None => return,
        };
        drop(mux_window);

        let tab_id = tab.tab_id();
        if confirm && !tab.can_close_without_prompting(CloseReason::Tab) {
            if self.activate_tab(tab_idx as isize).is_err() {
                return;
            }

            let window = self.window.clone().unwrap();
            let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                confirm_close_tab(tab_id, term, mux_window_id, window)
            });
            self.assign_overlay(tab_id, overlay);
            promise::spawn::spawn(future).detach();
        } else {
            mux.remove_tab(tab_id);
        }
    }

    fn close_current_tab(&mut self, confirm: bool) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let tab_id = tab.tab_id();
        let mux_window_id = self.mux_window_id;
        if confirm && !tab.can_close_without_prompting(CloseReason::Tab) {
            let window = self.window.clone().unwrap();
            let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                confirm_close_tab(tab_id, term, mux_window_id, window)
            });
            self.assign_overlay(tab_id, overlay);
            promise::spawn::spawn(future).detach();
        } else {
            mux.remove_tab(tab_id);
        }
    }

    pub fn pane_state(&self, pane_id: PaneId) -> RefMut<'_, PaneState> {
        RefMut::map(self.pane_state.borrow_mut(), |state| {
            state.entry(pane_id).or_insert_with(PaneState::default)
        })
    }

    fn font_scale_key(scale: f64) -> u32 {
        (scale * 100.0).round() as u32
    }

    pub fn get_or_create_scaled_config(
        &mut self,
        scale: f64,
    ) -> (Rc<FontConfiguration>, RenderMetrics) {
        let key = Self::font_scale_key(scale);
        if let Some(entry) = self.scaled_font_configs.get(&key) {
            return entry.clone();
        }
        let dpi = self.dimensions.dpi;
        let fonts = Rc::new(
            FontConfiguration::new(Some(self.config.clone()), dpi)
                .expect("FontConfiguration::new for scaled config"),
        );
        fonts.change_scaling(scale, dpi);
        let metrics =
            RenderMetrics::new(&fonts).expect("RenderMetrics::new for scaled config");
        self.scaled_font_configs
            .insert(key, (Rc::clone(&fonts), metrics));
        (fonts, metrics)
    }

    pub fn pane_font_scale(&self, pane_id: PaneId) -> f64 {
        self.pane_state
            .borrow()
            .get(&pane_id)
            .map(|s| s.font_scale)
            .unwrap_or(1.0)
    }

    pub fn tab_state(&self, tab_id: TabId) -> RefMut<'_, TabState> {
        RefMut::map(self.tab_state.borrow_mut(), |state| {
            state.entry(tab_id).or_insert_with(TabState::default)
        })
    }

    /// Resize overlays to match their corresponding tab/pane dimensions
    pub fn resize_overlays(&self) {
        let mux = Mux::get();
        for (_, state) in self.tab_state.borrow().iter() {
            if let Some(overlay) = state.overlay.as_ref().map(|o| &o.pane) {
                overlay.resize(self.terminal_size).ok();
            }
        }
        for (pane_id, state) in self.pane_state.borrow().iter() {
            if let Some(overlay) = state.overlay.as_ref().map(|o| &o.pane) {
                if let Some(pane) = mux.get_pane(*pane_id) {
                    let dims = pane.get_dimensions();
                    overlay
                        .resize(TerminalSize {
                            cols: dims.cols,
                            rows: dims.viewport_rows,
                            dpi: self.terminal_size.dpi,
                            pixel_height: (self.terminal_size.pixel_height
                                / self.terminal_size.rows)
                                * dims.viewport_rows,
                            pixel_width: (self.terminal_size.pixel_width / self.terminal_size.cols)
                                * dims.cols,
                        })
                        .ok();
                }
            }
        }
    }

    pub fn get_viewport(&self, pane_id: PaneId) -> Option<StableRowIndex> {
        self.pane_state(pane_id).viewport
    }

    pub fn set_viewport(
        &mut self,
        pane_id: PaneId,
        position: Option<StableRowIndex>,
        dims: RenderableDimensions,
    ) {
        let pos = match position {
            Some(pos) => {
                // Drop out of scrolling mode if we're off the bottom
                if pos >= dims.physical_top {
                    None
                } else {
                    Some(pos.max(dims.scrollback_top))
                }
            }
            None => None,
        };

        let mut state = self.pane_state(pane_id);
        if pos != state.viewport {
            state.viewport = pos;

            // This is a bit gross.  If we add other overlays that need this information,
            // this should get extracted out into a trait
            if let Some(overlay) = state.overlay.as_ref() {
                if let Some(copy) = overlay.pane.downcast_ref::<CopyOverlay>() {
                    copy.viewport_changed(pos);
                } else if let Some(qs) = overlay.pane.downcast_ref::<QuickSelectOverlay>() {
                    qs.viewport_changed(pos);
                }
            }
        }
        self.window.as_ref().unwrap().invalidate();
    }

    fn maybe_scroll_to_bottom_for_input(&mut self, pane: &Arc<dyn Pane>) {
        if self.config.scroll_to_bottom_on_input {
            self.scroll_to_bottom(pane);
        }
    }

    fn scroll_to_top(&mut self, pane: &Arc<dyn Pane>) {
        let dims = pane.get_dimensions();
        self.set_viewport(pane.pane_id(), Some(dims.scrollback_top), dims);
    }

    fn scroll_to_bottom(&mut self, pane: &Arc<dyn Pane>) {
        self.pane_state(pane.pane_id()).viewport = None;
    }

    fn get_active_pane_no_overlay(&self) -> Option<Arc<dyn Pane>> {
        let mux = Mux::get();
        mux.get_active_tab_for_window(self.mux_window_id)
            .and_then(|tab| tab.get_active_pane())
    }

    /// Returns a Pane that we can interact with; this will typically be
    /// the active tab for the window, but if the window has a tab-wide
    /// overlay (such as the launcher / tab navigator),
    /// then that will be returned instead.  Otherwise, if the pane has
    /// an active overlay (such as search or copy mode) then that will
    /// be returned.
    pub fn get_active_pane_or_overlay(&self) -> Option<Arc<dyn Pane>> {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return None,
        };

        let tab_id = tab.tab_id();

        if let Some(tab_overlay) = self
            .tab_state(tab_id)
            .overlay
            .as_ref()
            .map(|overlay| overlay.pane.clone())
        {
            Some(tab_overlay)
        } else {
            let pane = tab.get_active_pane()?;
            let pane_id = pane.pane_id();
            self.pane_state(pane_id)
                .overlay
                .as_ref()
                .map(|overlay| overlay.pane.clone())
                .or_else(|| Some(pane))
        }
    }

    fn get_splits(&mut self) -> Vec<PositionedSplit> {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return vec![],
        };

        let tab_id = tab.tab_id();

        if self.tab_state(tab_id).overlay.is_some() {
            vec![]
        } else {
            tab.iter_splits()
        }
    }

    fn pos_pane_to_pane_info(pos: &PositionedPane) -> PaneInformation {
        PaneInformation {
            pane_id: pos.pane.pane_id(),
            pane_index: pos.index,
            is_active: pos.is_active,
            is_zoomed: pos.is_zoomed,
            has_unseen_output: pos.pane.has_unseen_output(),
            left: pos.left,
            top: pos.top,
            width: pos.width,
            height: pos.height,
            pixel_width: pos.pixel_width,
            pixel_height: pos.pixel_height,
            title: pos.pane.get_title(),
            user_vars: pos.pane.copy_user_vars(),
            progress: pos.pane.get_progress(),
        }
    }

    fn get_tab_information(&mut self) -> Vec<TabInformation> {
        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(window) => window,
            _ => return vec![],
        };
        let tab_index = window.get_active_idx();

        window
            .iter()
            .enumerate()
            .map(|(idx, tab)| {
                let panes = self.get_pos_panes_for_tab(tab);

                TabInformation {
                    tab_index: idx,
                    tab_id: tab.tab_id(),
                    is_active: tab_index == idx,
                    is_last_active: window
                        .get_last_active_idx()
                        .map(|last_active| last_active == idx)
                        .unwrap_or(false),
                    window_id: self.mux_window_id,
                    tab_title: tab.get_title(),
                    active_pane: panes
                        .iter()
                        .find(|p| p.is_active)
                        .map(Self::pos_pane_to_pane_info),
                    has_notification: {
                        let is_inactive = tab_index != idx;
                        is_inactive
                            && panes.iter().any(|p| {
                                self.pane_state(p.pane.pane_id())
                                    .notification_start
                                    .is_some()
                            })
                    },
                    has_muted_pane: panes
                        .iter()
                        .any(|p| self.pane_state(p.pane.pane_id()).notifications_muted),
                    zoom_pct: if tab_index == idx {
                        panes
                            .iter()
                            .find(|p| p.is_active)
                            .and_then(|p| {
                                let pane_scale =
                                    self.pane_state(p.pane.pane_id()).font_scale;
                                let global_scale = self.fonts.get_font_scale();
                                let pct = (pane_scale * global_scale * 100.0).round() as u16;
                                if pct != 100 {
                                    Some(pct)
                                } else {
                                    None
                                }
                            })
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    fn get_pane_information(&self) -> Vec<PaneInformation> {
        self.get_panes_to_render()
            .iter()
            .map(Self::pos_pane_to_pane_info)
            .collect()
    }

    fn get_pos_panes_for_tab(&self, tab: &Arc<Tab>) -> Vec<PositionedPane> {
        let tab_id = tab.tab_id();

        if let Some(pane) = self
            .tab_state(tab_id)
            .overlay
            .as_ref()
            .map(|overlay| overlay.pane.clone())
        {
            let size = tab.get_size();
            vec![PositionedPane {
                index: 0,
                is_active: true,
                is_zoomed: false,
                left: 0,
                top: 0,
                width: size.cols as _,
                height: size.rows as _,
                pixel_width: size.cols as usize * self.render_metrics.cell_size.width as usize,
                pixel_height: size.rows as usize * self.render_metrics.cell_size.height as usize,
                pane,
            }]
        } else {
            let mut panes = tab.iter_panes();
            for p in &mut panes {
                if let Some(overlay) = self.pane_state(p.pane.pane_id()).overlay.as_ref() {
                    p.pane = Arc::clone(&overlay.pane);
                }
            }
            panes
        }
    }

    fn get_panes_to_render(&self) -> Vec<PositionedPane> {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return vec![],
        };

        self.get_pos_panes_for_tab(&tab)
    }

    /// if pane_id.is_none(), removes any overlay for the specified tab.
    /// Otherwise: if the overlay is the specified pane for that tab, remove it.
    fn cancel_overlay_for_tab(&mut self, tab_id: TabId, pane_id: Option<PaneId>) {
        if pane_id.is_some() {
            let current = self
                .tab_state(tab_id)
                .overlay
                .as_ref()
                .map(|o| o.pane.pane_id());
            if current != pane_id {
                return;
            }
        }
        if let Some(overlay) = self.tab_state(tab_id).overlay.take() {
            Mux::get().remove_pane(overlay.pane.pane_id());
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    pub fn schedule_cancel_overlay(window: Window, tab_id: TabId, pane_id: Option<PaneId>) {
        window.notify(TermWindowNotif::CancelOverlayForTab { tab_id, pane_id });
    }

    fn cancel_overlay_for_pane(&mut self, pane_id: PaneId) {
        if let Some(overlay) = self.pane_state(pane_id).overlay.take() {
            // Ungh, when I built the CopyOverlay, its pane doesn't get
            // added to the mux and instead it reports the overlaid
            // pane id.  Take care to avoid killing ourselves off
            // when closing the CopyOverlay
            if pane_id != overlay.pane.pane_id() {
                Mux::get().remove_pane(overlay.pane.pane_id());
            }
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    pub fn schedule_cancel_overlay_for_pane(window: Window, pane_id: PaneId) {
        window.notify(TermWindowNotif::CancelOverlayForPane(pane_id));
    }

    pub fn assign_overlay_for_pane(&mut self, pane_id: PaneId, pane: Arc<dyn Pane>) {
        self.cancel_overlay_for_pane(pane_id);
        self.pane_state(pane_id).overlay.replace(OverlayState {
            pane,
            key_table_state: KeyTableState::default(),
        });
        self.update_title();
    }

    pub fn assign_overlay(&mut self, tab_id: TabId, overlay: Arc<dyn Pane>) {
        self.cancel_overlay_for_tab(tab_id, None);
        self.tab_state(tab_id).overlay.replace(OverlayState {
            pane: overlay,
            key_table_state: KeyTableState::default(),
        });
        self.update_title();
    }

    fn resolve_search_pattern(&self, pattern: Pattern, pane: &Arc<dyn Pane>) -> MuxPattern {
        match pattern {
            Pattern::CaseSensitiveString(s) => MuxPattern::CaseSensitiveString(s),
            Pattern::CaseInSensitiveString(s) => MuxPattern::CaseInSensitiveString(s),
            Pattern::Regex(s) => MuxPattern::Regex(s),
            Pattern::CurrentSelectionOrEmptyString => {
                let text = self.selection_text(pane);
                let first_line = text
                    .lines()
                    .next()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                MuxPattern::CaseSensitiveString(first_line)
            }
        }
    }
}

impl Drop for TermWindow {
    fn drop(&mut self) {
        self.clear_all_overlays();
        if let Some(window) = self.window.take() {
            if let Some(fe) = try_front_end() {
                fe.forget_known_window(&window);
            }
        }
    }
}
