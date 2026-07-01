# Rift Architecture

This document describes the main runtime architecture and the data structures
that carry window, workspace, and layout state. It is intended for maintainers
who need to change behavior without accidentally moving state across ownership
boundaries.

## High-Level Shape

Rift is an actor-driven macOS window manager. The binary starts OS integration,
spawns actors, and then routes all meaningful state changes through a central
reactor and a layout engine.

Main modules:

- `src/bin/rift.rs`: process bootstrap. Initializes AppKit, accessibility,
  Skylight/Mach services, configuration, actors, the reactor, and the IPC
  server.
- `src/actor`: asynchronous actors and actor-facing event types.
- `src/actor/reactor.rs`: central event reducer. Keeps the model coherent and
  decides when layouts, focus, raises, UI updates, and broadcasts are needed.
- `src/layout_engine`: workspace and tiling state machine. It answers layout
  commands and calculates target window frames.
- `src/model`: state containers and domain models shared by actors and layout
  code.
- `src/sys`: macOS API wrappers around Accessibility, AppKit/CoreGraphics,
  Skylight, Carbon hotkeys, Mach IPC, screen/space discovery, timers, and run
  loops.
- `src/ipc`: Mach server/client protocol used by `rift-cli` and subscribers.
- `src/ui`: menu bar, stack line, and mission-control UI surfaces.

## Runtime Topology

The process has several long-lived execution contexts:

- Main thread: AppKit setup plus main-run-loop actors such as menu, stack line,
  gesture handling, mission control, notification center, process actor, window
  notifications, and `WmController`.
- Input thread: `EventTap` runs on its own CFRunLoop so keyboard/mouse event
  processing is not blocked by layout, animation, or SLS calls.
- Reactor thread: `Reactor::spawn` starts a dedicated thread that consumes
  `actor::Receiver<reactor::Event>`.
- Mach server thread: `ipc::run_mach_server` accepts JSON requests over the
  registered Mach service.
- Per-application threads: `actor::app::spawn_app_thread` watches an app through
  Accessibility notifications and executes per-window AX requests.

Actors communicate with `actor::Sender<T>` / `actor::Receiver<T>`, a thin
wrapper around Tokio unbounded channels that also preserves the current tracing
span.

## Core Event Flow

### Startup

1. `src/bin/rift.rs` reads config, verifies accessibility permission, initializes
   the Mach/SLS pieces, and requires "Displays have separate Spaces".
2. It creates `LayoutEngine`, then spawns `Reactor`.
3. It starts config watching, window notifications, IPC, `WmController`,
   `EventTap`, `GestureTap`, menu, stack line, mission control, notification
   center, and process monitoring.
4. `WmController` discovers running apps and starts one app actor per eligible
   application.

### OS Events

Application, window, display, space, and input events are normalized into
`reactor::Event`.

Typical paths:

- AppKit / process changes -> `WmController` -> `reactor::Event`.
- AX app notifications -> app actor -> `reactor::Event`.
- SLS window/space notifications -> `WindowNotify` -> `reactor::Event`.
- Hotkeys and pointer gestures -> `EventTap` / `GestureTap` -> `WmController` or
  `Reactor`.
- IPC requests -> Mach handler -> `ReactorHandle` or `ConfigActor`.

The reactor is the coherence point. It updates the live model, emits
`LayoutEvent` into the layout engine, runs layout when necessary, then sends
side effects to app actors, UI actors, the raise manager, and broadcast
subscribers.

### Commands

User commands usually flow through these layers:

1. `WmCommand` in `actor::wm_controller` is the hotkey-facing command surface.
2. `reactor::Command` wraps either a `LayoutCommand`, `ReactorCommand`, or
   metrics command.
3. `LayoutCommand` in `layout_engine::engine` mutates workspace/layout state.
4. `EventResponse` asks the reactor to focus or raise windows after the layout
   command completes.

IPC commands use the same `reactor::Command` shape via `ipc::protocol::RiftCommand`.

## State Ownership Boundaries

The most important rule is that different layers own different pieces of truth:

- `Reactor` owns live system coherence: running apps, known windows, active
  spaces/screens, drag state, focus/refocus state, menu/mission-control state,
  pending transactions, and communication channels.
- `WindowRegistry` owns the mapping between Rift window identities, WindowServer
  identities, live `WindowState`, and workspace assignment metadata.
