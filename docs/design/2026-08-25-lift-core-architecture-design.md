# Lift Core Architecture Design

## Status

Approved in conversation on 2026-08-25. This document is the governing design
for turning the current simplified Rift fork into **Lift**, a focused macOS
window manager whose name means "lite Rift".

The redesign is intentionally a program of independently testable migrations,
not one repository-wide rewrite. Each migration plan must leave a buildable
application and must identify exactly which component is the sole writer for
the state it migrates.

## Goals

- Preserve every feature that is in active use in this fork.
- Replace duplicated window, workspace, display, and layout state with one
  authoritative core model.
- Make operating-system observations and side effects explicit, ordered, and
  replayable.
- Remove unsafe cross-owner state access and obsolete upstream compatibility
  surfaces.
- Permit breaking configuration, CLI, IPC, and persistence changes when they
  produce a simpler long-term interface.
- Keep the macOS adapters replaceable and keep the domain model testable without
  AppKit, Accessibility, CoreGraphics, SkyLight, or a running window server.
- Keep upstream feature review cheap enough for an LLM to perform with a small
  context window.

## Product Boundary

Lift retains the following product features:

- BSP tiling with Fibonacci-style insertion, directional focus and movement,
  resizing, orientation changes, join, unjoin, swap, constraints, and gaps.
- Global virtual workspace numbers 1 through 10, create, switch, move-window,
  switch-to-last, per-display active and last workspaces, and automatic removal
  of inactive empty workspaces.
- Multiple displays, display removal and reconnection, native Spaces changes,
  sleep/wake, login-screen transitions, and topology-safe workspace migration.
- Floating and fullscreen windows, focus-follows-mouse, mouse warp and hide,
  drag-to-swap, animations, gestures, and haptics.
- Menu bar controls, custom Mission Control, Stack Line, and grouped-window
  indicators.
- Application rules and live configuration reload.
- CLI control, typed IPC and subscriptions, service operation, metrics,
  diagnostics, recording, and deterministic replay.

Lift removes compatibility-only commands for layouts that this fork no longer
implements. This includes master/stack ratio and count commands, promote to
master, master-stack swap, strip scrolling and snapping, selection centering,
and selection of removed layout kinds. A request for an unsupported command is
a typed error; it is never silently accepted as a no-op.

## Naming and Platform Identity

The user-facing product, Rust package and library crate, primary executable,
CLI executable, documentation, default configuration, and IPC protocol names
become `lift`, `lift`, `lift`, `lift-cli`, `Lift`, `lift.default.toml`, and
`lift`, respectively. There is no permanent `rift` executable or CLI alias.

The existing macOS code-signing and launchd identity
`git.acsandmann.rift` remains unchanged. macOS Accessibility permission is
associated with this identity, and changing it would make a remotely managed
window manager unable to start until a local user grants permission again.
This retained identifier is an implementation detail, not product branding.
Its migration requires a separate explicit operational design.

References to upstream use the unambiguous names **upstream Rift** or
`upstream/main`.

## Architectural Shape

The target source tree has four boundaries:

```text
src/core/             deterministic domain state and transitions
src/runtime/          serialized input loop, snapshots, effects, and replay
src/platform/macos/   observation adapters and effect executors
src/interfaces/       config, hotkeys, CLI, IPC, menu, and UI projections
```

Dependencies point inward:

```text
interfaces  -> runtime -> core
platform    -> runtime -> core
```

`core` owns its identifiers and geometry types. It cannot import actors,
AppKit, Accessibility, CoreGraphics, SkyLight, Mach IPC, UI types, or platform
handles. Platform and interface modules translate at the boundary.

During migration, legacy modules may coexist with this tree, but new core code
must not depend on them. A capability has only one production writer at a time.
A shadow core may consume the same inputs and report differences, but it cannot
emit real effects before that capability is cut over.

## Authoritative State

All mutable domain state lives under one root:

```rust
pub struct CoreState {
    platform: PlatformState,
    workspaces: WorkspaceCatalog,
    focus: FocusState,
    interactions: InteractionState,
    config: CoreConfig,
}
```

### PlatformState

`PlatformState` contains only accepted macOS facts: online displays and their
frames, current native Space per display, observed applications and windows,
window capabilities and constraints, fullscreen/minimized status, and the
generation of each accepted snapshot. It does not assign virtual workspaces or
own layout membership.

Display identity is a stable display UUID wrapped as `DisplayId`. A `SpaceId`
is transient platform data attached to a current display snapshot. Virtual
workspaces never use `SpaceId` as their identity or long-term owner.

