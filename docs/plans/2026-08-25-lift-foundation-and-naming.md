# Lift Foundation and Naming Implementation Plan

Execute this plan task-by-task in the current branch. Each checkbox is a
reviewable action, and each task ends with focused verification and a commit.

**Goal:** Establish the Lift product identity and add a platform-independent core API foundation without changing which code controls the live window manager.

**Architecture:** The existing actor/reactor/layout runtime remains the sole production writer in this phase. New code under `src/core/` defines stable identifiers, geometry, input, command, effect, state, and snapshot contracts with no macOS dependencies; later phases can populate and shadow this model. Product-facing Rift names change to Lift while the launchd and code-sign identity remains `git.acsandmann.rift` to preserve Accessibility permission.

**Tech Stack:** Rust 2024, Cargo, serde, thiserror, clap, macOS launchd/Mach integration, Git

**Spec:** `docs/design/2026-08-25-lift-core-architecture-design.md`

## Global Constraints

- Preserve all currently used BSP, workspace, display, floating/fullscreen, focus, interaction, animation, gesture, UI, rule, CLI, IPC, service, metrics, and replay features.
- The Rust package/library crate, main executable, CLI executable, default configuration, and user-facing product name become `lift`, `lift`, `lift`, `lift-cli`, `lift.default.toml`, and `Lift`.
- Do not ship permanent `rift` or `rift-cli` executable aliases.
- Keep the launchd label and embedded bundle identifier exactly `git.acsandmann.rift`.
- Keep the Mach service name `acsandmann.rift` in this phase because changing the live service endpoint belongs to the typed IPC cutover; rename only its user-facing Rust types and override environment variable.
- New `src/core/` code must not import `actor`, `layout_engine`, `model`, `sys`, `ui`, AppKit, Accessibility, CoreGraphics, SkyLight, or Mach types.
- Workspace numbers are globally unique values in `1..=10`; `SpaceId` is transient platform state and never a workspace identity.
- Use TDD for Rust behavior: add one failing test, verify the expected failure, add the minimum implementation, and verify green before continuing.
- Work directly on `refactor/lift-architecture`; do not merge to `main` until the full requested refactor is complete and verified.
- Commit messages use an area prefix followed by an imperative summary and contain no `Co-Authored-By` lines.

---

### Task 1: Add the compact upstream feature watch

**Files:**
- Create: `docs/upstream-watch.md`

**Interfaces:**
- Consumes: Git remote `upstream`, branch `upstream/main`, merge base `23a4c9d7a3acc5a0477dc5259715c80af7236bde`, and observed head `be8afef6036c77b67b4c49725ced6414601d63b0`.
- Produces: A one-page LLM context file with `remote`, `branch`, `last_observed`, `observed_at`, `Scope`, `Pending candidates`, and `Last review`.

- [ ] **Step 1: Confirm the upstream reference used by this plan**

Run:

```bash
git fetch upstream main
test "$(git merge-base HEAD upstream/main)" = "23a4c9d7a3acc5a0477dc5259715c80af7236bde"
test "$(git rev-parse upstream/main)" = "be8afef6036c77b67b4c49725ced6414601d63b0"
git log --oneline 23a4c9d7a3acc5a0477dc5259715c80af7236bde..upstream/main
git diff --stat 23a4c9d7a3acc5a0477dc5259715c80af7236bde..upstream/main
git diff --name-status 23a4c9d7a3acc5a0477dc5259715c80af7236bde..upstream/main
```

Expected: the assertions succeed and the compact inventory ends at
`be8afef6036c77b67b4c49725ced6414601d63b0`. If the head assertion fails, stop
and refresh this plan's observed head before creating the watch file.

- [ ] **Step 2: Inspect the six retained-scope candidates**

```bash
git show --stat --oneline 38572a003279d25b00391afc9611b214681293e5
git show --format=fuller 38572a003279d25b00391afc9611b214681293e5 -- src/actor/event_tap.rs src/sys/event_tap.rs
git show --stat --oneline 5e109721d8b68bf6f6bfdaa94fafa72ea55de9fe
git show --format=fuller 5e109721d8b68bf6f6bfdaa94fafa72ea55de9fe -- src/actor/app.rs
git show --stat --oneline 6c64d8b7bacda04958e673d32c7540c726c02928
git show --format=fuller 6c64d8b7bacda04958e673d32c7540c726c02928 -- src/model/app_rules.rs src/common/config.rs
git show --stat --oneline a39581e838fdce33981b4452272388fc49eac981
git show --format=fuller a39581e838fdce33981b4452272388fc49eac981 -- crates/rift-protocol src/ipc.rs
git show --stat --oneline e546861f6899384288940dc76dce5bbcd2fc70eb
git show --format=fuller e546861f6899384288940dc76dce5bbcd2fc70eb -- crates/rift-protocol/src/events.rs crates/rift-protocol/src/transport.rs
git show --stat --oneline c096f26fa440ac697962aaeb76be55bbb3076dbf
git show --format=fuller c096f26fa440ac697962aaeb76be55bbb3076dbf -- src/layout_engine/engine/persistence.rs
```