- `LayoutEngine` owns layout state: active layout trees, floating state,
  window constraints, virtual workspace manager state, and layout broadcasts.
- `VirtualWorkspaceManager` owns virtual workspace membership, global workspace
  numbers, display bindings, active/last workspace per display, app-rule
  assignment results, and floating positions saved per workspace.
- `sys` wrappers own unsafe or platform-specific calls. Higher-level code should
  prefer `sys` APIs instead of calling macOS APIs directly.

When adding behavior, put state where the owner can maintain invariants. For
example, focus heuristics belong in the reactor, but moving a window between
workspace membership sets belongs in the layout engine / virtual workspace
manager.

## Key Data Types

The code blocks below are structural summaries. They intentionally omit serde
attributes and some trait bounds, but keep the field names and ownership shape
that matter when changing behavior.

### Window And System Identity

`WindowId` is Rift's app-scoped window identity. Always compare the full
`(pid, idx)` pair; `idx` is only unique inside one process.

```rust
// src/actor/app.rs
pub struct WindowId {
    pub pid: pid_t,
    pub idx: NonZeroU32,
}
```

`WindowServerId` is the WindowServer/SLS id. Use `WindowRegistry` to map it back
to a `WindowId`.

```rust
// src/sys/window_server.rs
pub struct WindowServerId(pub CGWindowID);
```

Runtime OS snapshots:

```rust
// src/sys/screen.rs
pub struct ScreenInfo {
    pub id: ScreenId,
    pub frame: CGRect,
    pub display_uuid: String,
    pub name: Option<String>,
    pub space: Option<SpaceId>,
}

// src/sys/app.rs
pub struct AppInfo {
    pub bundle_id: Option<String>,
    pub localized_name: Option<String>,
}

pub struct WindowInfo {
    pub is_standard: bool,
    pub is_root: bool,
    pub is_minimized: bool,
    pub is_resizable: bool,
    pub title: String,
    pub frame: CGRect,
    pub min_size: Option<CGSize>,
    pub max_size: Option<CGSize>,
    pub sys_id: Option<WindowServerId>,
    pub bundle_id: Option<String>,
    pub path: Option<PathBuf>,
    pub ax_role: Option<String>,
    pub ax_subrole: Option<String>,
}

// src/sys/window_server.rs
pub struct WindowServerInfo {
    pub id: WindowServerId,
    pub pid: pid_t,
    pub layer: i32,
    pub frame: CGRect,
    pub min_frame: CGSize,
    pub max_frame: CGSize,
}
```

### Window Registry And Reactor State

`WindowRegistry` is the bridge between Rift windows, WindowServer windows, live
AX state, and workspace assignment metadata.

```rust
// src/model/window_registry.rs
pub struct WindowRegistry {
    windows: HashMap<WindowId, WindowRecord>,
    window_servers: HashMap<WindowServerId, WindowServerRecord>,
}

struct WindowRecord {
    state: Option<WindowState>,
    workspace: Option<WindowWorkspaceInfo>,
    rule_floating: bool,
    last_rule_decision: bool,
}

struct WindowServerRecord {
    window_id: Option<WindowId>,
    visible: bool,
    observed: bool,
    info: Option<WindowServerInfo>,
    recent_at: Option<Instant>,
}

pub struct WindowWorkspaceInfo {
    pub space: SpaceId,
    pub workspace_id: VirtualWorkspaceId,
}
```

`WindowState` is the reactor's live window model. `frame_monotonic` is the frame
cache used to avoid accepting stale AX reads after Rift has written a frame.

```rust
// src/model/reactor.rs
pub struct WindowState {
    pub(crate) info: WindowInfo,
    pub(crate) frame_monotonic: CGRect,
    pub(crate) is_manageable: bool,
    pub(crate) ignore_app_rule: bool,
}
```

`Reactor` is the central reducer. Most behavior is split into handlers under
`src/actor/reactor/events/`, but these fields show the main state partitions.