### WorkspaceCatalog

`WorkspaceCatalog` owns virtual workspaces, the global number-to-workspace
index, display bindings, per-display active and last workspaces, window
assignment, floating state, and each workspace's BSP tree.

Each window is assigned to at most one workspace. The authoritative direction
is `WindowId -> WorkspaceId`; a workspace's windows are derived from that index
and its BSP/floating membership instead of being mirrored in another set.
Reverse indexes are private implementation details and are updated atomically
with their source relation.

Each virtual workspace binds to a `DisplayId`, never to a `SpaceId`. When a
display disappears, its workspaces are rebound in place to the selected online
receiver, preserving workspace IDs, global numbers, window membership, BSP
structure, floating state, and valid active/last state. A returning display
does not reclaim workspaces automatically.

### BSP and Window Groups

The BSP tree contains only two structural nodes:

```rust
enum BspNode {
    Split {
        axis: Axis,
        ratio: Ratio,
        first: NodeId,
        second: NodeId,
    },
    Group {
        windows: Vec<WindowId>,
        selected: usize,
    },
}
```

A normal tiled window is a group with one member. Joining moves a window into a
neighboring group; unjoining turns the selected member into a sibling group;
all members of a group share a frame. Stack Line and group indicators are UI
projections of `Group`, not an independent layout or duplicate model.

### FocusState and InteractionState

`FocusState` owns logical focus and the history needed by focus commands.
`InteractionState` owns in-progress drag, workspace switch, display topology,
focus-next, Mission Control, and animation transactions. Temporary state that
spans observations must live here with an explicit generation or transaction
identifier; it cannot be hidden in unrelated event handlers.

## Inputs, Transactions, and Effects

The runtime feeds a single serialized core transition function:

```rust
pub enum Input {
    Observation(Observation),
    Command(Command),
    EffectCompleted(EffectCompletion),
    Timer(TimerEvent),
    ConfigReloaded(CoreConfig),
}

pub fn transition(
    state: &mut CoreState,
    input: Input,
) -> Result<Transition, CoreError>;
```

Every input is one transaction:

1. Validate the input and its generation against the current state.
2. Apply all related state changes or none of them.
3. Produce a `ChangeSet` describing affected displays, workspaces, windows, and
   UI projections.
4. Check core invariants in tests and debug builds.
5. Recalculate layouts only for affected workspaces.
6. Produce ordered effects, domain events, and a new immutable snapshot.
7. Commit the transition.

Topology and application observations use complete snapshots when correctness
depends on ordering. An incomplete topology observation returns
`AwaitingMoreData` and does not partially migrate state.

Effects are data, not direct calls from the reducer. The initial effect set is:

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

The macOS runtime executes effects and returns relevant outcomes as
`EffectCompleted` with the originating transaction and generation. Stale
completions are ignored or surfaced as typed diagnostics; they cannot mutate a
newer transaction.

Read-only clients load an immutable `Arc<CoreSnapshot>` published through
`ArcSwap`. Queries do not travel through the mutation queue and do not use
per-query synchronous reply channels.

## Errors and Recovery

Expected rejection uses typed errors: invalid command, missing window,
workspace conflict, stale generation, unsupported command, incomplete
observation, and platform effect failure. CLI and IPC preserve the error kind
and attach human-readable context.

The reducer never fabricates a default success response. An invariant failure
means state corruption: emit diagnostics, persist the replay tail when safe,
and perform a controlled process exit so launchd can restart Lift. Platform
adapter failures remain outside the reducer and are retried only when the
effect's explicit policy allows it.

## Interfaces and Persistence

Hotkeys, CLI, and IPC all produce the same typed `Command`. IPC uses a
versioned request/response/event envelope with golden JSON fixtures. UI actors
consume snapshots or purpose-built projections and never inspect mutable core
state.

Configuration is a versioned Lift schema. Removed Rift layout fields and
commands are rejected with a precise migration error. The old RON state format
is not loaded silently.

`PersistedState` contains only stable domain data that can survive a process or
OS restart: schema version, workspace IDs, global workspace numbers, and
display UUID bindings. Window membership and placement are reconstructed from
fresh platform observations because Lift has no durable window matcher. It
never treats current AX handles, WindowServer IDs, or `SpaceId` values as
durable identities. Runtime record/replay logs are diagnostic artifacts and
are separate from persistence.

## Testing Contract

Core tests use a transition harness that runs the real reducer and verifies
invariants after every input. The mandatory invariants are:

- Workspace numbers are unique and in `1..=10`.
- Every online display has exactly one valid active workspace; last-workspace
  references are either valid or absent.