Use these exact filters:

- Keep: event-tap recovery, macOS write reliability, app-rule expressiveness,
  typed IPC ideas, identity-safe persistence, diagnostics, and performance
  improvements that preserve Lift's features.
- Ignore: scrolling/master-stack layouts, removed layout compatibility, and
  assumptions that key virtual workspaces by `SpaceId`.

- [ ] **Step 3: Create the one-page watch file**

Use this exact initial content:

```markdown
---
remote: upstream
branch: main
last_observed: be8afef6036c77b67b4c49725ced6414601d63b0
observed_at: 2026-08-25
---

# Upstream Rift Watch

## Scope

- Keep: event-tap recovery; macOS write reliability; richer app rules; typed
  IPC ideas; identity-safe persistence; diagnostics; relevant performance work.
- Ignore: scrolling/master-stack layouts; removed compatibility commands;
  `SpaceId`-owned virtual workspaces.

## Pending candidates

- `38572a003279d25b00391afc9611b214681293e5` — recover event taps that macOS disables — paths: `src/actor/event_tap.rs`, `src/sys/event_tap.rs` — status: review
- `5e109721d8b68bf6f6bfdaa94fafa72ea55de9fe` — retry failed Accessibility position writes — paths: `src/actor/app.rs` — status: review
- `6c64d8b7bacda04958e673d32c7540c726c02928` — extend app-rule match/action vocabulary — paths: `src/model/app_rules.rs`, `src/common/config.rs` — status: review
- `a39581e838fdce33981b4452272388fc49eac981`, `e546861f6899384288940dc76dce5bbcd2fc70eb` — reference typed IPC commands, queries, and events — paths: `crates/rift-protocol/`, `src/ipc.rs` — status: review
- `c096f26fa440ac697962aaeb76be55bbb3076dbf` — reference restore matching without persisting `SpaceId` — paths: `src/layout_engine/engine/persistence.rs` — status: review

## Last review

- Range: `23a4c9d7a3acc5a0477dc5259715c80af7236bde..be8afef6036c77b67b4c49725ced6414601d63b0`
- Result: Five feature areas remain candidates for selective Lift-native implementations; no upstream merge is planned.
```

- [ ] **Step 4: Verify and commit the watch file**

Run:

```bash
git diff --check -- docs/upstream-watch.md
git diff -- docs/upstream-watch.md
git add docs/upstream-watch.md
git commit -m "docs: add compact upstream Rift watch"
```

Expected: one concise document is committed; there is no watcher executable,
database, scheduled workflow, or generated report.

---

### Task 2: Introduce core identifiers and geometry

**Files:**
- Create: `src/core/mod.rs`
- Create: `src/core/ids.rs`
- Create: `src/core/geometry.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `serde::{Serialize, Deserialize}`, `thiserror::Error`, standard `NonZeroU32`.
- Produces: `ApplicationId`, `DisplayId`, `WorkspaceId`, `WorkspaceNumber`, `WindowId`, `SpaceId`, `Generation`, `TransactionId`, `EffectId`, `Point`, `Size`, `Rect`, and `GeometryError`.

- [ ] **Step 1: Export an empty core module**

Add `pub mod core;` to `src/lib.rs` and create `src/core/mod.rs` with:

```rust
pub mod geometry;
pub mod ids;
```

Run:

```bash
cargo check --lib
```

Expected: PASS, proving the boundary is wired before behavior is added.

- [ ] **Step 2: Write failing identifier tests**

In `src/core/ids.rs`, add tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_number_accepts_only_global_slots() {
        assert_eq!(WorkspaceNumber::try_from(1).unwrap().get(), 1);
        assert_eq!(WorkspaceNumber::try_from(10).unwrap().get(), 10);
        assert_eq!(WorkspaceNumber::try_from(0), Err(WorkspaceNumberError(0)));
        assert_eq!(WorkspaceNumber::try_from(11), Err(WorkspaceNumberError(11)));
    }

    #[test]
    fn window_identity_includes_application_and_index() {
        let index = NonZeroU32::new(7).unwrap();
        let first = WindowId::new(ApplicationId(41), index);
        let second = WindowId::new(ApplicationId(42), index);
        assert_ne!(first, second);
    }
}
```