```rust
// src/actor/reactor.rs
pub struct Reactor {
    pub config: Config,
    pub one_space: bool,

    app_manager: AppManager,
    layout_manager: LayoutManager,
    window_manager: WindowManager,
    space_manager: SpaceManager,
    space_activation_policy: SpaceActivationPolicy,
    main_window_tracker: MainWindowTracker,
    drag_manager: DragManager,
    workspace_switch_manager: WorkspaceSwitchManager,
    recording_manager: RecordingManager,
    communication_manager: CommunicationManager,
    notification_manager: NotificationManager,
    transaction_manager: TransactionManager,
    menu_manager: MenuManager,
    mission_control_manager: MissionControlManager,
    refocus_manager: RefocusManager,
    pending_space_change_manager: PendingSpaceChangeManager,

    active_spaces: HashSet<SpaceId>,
    display_topology_manager: DisplayTopologyManager,
    pub above_window: Option<WindowServerId>,
    pub animation_tx: Option<AnimationSender>,
}
```

Important manager shapes:

```rust
// src/actor/reactor/managers.rs
pub type WindowManager = Box<WindowRegistry>;

pub struct AppManager {
    pub apps: HashMap<pid_t, AppState>,
}

pub struct SpaceManager {
    pub screens: Vec<ScreenInfo>,
    pub fullscreen_by_space: HashMap<u64, FullscreenSpaceTrack>,
    pub has_seen_display_set: bool,
}

pub struct LayoutManager {
    pub layout_engine: LayoutEngine,
}

pub struct RefocusManager {
    pub stale_cleanup_state: StaleCleanupState,
    pub refocus_state: RefocusState,
    pub focus_next_window_deadline: Option<Instant>,
    pub focus_next_window_target: Option<FocusNextWindowTarget>,
    pub recent_workspace_targets: HashMap<WindowId, RecentWorkspaceTarget>,
}

pub struct CommunicationManager {
    pub event_tap_tx: Option<event_tap::Sender>,
    pub gesture_tap_tx: Option<gesture_tap::Sender>,
    pub stack_line_tx: Option<stack_line::Sender>,
    pub raise_manager_tx: raise_manager::Sender,
    pub event_broadcaster: BroadcastSender,
    pub wm_sender: Option<wm_controller::Sender>,
    pub events_tx: Option<actor::Sender<Event>>,
}
```

### Commands And Event Contracts

Hotkey-facing commands start in `WmCommand`, then become `reactor::Command`.

```rust
// src/actor/wm_controller.rs
pub enum WmCommand {
    Wm(WmCmd),
    ReactorCommand(reactor::Command),
}

pub enum WmCmd {
    ToggleSpaceActivated,
    Exec(ExecCmd),
    NextWorkspace,
    PrevWorkspace,
    SwitchToWorkspace(WorkspaceSelector),
    MoveWindowToWorkspace(WorkspaceSelector),
    CreateWorkspace,
    SwitchToLastWorkspace,
    ShowMissionControlAll,
    ShowMissionControlCurrent,
    DismissMissionControl,
}

// src/model/reactor.rs
pub enum Command {
    Layout(LayoutCommand),
    Metrics(MetricsCommand),
    Reactor(ReactorCommand),
}
```

`ReactorCommand` covers non-layout actions that still need reactor context.

```rust
// src/model/reactor.rs
pub enum ReactorCommand {
    Debug,
    Serialize,
    SaveAndExit,
    SwitchSpace(Direction),
    ToggleSpaceActivated,
    FocusWindow {
        window_id: WindowId,
        window_server_id: Option<WindowServerId>,
    },
    FocusNextWindow,
    CancelFocusNextWindow,
    ShowMissionControlAll,
    ShowMissionControlCurrent,
    DismissMissionControl,
    MoveMouseToDisplay(DisplaySelector),
    FocusDisplay(DisplaySelector),
    CloseWindow {
        window_server_id: Option<WindowServerId>,
    },
    MoveWindowToDisplay {
        selector: DisplaySelector,
        window_id: Option<u32>,
    },
}
```

`LayoutCommand` mutates layout/workspace state. Current-window commands should
resolve to an exact `WindowId` before entering mutation code when possible.