- Every managed window belongs to exactly one workspace.
- Every tiled window occurs in exactly one BSP group.
- Floating windows do not occur in a BSP tree.
- Every platform reference points to a currently known object.
- Every nonempty group has a valid selected index.
- Offline displays own no workspaces after a committed topology transition.
- Current `SpaceId` assignments do not conflict within the accepted platform
  snapshot.
- Published snapshots agree with committed state.

Scenario tests cover startup event ordering, native Space changes, display
churn and `SpaceId` reuse, stale application snapshots, workspace lifecycle,
rules, minimize/restore/destroy, fullscreen, Mission Control, login screen,
sleep/wake, animation generations, join/group behavior, and Stack Line
projection.

Property tests generate command and observation sequences and assert invariant
preservation, deterministic replay, atomic topology changes, and effect
references to live objects. A fake effect executor tests completion ordering and
failure policies. Adapter tests cover macOS normalization separately.

Every migration must pass serial library tests, `cargo check`, formatting,
linting for the touched scope, deterministic replay fixtures, and any active
shadow comparison. A known failing baseline test must be fixed or replaced by
an equivalent passing scenario before the capability it covers is cut over.

## Migration Program

Implementation is split into separately planned and committed stages:

1. **Foundation and naming.** Rename the product/package/binaries/config and
   user-facing protocol to Lift while retaining the macOS identity. Add core
   identifiers, geometry, typed input/command/effect/snapshot boundaries, and
   the upstream-watch document. The legacy runtime remains the production
   writer.
2. **Workspace and BSP model.** Implement `WorkspaceCatalog`, grouped BSP, and
   invariant/property tests as a pure model. Feed observations into it in
   shadow mode and compare snapshots without emitting effects.
3. **Snapshot read path.** Move CLI queries, IPC subscriptions, menu bar,
   Mission Control, and Stack Line to `CoreSnapshot` projections.
4. **Window lifecycle and rules.** Cut over discovery, creation, destruction,
   minimize/restore, floating, fullscreen, and application-rule decisions.
5. **Workspace commands and layout effects.** Cut over workspace switching and
   moving, BSP commands, focus, layout planning, and frame application.
6. **Display and Space topology.** Cut over complete display snapshots, native
   Space changes, sleep/wake, login screen, and identity-safe migration.
7. **Interactions.** Cut over drag swap, pointer behavior, gestures, haptics,
   animations, focus transactions, and custom Mission Control.
8. **Retirement.** Remove the legacy reactor/layout engine/virtual workspace
   manager, raw-pointer handle and unsafe `Send`/`Sync`, compatibility commands,
   duplicate models, and shadow comparison machinery.

Within each stage, shadowing is read-only and temporary. At cutover, the new
component becomes the sole writer for the entire capability in one commit; the
legacy writer is disabled in that same commit. Feature flags used for
development are not permanent user configuration.

## Compact Upstream Feature Watch

The repository keeps one short file, `docs/upstream-watch.md`, capped at roughly
one page. It contains:

```yaml
---
remote: upstream
branch: main
last_observed: <full commit SHA>
observed_at: <YYYY-MM-DD>
---
```

Below the front matter are only three sections:

- **Scope:** concise `keep` and `ignore` lists reflecting Lift's product
  boundary.
- **Pending candidates:** unresolved items only, each with an upstream SHA, a
  one-line reason, relevant paths, and status.
- **Last review:** the inspected commit range and one-line result.

An LLM review reads this one file, fetches `upstream/main`, and first inspects
only:

```bash
git log --oneline <last_observed>..upstream/main
git diff --stat <last_observed>..upstream/main
git diff --name-status <last_observed>..upstream/main
```

It filters by `Scope`, then expands one candidate commit or path at a time.
Rejected and already-present items are removed; their reasoning remains in Git
history. An adopted local implementation references the upstream SHA in its
commit message and is removed from pending. Finally the review updates
`last_observed`, `observed_at`, and `Last review`.

There is no custom watcher executable, database, scheduled report, or automatic
pull request. The first review starts at this fork's merge base with
`upstream/main`; later reviews start at `last_observed`.

## Acceptance Criteria

The redesign is complete when all retained features run through the new core,
no mutable domain state is duplicated across owners, production code contains
no raw-pointer bridge between the workspace model and window registry, and the
legacy reactor/layout/workspace implementations are deleted. All required
tests and checks pass, replay is deterministic, display migration remains
identity-safe under `SpaceId` reuse, and `docs/upstream-watch.md` can drive the
next upstream review using only its compact context.