Run:

```bash
cargo test --lib core::ids::tests -- --test-threads=1
```

Expected: compilation fails because the identifier types do not exist.

- [ ] **Step 3: Implement the identifier types**

Implement value types with these exact public shapes:

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ApplicationId(pub i32);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct DisplayId(pub String);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WorkspaceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SpaceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Generation(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct TransactionId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct EffectId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WindowId {
    pub application: ApplicationId,
    pub index: NonZeroU32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct WorkspaceNumber(u8);

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("workspace number must be in 1..=10, got {0}")]
pub struct WorkspaceNumberError(pub u8);
```

Provide `WindowId::new`, `WorkspaceNumber::get`, and `TryFrom<u8> for
WorkspaceNumber`. Do not add conversions from legacy identifiers in `core`;
those adapters belong outside this module.

- [ ] **Step 4: Verify identifier behavior**

Run:

```bash
cargo test --lib core::ids::tests -- --test-threads=1
```

Expected: 2 tests pass.

- [ ] **Step 5: Write failing geometry tests**

In `src/core/geometry.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_rejects_negative_or_non_finite_dimensions() {
        assert_eq!(Rect::new(0.0, 0.0, -1.0, 20.0), Err(GeometryError::InvalidSize));
        assert_eq!(Rect::new(0.0, 0.0, f64::NAN, 20.0), Err(GeometryError::NonFinite));
    }

    #[test]
    fn intersection_area_handles_arbitrary_display_origins() {
        let left = Rect::new(-1920.0, 0.0, 1920.0, 1080.0).unwrap();
        let window = Rect::new(-100.0, 100.0, 300.0, 200.0).unwrap();
        assert_eq!(left.intersection_area(window), 20_000.0);
    }
}
```

Run:

```bash
cargo test --lib core::geometry::tests -- --test-threads=1
```

Expected: compilation fails because the geometry types do not exist.

- [ ] **Step 6: Implement platform-independent geometry**

Define `Point { x, y }`, `Size { width, height }`, and `Rect { origin, size }`
as `Copy + PartialEq + Serialize + Deserialize` values using `f64`. Implement:

```rust
pub fn Rect::new(x: f64, y: f64, width: f64, height: f64)
    -> Result<Self, GeometryError>;
pub fn Rect::intersection_area(self, other: Self) -> f64;
```

Reject any non-finite component with `GeometryError::NonFinite`, reject negative
width or height with `GeometryError::InvalidSize`, and allow zero-sized rects.

- [ ] **Step 7: Verify and commit core primitives**

Run:

```bash
cargo test --lib core:: -- --test-threads=1
cargo check --lib
git add src/lib.rs src/core/mod.rs src/core/ids.rs src/core/geometry.rs
git commit -m "core: add domain identifiers and geometry"
```

Expected: the core tests and library check pass and the commit contains no
macOS imports under `src/core/`.

---

### Task 3: Define core inputs, commands, effects, state, and snapshots

**Files:**
- Create: `src/core/command.rs`
- Create: `src/core/config.rs`
- Create: `src/core/effect.rs`
- Create: `src/core/error.rs`
- Create: `src/core/input.rs`
- Create: `src/core/snapshot.rs`
- Create: `src/core/state.rs`
- Modify: `src/core/mod.rs`

**Interfaces:**
- Consumes: Task 2 identifiers and geometry.
- Produces: `Command`, `Input`, `Observation`, `Effect`, `EffectCompletion`, `CoreConfig`, `CoreError`, `CoreState`, `CoreSnapshot`, `ChangeSet`, and `Transition` data contracts.

- [ ] **Step 1: Write failing snapshot tests**

In `src/core/state.rs`, add these tests before the state implementation:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_publishes_an_empty_revision_zero_snapshot() {
        let state = CoreState::new(CoreConfig::default());
        let snapshot = state.snapshot();
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.platform_generation, Generation(0));
        assert!(snapshot.displays.is_empty());
        assert!(snapshot.workspaces.is_empty());
        assert!(snapshot.windows.is_empty());
        assert_eq!(snapshot.focused_window, None);
    }

    #[test]
    fn stale_effect_completion_is_not_current() {
        let state = CoreState::new(CoreConfig::default());
        let stale = EffectCompletion {
            effect_id: EffectId(8),
            transaction: TransactionId(3),
            generation: Generation(1),
            outcome: EffectOutcome::Succeeded,
        };
        assert!(!state.completion_is_current(&stale));
    }
}
```