```rust
// src/layout_engine/engine.rs
pub enum LayoutCommand {
    NextWindow,
    PrevWindow,
    MoveFocus(Direction),
    Ascend,
    Descend,
    MoveNode(Direction),

    JoinWindow(Direction),
    ToggleStack,
    ToggleOrientation,
    UnjoinWindows,
    ToggleFocusFloating,
    ToggleWindowFloating,
    ToggleFullscreen,
    ToggleFullscreenWithinGaps,

    ResizeWindowGrow,
    ResizeWindowShrink,
    ResizeWindowBy { amount: f64 },
    ScrollStrip { delta: f64 },
    SnapStrip,
    CenterSelection,

    NextWorkspace(Option<bool>),
    PrevWorkspace(Option<bool>),
    SwitchToWorkspace(usize),
    SwitchToGlobalSlot(usize),
    MoveWindowToWorkspace {
        workspace: usize,
        window_id: Option<u32>,
    },
    SetWorkspaceLayout {
        workspace: Option<usize>,
        mode: LayoutMode,
    },
    CreateWorkspace,
    SwitchToLastWorkspace,

    SwapWindows(WindowId, WindowId),
    AdjustMasterRatio { delta: f64 },
    AdjustMasterCount { delta: i32 },
    PromoteToMaster,
    SwapMasterStack,
}
```

The layout engine receives `LayoutEvent` from the reactor and returns
`EventResponse` for side effects it cannot execute itself.

```rust
// src/layout_engine/engine.rs
pub enum LayoutEvent {
    WindowsOnScreenUpdated(
        SpaceId,
        pid_t,
        Vec<(WindowId, Option<String>, Option<String>, Option<String>, bool,
             CGSize, Option<CGSize>, Option<CGSize>)>,
        Option<AppInfo>,
    ),
    AppClosed(pid_t),
    WindowAdded(SpaceId, WindowId),
    WindowRemoved(WindowId),
    WindowRemovedPreserveFloating(WindowId),
    WindowFocused(SpaceId, WindowId),
    WindowResized {
        wid: WindowId,
        old_frame: CGRect,
        new_frame: CGRect,
        screens: Vec<(SpaceId, CGRect, Option<String>)>,
    },
    SpaceExposed(SpaceId, CGSize),
}

pub struct EventResponse {
    pub raise_windows: Vec<WindowId>,
    pub focus_window: Option<WindowId>,
    pub boundary_hit: Option<Direction>,
}
```

### Layout Engine And Workspace Model

`LayoutEngine` is the mutable layout state machine.

```rust
// src/layout_engine/engine.rs
pub struct LayoutEngine {
    workspace_layouts: WorkspaceLayouts,
    floating: FloatingManager,
    focused_window: Option<WindowId>,
    window_layout_constraints: HashMap<WindowId, WindowLayoutConstraints>,
    virtual_workspace_manager: VirtualWorkspaceManager,
    layout_settings: LayoutSettings,
    broadcast_tx: Option<BroadcastSender>,
    space_display_map: HashMap<SpaceId, Option<String>>,
    display_last_space: HashMap<String, SpaceId>,
}
```

`VirtualWorkspaceId` is the internal slotmap key. `WorkspaceNumber` is the
user-facing global slot number used by digit-row workspace commands.

```rust
// src/model/virtual_workspace.rs
pub struct VirtualWorkspaceId; // slotmap key
pub type WorkspaceNumber = usize;
pub const GLOBAL_WORKSPACE_SLOTS: usize = 10;
```

Workspace resolution returns the workspace and the display/space that owns it.

```rust
pub struct SlotTarget {
    pub space: SpaceId,
    pub workspace_id: VirtualWorkspaceId,
    pub per_space_index: usize,
    pub display_uuid: String,
}
```

`VirtualWorkspace` stores workspace-local membership and layout state.

```rust
pub struct VirtualWorkspace {
    pub number: WorkspaceNumber,
    pub name: String,
    pub space: SpaceId,
    windows: HashSet<WindowId>,
    last_focused: Option<WindowId>,
    pub layout_system: LayoutSystemKind,
    pub layout_mode: LayoutMode,
}
```

`VirtualWorkspaceManager` owns global workspace numbering, display binding,
active workspace state, app rules, and workspace assignment metadata.

