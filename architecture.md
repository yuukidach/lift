# Lift Architecture

Lift is a focused macOS window manager built around one deterministic domain
state. The actor runtime collects platform observations and executes effects;
it does not maintain a second workspace or layout model.

This document describes the implementation that exists now.

## Boundaries

The dependency direction is inward:

```text
CLI / IPC / menu / hotkeys ─┐
                            ├─> serialized actor runtime ─> core
macOS AX / AppKit / SLS ─────┘             │
                                           └─> macOS effects
```

- `src/core/` is the domain. It owns stable identifiers, geometry, BSP trees,
  virtual workspaces, rules, commands, observations, effects, snapshots, and
  validation. It has no dependency on actors, Objective-C, or macOS handles.
- `src/actor/reactor.rs` is the serialized runtime coordinator. It normalizes
  live observations into core inputs, publishes snapshots, and routes effects.
- `src/runtime/` contains small runtime mechanisms: immutable snapshot storage,
  frame projection, and versioned persistence.
- `src/interfaces/` translates configuration and builds read-only query/UI
  projections from `CoreSnapshot`.
- `src/sys/` and the app/input actors are the macOS adapter layer. Unsafe code
  and platform handles stay here.
- `src/ipc/`, `src/bin/lift-cli.rs`, and `src/ui/` are interface adapters.

`actor`, `sys`, and `ui` retain their established directory names to keep the
macOS implementation navigable. Their role is defined by the boundary above,
not by domain-state ownership.

## Authoritative State

`CoreState` is the only writer for mutable domain state:

```rust
pub struct CoreState {
    platform: PlatformState,
    workspaces: WorkspaceCatalog,
    focus: FocusState,
    interactions: InteractionState,
    config: CoreConfig,
}
```

It owns:

- accepted display, native Space, application, and window facts;
- global workspace numbers 0 through 9, ordered as 1 through 9 then 0, and stable workspace IDs;
- per-display active/last workspaces and identity-safe display migration;
- window-to-workspace assignment, tiled/floating membership, and BSP groups;
- logical focus history, drag-swap state, and Mission Control phase;
- rule decisions and layout frames.

The runtime still keeps live platform objects and short-lived execution state,
such as AX app handles, animation progress, pending notifications, and retry
timers. Those values are adapters or in-flight effects, not a second domain
model.

Display UUID is durable identity. A macOS `SpaceId` belongs only to the latest
accepted platform snapshot. Workspace migration never uses historical
`SpaceId` mappings.

## Transaction Flow

Every domain change enters through `CoreState::transition(Input)`:

1. Validate the complete observation or typed command.
2. Apply it to a cloned candidate state.
3. Validate all invariants.
4. Calculate the changed snapshot and ordered effects.
5. Commit atomically and publish an immutable `Arc<CoreSnapshot>`.

Rejected input leaves the committed state unchanged. Read-only callers load
the latest snapshot through `ArcSwap`; queries never enter the mutation queue.

The runtime adapts the current actor-facing commands to typed core commands.
Unsupported historical layout commands and fields are absent from the schema,
so serde or clap returns an explicit error instead of accepting a no-op.

## Layout and Groups

Lift implements one layout: BSP. A leaf is a group containing one or more
windows. Joining adds a window to a neighboring group; unjoining creates a new
sibling group. Stack Line and grouped-window indicators are projections of
those groups, not separate layouts.

Layout planning is pure core work. `runtime::placement` converts core frames to
the actor window IDs and CoreGraphics rectangles needed by the macOS executor.
Animation changes how frames are applied, never which layout owns a window.

## Persistence and Replay

`SaveAndExit` writes `~/.lift/layout.ron` atomically. Schema version 1 persists
only stable workspace IDs, global numbers, and display UUID bindings. Live AX
objects, WindowServer IDs, window membership, and native `SpaceId` values are
reconstructed from fresh observations after launch.

Production startup validates the persisted schema before creating the runtime.
An absent file starts empty; malformed or incompatible state is logged and
ignored. Tests and diagnostic replay explicitly disable machine-local restore,
which keeps them deterministic.

Core inputs are serializable. The test suite replays serialized input sequences
through fresh `CoreState` instances and requires identical transitions and
final snapshots.

## Invariants

The core validates these rules after every transition:

- workspace numbers and IDs are unique;
- each online display has a valid active workspace;
- offline displays own no workspace after a committed topology change;
- a window belongs to at most one workspace;
- tiled windows occur in one BSP group and floating windows occur in none;
- BSP groups are nonempty and have a valid selection;
- platform references and display/native-Space assignments are coherent;
- published snapshots describe the committed state.

## Adding Behavior

- Add domain behavior as a typed `Input`, command, reducer transition, and
  effect under `src/core/`.
- Add macOS observation or effect code at the actor/`src/sys/` boundary.
- Add queries as pure projections of `CoreSnapshot`.
- Do not add mirrored workspace membership, layout trees, or mutable query
  caches outside the core.
- Keep platform retries explicit and generation-aware so stale completions
  cannot alter newer state.

For selective upstream work, start with [`docs/upstream-watch.md`](docs/upstream-watch.md).
It records the last observed upstream Rift commit and only unresolved Lift-fit
candidates, keeping the review context small enough for an LLM.