Run:

```bash
cargo test --lib core::state::tests -- --test-threads=1
```

Expected: compilation fails because the boundary and state types do not exist.

- [ ] **Step 2: Define core configuration and commands**

Create `CoreConfig` with these fields and defaults:

```rust
pub struct CoreConfig {
    pub focus_follows_mouse: bool,              // false
    pub mouse_follows_focus: bool,              // false
    pub mouse_hides_on_focus: bool,             // false
    pub auto_destroy_empty_workspaces: bool,    // true
    pub animation: AnimationConfig,
}

pub struct AnimationConfig {
    pub enabled: bool,                          // true
    pub duration_seconds: f64,                  // 0.2
    pub frames_per_second: f64,                 // 120.0
}
```

In `command.rs`, define serializable `Direction`, `Command`, `WindowCommand`,
`WorkspaceCommand`, `DisplayCommand`, `MissionControlCommand`, and
`DiagnosticsCommand`. The command variants must cover:

```rust
pub enum Command {
    Window(WindowCommand),
    Workspace(WorkspaceCommand),
    Display(DisplayCommand),
    MissionControl(MissionControlCommand),
    Diagnostics(DiagnosticsCommand),
    ReloadConfig,
    SaveAndExit,
}
```

Window commands include directional focus/move, proportional resize,
toggle-floating, both fullscreen modes, join, unjoin, swap, close, next, and
previous. Workspace commands include activate by `WorkspaceNumber`, move a
specific or focused window, next, previous, last, and create. Display commands
include directional focus and moving a specific or focused window.

Use these exact payloads:

```rust
pub enum WindowCommand {
    Focus(Direction),
    Move(Direction),
    Resize { direction: Direction, amount: f64 },
    ToggleFloating,
    ToggleFullscreen,
    ToggleFullscreenWithinGaps,
    Join(Direction),
    Unjoin,
    Swap { first: WindowId, second: WindowId },
    Close(Option<WindowId>),
    Next,
    Previous,
}

pub enum WorkspaceCommand {
    Activate(WorkspaceNumber),
    MoveWindow { workspace: WorkspaceNumber, window: Option<WindowId> },
    Next { skip_empty: bool },
    Previous { skip_empty: bool },
    Last,
    Create,
}

pub enum DisplayCommand {
    Focus(Direction),
    MoveWindow { direction: Direction, window: Option<WindowId> },
}

pub enum MissionControlCommand { ShowAll, ShowCurrent, Dismiss }
pub enum DiagnosticsCommand { PrintTree, ShowTiming, StartRecording, StopRecording }
```

- [ ] **Step 3: Define observations and effect completions**

In `input.rs`, use these exact top-level contracts:

```rust
pub enum Input {
    Observation(Observation),
    Command(Command),
    EffectCompleted(EffectCompletion),
    Timer(TimerEvent),
    ConfigReloaded(CoreConfig),
}

pub enum Observation {
    PlatformSnapshot(PlatformSnapshotObservation),
}

pub struct PlatformSnapshotObservation {
    pub generation: Generation,
    pub displays: Vec<DisplayObservation>,
    pub windows: Vec<WindowObservation>,
}
```

`DisplayObservation` contains `DisplayId`, `Rect`, and `Option<SpaceId>`.
`WindowObservation` contains `WindowId`, `Rect`, `Option<DisplayId>`, and the
booleans `minimized`, `fullscreen`, and `resizable`. `EffectCompletion` contains
`EffectId`, `TransactionId`, `Generation`, and `EffectOutcome`.

- [ ] **Step 4: Define effects, errors, snapshots, and transition output**

In `effect.rs`, define every effect required by the spec:

```rust
pub enum Effect {
    ApplyLayout(LayoutRequest),
    FocusWindow(WindowId),
    RaiseWindow(WindowId),
    RefreshWindows(RefreshRequest),
    CloseWindow(WindowId),
    SwitchNativeSpace(NativeSpaceRequest),
    WarpPointer(Point),
    SetPointerHidden(bool),
    Haptic(HapticKind),
    UpdateUi(UiSnapshot),
    Publish(DomainEvent),
    RunCommand(ExternalCommand),
    Save(PersistedState),
    Shutdown(ShutdownReason),
}
```

`LayoutRequest` contains `WorkspaceId` and `Vec<WindowFrame>`;
`WindowFrame` contains `WindowId` and `Rect`; `RefreshRequest` contains an
optional `ApplicationId`; `NativeSpaceRequest` contains `DisplayId` and
`SpaceId`; `ExternalCommand` contains a program and argument vector. Define
closed enums with these variants:

```rust
pub enum HapticKind { Alignment, LevelChange }
pub enum ShutdownReason { Requested, InvariantViolation }
pub enum EffectOutcome {
    Succeeded,
    Failed { code: String, message: String },
}
```

In `error.rs`, define the exact typed errors below with stable human-readable
`thiserror` messages:

```rust
pub enum CoreError {
    InvalidCommand(String),
    MissingWindow(WindowId),
    WorkspaceConflict(WorkspaceId),
    StaleGeneration { expected: Generation, received: Generation },
    UnsupportedCommand(String),
    IncompleteObservation(String),
    PlatformEffectFailed { effect: EffectId, message: String },
    InvariantViolation(String),
}
```

In `snapshot.rs`, define owned, serializable `DisplaySnapshot`,
`WorkspaceSnapshot`, `WindowSnapshot`, and `UiSnapshot`; define:

```rust
pub struct DisplaySnapshot {
    pub id: DisplayId,
    pub frame: Rect,
    pub space: Option<SpaceId>,
    pub active_workspace: Option<WorkspaceId>,
    pub last_workspace: Option<WorkspaceId>,
}

pub struct GroupSnapshot {
    pub windows: Vec<WindowId>,
    pub selected: usize,
}

pub struct WorkspaceSnapshot {
    pub id: WorkspaceId,
    pub number: WorkspaceNumber,
    pub display: DisplayId,
    pub groups: Vec<GroupSnapshot>,
    pub floating_windows: Vec<WindowId>,
}

pub struct WindowSnapshot {
    pub id: WindowId,
    pub workspace: Option<WorkspaceId>,
    pub frame: Rect,
    pub floating: bool,
    pub minimized: bool,
    pub fullscreen: bool,
}

pub struct CoreSnapshot {
    pub revision: u64,
    pub platform_generation: Generation,
    pub displays: Vec<DisplaySnapshot>,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub windows: Vec<WindowSnapshot>,
    pub focused_window: Option<WindowId>,
}

pub struct UiSnapshot {
    pub revision: u64,
    pub active_workspace_by_display: BTreeMap<DisplayId, WorkspaceId>,
    pub focused_window: Option<WindowId>,
}

pub struct PersistedWorkspace {
    pub id: WorkspaceId,
    pub number: WorkspaceNumber,
    pub display: DisplayId,
}

pub struct PersistedState {
    pub schema_version: u16,
    pub workspaces: Vec<PersistedWorkspace>,
}

pub enum DomainEvent {
    SnapshotPublished { revision: u64 },
    WorkspaceChanged { display: DisplayId, workspace: WorkspaceId },
    FocusChanged { window: Option<WindowId> },
}
```

All vectors are emitted in identifier order so equal state always serializes
identically. Define the remaining output contracts exactly as:

```rust
pub struct ChangeSet {
    pub displays: BTreeSet<DisplayId>,
    pub workspaces: BTreeSet<WorkspaceId>,
    pub windows: BTreeSet<WindowId>,
    pub focus_changed: bool,
    pub config_changed: bool,
    pub ui_changed: bool,
}

pub struct Transition {
    pub transaction: TransactionId,
    pub changes: ChangeSet,
    pub effects: Vec<Effect>,
    pub events: Vec<DomainEvent>,
    pub snapshot: Arc<CoreSnapshot>,
}
```

- [ ] **Step 5: Implement the non-operational CoreState foundation**

Define `CoreState` and its internal state with these exact fields. No legacy
event is routed into them in this phase.

```rust
pub struct CoreState {
    platform: PlatformState,
    workspaces: WorkspaceCatalog,
    focus: FocusState,
    interactions: InteractionState,
    config: CoreConfig,
    revision: u64,
}

struct PlatformState {
    generation: Generation,
    displays: BTreeMap<DisplayId, DisplayObservation>,
    windows: BTreeMap<WindowId, WindowObservation>,
}

struct WorkspaceCatalog {
    workspaces: BTreeMap<WorkspaceId, WorkspaceSnapshot>,
    window_assignment: BTreeMap<WindowId, WorkspaceId>,
}

struct FocusState { focused_window: Option<WindowId> }
struct InteractionState { current_transaction: TransactionId }
```

Implement:

```rust
impl CoreState {
    pub fn new(config: CoreConfig) -> Self;
    pub fn snapshot(&self) -> Arc<CoreSnapshot>;
    pub fn completion_is_current(&self, completion: &EffectCompletion) -> bool;
}
```