```rust
pub struct VirtualWorkspaceManager {
    pub(crate) workspaces: SlotMap<VirtualWorkspaceId, VirtualWorkspace>,
    display_uuid_for_space: HashMap<SpaceId, String>,
    workspace_by_number: HashMap<WorkspaceNumber, VirtualWorkspaceId>,
    display_for_workspace: HashMap<VirtualWorkspaceId, String>,
    active_workspace_per_display: HashMap<String, WorkspaceNumber>,
    last_workspace_per_display: HashMap<String, WorkspaceNumber>,
    display_default_workspaces: HashMap<String, WorkspaceNumber>,

    floating_positions: HashMap<(SpaceId, VirtualWorkspaceId), FloatingWindowPositions>,
    workspace_counter: usize,
    app_rules: Vec<AppWorkspaceRule>,
    app_rule_regex_cache: Vec<Option<regex::Regex>>,
    max_workspaces: usize,
    pub workspace_auto_back_and_forth: bool,
    pub workspace_rules: Vec<WorkspaceLayoutRule>,
    pub default_layout_mode: LayoutMode,
    pub layout_settings: LayoutSettings,

    window_registry: WindowRegistryHandle,
    owned_window_registry: Box<WindowRegistry>,
}
```

App rule evaluation returns either an unmanaged decision or a managed workspace
assignment.

```rust
pub struct AppRuleAssignment {
    pub workspace_id: VirtualWorkspaceId,
    pub floating: bool,
    pub prev_rule_decision: bool,
}

pub enum AppRuleResult {
    Managed(AppRuleAssignment),
    Unmanaged,
}
```

`WorkspaceLayouts` maps workspace identity to concrete layout instances per
screen size.

```rust
// src/layout_engine/workspaces.rs
pub struct WorkspaceLayouts {
    map: HashMap<(SpaceId, VirtualWorkspaceId), SpaceLayoutInfo>,
}

struct SpaceLayoutInfo {
    configurations: HashMap<Size, LayoutId>,
    active_size: Size,
    last_saved: Option<LayoutId>,
}

struct Size {
    width: i32,
    height: i32,
}
```

Floating windows are workspace members, but they are not part of the tiling
tree.

```rust
// src/layout_engine/floating.rs
pub struct FloatingManager {
    floating_windows: BTreeSet<WindowId>,
    active_floating_windows: HashMap<SpaceId, HashMap<pid_t, HashSet<WindowId>>>,
    last_floating_focus: Option<WindowId>,
}
```

### Layout Systems

`LayoutSystem` is the interface implemented by concrete tiling systems.

```rust
// src/layout_engine/systems.rs
pub trait LayoutSystem {
    fn create_layout(&mut self) -> LayoutId;
    fn clone_layout(&mut self, layout: LayoutId) -> LayoutId;
    fn remove_layout(&mut self, layout: LayoutId);
    fn calculate_layout(...) -> Vec<(WindowId, CGRect)>;
    fn selected_window(&self, layout: LayoutId) -> Option<WindowId>;
    fn visible_windows_in_layout(&self, layout: LayoutId) -> Vec<WindowId>;
    fn add_window_after_selection(&mut self, layout: LayoutId, wid: WindowId);
    fn remove_window(&mut self, wid: WindowId);
    fn select_window(&mut self, layout: LayoutId, wid: WindowId) -> bool;
    fn move_focus(&mut self, layout: LayoutId, direction: Direction)
        -> (Option<WindowId>, Vec<WindowId>);
    fn swap_windows(&mut self, layout: LayoutId, a: WindowId, b: WindowId) -> bool;
    fn split_selection(&mut self, layout: LayoutId, kind: LayoutKind);
    fn toggle_fullscreen_of_selection(&mut self, layout: LayoutId) -> Vec<WindowId>;
    fn resize_selection_by(&mut self, layout: LayoutId, amount: f64);
    fn rebalance(&mut self, layout: LayoutId);
}

pub enum LayoutSystemKind {
    Bsp(BspLayoutSystem),
}
```

`BspLayoutSystem` is the current tiling implementation.

```rust
// src/layout_engine/systems/bsp.rs
pub struct BspLayoutSystem {
    layouts: SlotMap<LayoutId, LayoutState>,
    tree: Tree<Components>,
    kind: SecondaryMap<NodeId, NodeKind>,
    window_to_node: HashMap<WindowId, NodeId>,
}

struct LayoutState {
    root: NodeId,
}

enum NodeKind {
    Split {
        orientation: Orientation,
        ratio: f32,
    },
    Leaf {
        window: Option<WindowId>,
        fullscreen: bool,
        fullscreen_within_gaps: bool,
        preselected: Option<Direction>,
    },
}
```

`model::tree` is the generic tree storage used by BSP.