`completion_is_current` returns true only when the completion generation equals
the state's platform generation and its transaction equals the state's current
transaction. Initial generation and transaction are both zero. Do not add a
partial `transition` implementation; Stage 2 introduces the reducer with the
workspace model.

- [ ] **Step 6: Verify tests and dependency direction**

Run:

```bash
cargo test --lib core:: -- --test-threads=1
cargo check --lib
rg -n "crate::(actor|layout_engine|model|sys|ui)|objc2|CoreGraphics|SkyLight|mach" src/core
```

Expected: all core tests pass, the library checks, and `rg` returns no matches.

- [ ] **Step 7: Commit the core contracts**

Run:

```bash
git add src/core src/lib.rs
git commit -m "core: define transition boundaries"
```

Expected: the commit adds only the pure core boundary and its unit tests.

---

### Task 4: Rename the Cargo package, executables, configuration paths, and service executable

**Files:**
- Create: `tests/lift_brand.rs`
- Rename: `src/bin/rift.rs` to `src/bin/lift.rs`
- Rename: `src/bin/rift-cli.rs` to `src/bin/lift-cli.rs`
- Rename: `rift.default.toml` to `lift.default.toml`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/bin/lift.rs`
- Modify: `src/bin/lift-cli.rs`
- Modify: `src/common/config.rs`
- Modify: `src/sys/service.rs`
- Modify: `assets/Info.plist`

**Interfaces:**
- Consumes: Existing executable behavior and the `git.acsandmann.rift` platform identity.
- Produces: Cargo package/library `lift`, executables `lift` and `lift-cli`, default paths `~/.lift` and `~/.config/lift/config.toml`, and launchd service execution of `lift`.

- [ ] **Step 1: Write failing product-brand tests**

Create `tests/lift_brand.rs`:

```rust
use std::process::Command;

#[test]
fn lift_cli_help_uses_lift_brand() {
    let output = Command::new(test_bin::get_test_bin("lift-cli"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Command-line interface for Lift window manager"));
}

#[test]
fn lift_agent_help_uses_lift_command_name() {
    let output = Command::new(test_bin::get_test_bin("lift"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Usage: lift"));
}
```

Run:

```bash
cargo test --test lift_brand -- --test-threads=1
```

Expected: FAIL because `lift` and `lift-cli` binaries do not exist.

- [ ] **Step 2: Write failing path and service lookup tests**

Add pure helpers `data_dir_for(home: &Path)` and `config_file_for(home: &Path)`
to `src/common/config.rs`, then first add tests asserting:

```rust
let home = Path::new("/Users/example");
assert_eq!(data_dir_for(home), PathBuf::from("/Users/example/.lift"));
assert_eq!(config_file_for(home), PathBuf::from("/Users/example/.config/lift/config.toml"));
```

Rename the existing service test to
`find_lift_executable_prefers_stable_symlink_path` and change its fixture names
to `lift-real` and `lift`. Run the two targeted tests before changing the
implementations:

```bash
cargo test --lib common::config::tests::lift_paths_use_lift_directories -- --test-threads=1
cargo test --lib sys::service::tests::find_lift_executable_prefers_stable_symlink_path -- --test-threads=1
```

Expected: the config assertion fails with `.rift`/`.config/rift`, and the
service test fails because lookup still searches for `rift`.

- [ ] **Step 3: Rename Cargo artifacts and Rust crate imports**

Set `[package].name = "lift"`, set the explicit primary `[[bin]]` name to
`"lift"`, and keep the existing `dev` binary. Rename both binary source files.
Replace external crate paths `rift_wm::` with `lift::` in both binaries. Cargo's
automatic binary discovery provides `lift-cli` from `src/bin/lift-cli.rs`.

Run:

```bash
cargo metadata --no-deps --format-version 1
cargo check --bins
```

Expected: metadata names package/library `lift` and binaries include `lift`,
`lift-cli`, and `dev`; all binaries check.

- [ ] **Step 4: Rename configuration and service executable paths**

Rename `rift.default.toml` to `lift.default.toml`; update `include_str!` in
`src/common/config.rs`; implement `.lift` and `.config/lift/config.toml` paths.
Rename internal service symbols from `RIFT_PLIST` to `LEGACY_SERVICE_LABEL` and
from `find_rift_executable*` to `find_lift_executable*`. Search only for a
`lift` executable and update service messages and log paths to Lift, while
leaving the label value exactly `git.acsandmann.rift`.

In `assets/Info.plist`, set `CFBundleExecutable` and `CFBundleName` to `lift` and
`Lift`; leave `CFBundleIdentifier` exactly `git.acsandmann.rift`.

- [ ] **Step 5: Add explicit clap branding and remove deprecated startup flags**

Set the main parser name/about to Lift and update its service help. Remove the
deprecated no-op `--validate` and `--restore` fields and the unused
`restore_file` import. Set `lift-cli`'s parser name and about text to Lift.
This is an allowed breaking change and prevents new Lift documentation from
carrying obsolete compatibility.

- [ ] **Step 6: Verify product and path behavior**

Run:

```bash
cargo test --test lift_brand -- --test-threads=1
cargo test --lib common::config::tests::lift_paths_use_lift_directories -- --test-threads=1
cargo test --lib sys::service::tests::find_lift_executable_prefers_stable_symlink_path -- --test-threads=1
cargo check --bins
```

Expected: all targeted tests and checks pass.

- [ ] **Step 7: Commit the artifact rename**

Run:

```bash
git add Cargo.toml Cargo.lock src/bin/lift.rs src/bin/lift-cli.rs src/common/config.rs src/sys/service.rs assets/Info.plist lift.default.toml tests/lift_brand.rs
git add -u src/bin/rift.rs src/bin/rift-cli.rs rift.default.toml
git commit -m "brand: rename executables and package to Lift"
```

Expected: the commit contains the package/artifact/path rename, while
`git.acsandmann.rift` remains present in the plist and service implementation.

---

### Task 5: Rename user-facing IPC, environment, and UI symbols

**Files:**
- Modify: `src/ipc/protocol.rs`
- Modify: `src/ipc.rs`
- Modify: `src/ipc/cli_exec.rs`
- Modify: `src/bin/lift-cli.rs`
- Modify: `src/sys/mach.rs`
- Modify: `src/sys/accessibility.rs`
- Modify: `src/actor/menu_bar.rs`
- Modify: `src/ui/menu_bar.rs`
- Modify: `src/bin/lift.rs`
- Modify: affected tests under `src/actor/reactor/tests.rs`, `src/sys/service.rs`, and IPC modules

**Interfaces:**
- Consumes: Existing JSON wire shapes and existing Mach service `acsandmann.rift`.
- Produces: Rust protocol names `LiftRequest`, `LiftResponse`, `LiftCommand`, `LiftMachClient`, and `LiftMachSubscription`; environment namespace `LIFT_*`; visible UI text `Lift`.

- [ ] **Step 1: Write a failing environment mapping test**

Extract the pure function
`fn environment_for_event(event: &BroadcastEvent) -> Result<HashMap<String,
String>, serde_json::Error>` from `DefaultCliExecutor::execute`. Add a unit test
using this `WorkspaceChanged` event and assert these values:

```rust
let mut workspaces =
    slotmap::SlotMap::<crate::layout_engine::VirtualWorkspaceId, ()>::with_key();
let workspace_id = workspaces.insert(());
let event = BroadcastEvent::WorkspaceChanged {
    space_id: crate::sys::screen::SpaceId::new(91),
    workspace_id,
    workspace_name: "code".into(),
    display_uuid: Some("display-a".into()),
};
let env = environment_for_event(&event).unwrap();

assert_eq!(env["LIFT_EVENT_TYPE"], "workspace_changed");
assert_eq!(env["LIFT_WORKSPACE_ID"], workspace_id.to_string());
assert_eq!(env["LIFT_WORKSPACE_NAME"], "code");
assert_eq!(env["LIFT_SPACE_ID"], "91");
assert_eq!(env["LIFT_DISPLAY_UUID"], "display-a");
assert!(env.contains_key("LIFT_EVENT_JSON"));
assert!(!env.keys().any(|key| key.starts_with("RIFT_")));
```

Run:

```bash
cargo test --lib ipc::cli_exec::tests::event_environment_uses_lift_namespace -- --test-threads=1
```

Expected: FAIL because current variables use `RIFT_*`.

- [ ] **Step 2: Rename protocol and client Rust types**

Rename `RiftRequest`, `RiftResponse`, `RiftCommand`, `RiftMachClient`, and
`RiftMachSubscription` to their `Lift*` forms in definitions, re-exports,
server matching, CLI request construction, and tests. Do not add deprecated type
aliases. Preserve the serialized enum variant names in this phase so the live
server and client continue to interoperate during the rename commit.

- [ ] **Step 3: Rename environment variables and visible product strings**

Change all CLI subscription variables from `RIFT_*` to `LIFT_*`, including
`LIFT_EVENT_JSON` and `LIFT_ACTIVE_WORKSPACE_HAS_FULLSCREEN`. Rename
`RIFT_CLI_PRETTY` to `LIFT_CLI_PRETTY` and the Mach test override
`RIFT_BS_NAME` to `LIFT_BS_NAME`.

Rename visible messages, menu labels, event-log text, Objective-C class names,
selectors, and Rust menu variants from Rift to Lift. Keep references that
explicitly identify upstream Rift and keep these two platform constants:

```text
git.acsandmann.rift
acsandmann.rift
```

- [ ] **Step 4: Verify protocol, environment, and binary behavior**

Run:

```bash
cargo test --lib ipc:: -- --test-threads=1
cargo test --test lift_brand -- --test-threads=1
cargo check --bins
rg -n "Rift(Request|Response|Command|Mach)|RIFT_" src
```

Expected: tests/checks pass and `rg` returns no matches. The lower-case local
variable `triggered_by_rift` may remain until the owning reactor is migrated;
it describes legacy transaction provenance and is not a public interface.

- [ ] **Step 5: Commit the interface rename**

Run:

```bash
git add src
git commit -m "brand: rename public interfaces to Lift"
```

Expected: the runtime still behaves through the legacy reactor, but public Rust
types, environment variables, CLI output, and UI text use Lift.

---

### Task 6: Align documentation, release packaging, and run the phase gate

**Files:**
- Modify: `README.md`
- Modify: `CLAUDE.md`
- Modify: `architecture.md`
- Modify: `manifesto.md`
- Modify: `roadmap.md`
- Modify: `.gitignore`
- Modify: `.github/workflows/release.yml`
- Modify: `lift.default.toml`
- Modify: relevant source comments and test descriptions under `src/`

**Interfaces:**
- Consumes: Completed Lift artifact/interface rename from Tasks 4 and 5.
- Produces: Accurate maintainer/user documentation and release artifacts named `lift` and `lift-cli`.

- [ ] **Step 1: Update user and maintainer documentation**

Rewrite current-project references to Lift and describe it as a simplified fork
of upstream Rift. Update command examples, config paths, environment variables,
headings, image alt text, service commands, and architecture file paths. Keep
copyright and attribution references to upstream Rift where historically
required. In `CLAUDE.md`, update build/install examples to Lift but retain and
explain `git.acsandmann.rift` as the stable Accessibility identity.

- [ ] **Step 2: Update packaging and ignored local config**

Change `.gitignore` from `rift.toml` to `lift.toml`. Update the release workflow
asset basename, lipo inputs/outputs, tar members, tap formula path, and commit
message from Rift to Lift. Update `lift.default.toml` comments and examples to
the Lift CLI, paths, event variables, and product name.

- [ ] **Step 3: Audit remaining old names by category**

Run:

```bash
rg -n --hidden -g '!target/**' -g '!.git/**' '(?i)rift' .
```

Classify every remaining match into exactly one allowed category:

- upstream Rift attribution/history/URLs;
- the design spec or upstream watch;
- the retained identifiers `git.acsandmann.rift` or `acsandmann.rift`;
- legacy internal reactor variable names explicitly deferred to their owning
  migration.

Rename every other match to Lift. Do not alter the LICENSE attribution text.

- [ ] **Step 4: Run formatting and focused verification**

Run:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --lib core:: -- --test-threads=1
cargo test --lib ipc:: -- --test-threads=1
cargo test --test lift_brand -- --test-threads=1
git diff --check
```

Expected: every command passes with no warnings introduced by this phase.

- [ ] **Step 5: Run the serial library regression suite**

Run:

```bash
cargo test --lib -- --test-threads=1
```

Expected: all tests pass. If the historical login-screen test fails, stop,
identify its root cause, and fix or replace the scenario; do not bless a red
baseline in Lift.

- [ ] **Step 6: Commit documentation and release changes**

Run:

```bash
git add README.md CLAUDE.md architecture.md manifesto.md roadmap.md .gitignore .github/workflows/release.yml lift.default.toml src
git commit -m "docs: align project surfaces with Lift"
```

Expected: documentation and release packaging agree with the implemented Lift
artifacts and the working tree is clean.

- [ ] **Step 7: Record phase evidence**

Run:

```bash
git status --short --branch
git log --oneline 02896e9..HEAD
git diff --stat 02896e9..HEAD
```

Expected: branch `refactor/lift-architecture` is clean and contains the design
commit plus the independently testable Stage 1 commits. Do not merge to `main`;
continue with a separate Stage 2 workspace/BSP specification and plan.