```rust
// src/model/tree.rs
pub struct Tree<O> {
    pub map: NodeMap,
    pub data: O,
}

pub struct NodeMap {
    map: SlotMap<NodeId, Node>,
}

pub struct OwnedNode(Option<NodeId>, String);
pub struct NodeId; // slotmap key
```

### Configuration And IPC

Runtime config is owned by `ConfigActor`; `WmController` reloads hotkeys when
key specs change.

```rust
// src/common/config.rs
pub struct Config {
    pub settings: Settings,
    pub keys: Vec<(Hotkey, WmCommand)>,
    pub key_specs: Vec<(String, WmCommand)>,
    pub virtual_workspaces: VirtualWorkspaceSettings,
}

pub struct VirtualWorkspaceSettings {
    pub enabled: bool,
    pub auto_assign_windows: bool,
    pub preserve_focus_per_workspace: bool,
    pub workspace_auto_back_and_forth: bool,
    pub reapply_app_rules_on_title_change: bool,
    pub app_rules: Vec<AppWorkspaceRule>,
    pub workspace_rules: Vec<WorkspaceLayoutRule>,
    pub display_default_workspaces: HashMap<String, usize>,
}

pub struct AppWorkspaceRule {
    pub app_id: Option<String>,
    pub workspace: Option<WorkspaceSelector>,
    pub floating: bool,
    pub manage: bool,
    pub app_name: Option<String>,
    pub title_regex: Option<String>,
    pub title_substring: Option<String>,
    pub ax_role: Option<String>,
    pub ax_subrole: Option<String>,
}
```

IPC is JSON over Mach messages and reuses the reactor/config command model.

```rust
// src/ipc/protocol.rs
pub enum RiftRequest {
    GetWorkspaces {
        space_id: Option<u64>,
        display_uuid: Option<String>,
    },
    GetDisplays,
    GetWindows {
        space_id: Option<u64>,
    },
    GetWindowInfo {
        window_id: String,
    },
    GetLayoutState {
        space_id: u64,
    },
    GetWorkspaceLayouts {
        space_id: Option<u64>,
        workspace_id: Option<usize>,
    },
    GetApplications,
    GetMetrics,
    GetConfig,
    ExecuteCommand {
        command: String,
        args: Vec<String>,
    },
    Subscribe {
        event: String,
    },
    Unsubscribe {
        event: String,
    },
    SubscribeCli {
        event: String,
        command: String,
        args: Vec<String>,
    },
    UnsubscribeCli {
        event: String,
    },
    ListCliSubscriptions,
}

pub enum RiftResponse {
    Success { data: Value },
    Error { error: Value },
}

pub enum RiftCommand {
    Reactor(crate::actor::reactor::Command),
    Config(crate::common::config::ConfigCommand),
}
```

## Common Change Points

- Add a new hotkey command: extend `WmCmd` or `LayoutCommand`, map it in
  `WmController::handle_event`, then implement the reactor/layout behavior.
- Add a layout-only behavior: prefer `LayoutCommand` plus `LayoutEngine` changes,
  returning `EventResponse` for focus/raise side effects.
- Add behavior based on live system focus, pointer position, or active app:
  implement resolution in `actor/reactor/events/command.rs` or nearby reactor
  logic, then pass exact `WindowId`s into the layout engine.
- Add a new OS observation: wrap raw API details in `sys`, convert them into an
  actor event, and let the reactor reconcile state.
- Add workspace metadata: put persistent workspace membership/number/display
  state in `VirtualWorkspaceManager`; put transient interaction state in the
  relevant reactor manager.

## Invariants To Preserve

- Treat `WindowId` as `(pid, idx)`. Never compare only `idx`.
- The reactor should be the only place that combines stale/asynchronous OS
  observations into coherent state.
- The layout engine should not call AX/SLS/AppKit directly; return
  `EventResponse` or broadcasts instead.
- Workspace number and workspace id are different concepts. Use
  `WorkspaceNumber` for user-facing global slots and `VirtualWorkspaceId` for
  internal workspace identity.
- Moving a window between workspaces must update membership, layout tree or
  floating state, last-focused workspace state, and workspace assignment
  metadata consistently.
- Display/space churn can temporarily produce incomplete display UUID mirrors.
  Code that resolves workspaces should use `VirtualWorkspaceManager` helpers
  instead of reading the mirror maps directly.
