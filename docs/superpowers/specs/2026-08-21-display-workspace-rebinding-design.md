# Preserve workspaces across display removal

**Date:** 2026-08-21
**Status:** approved for implementation
**Author:** brainstormed in collaboration

## Problem

Rift currently treats a removed display as the end of every workspace bound to
that display. `migrate_workspaces_off_display` moves all of their windows into
the receiving display's active workspace and destroys the original workspaces.
This collapses independent global workspaces into one.

A second, delayed collapse can happen after the display transition. A complete
`ScreenParametersChanged` event applies the new topology, but
`topology_relayout_pending` is armed at the end of the handler. The next
otherwise-unrelated `SpaceChanged` event consumes that stale flag, requests a
full application refresh, and relayouts everything after the user has already
started redistributing windows.

Rift's workspace numbers are global. Removing a display should therefore move
the workspace objects to a remaining display, not merge their windows or free
their numbers.

## Goals

- Preserve every live workspace's ID, global number, windows, layout tree,
  floating membership, stored floating positions, and last-focused window when
  its display is removed.
- Rebind those workspaces to one deterministic remaining display and its current
  native `SpaceId`.
- Preserve all workspaces already bound to the receiving display.
- Keep the receiving display's current and last workspace unchanged; migrated
  workspaces arrive inactive.
- Do not automatically move workspaces back when the old display reconnects.
- Consume the one-shot topology refresh during the complete topology event, so
  it cannot run after the user begins switching workspaces.
- Support a configurable receiving-display priority while retaining a stable
  default.

## Non-goals

- Persisting display ownership across Rift restarts.
- Spreading one removed display's workspaces across several receivers.
- Restoring workspaces to a reconnected display.
- Changing the global Cmd+N or workspace garbage-collection semantics.
- Changing which workspace is visible on the receiving display at migration
  time.

## Receiver selection

Add this optional setting under `[virtual_workspaces]`:

```toml
display_migration_priority = [
  "30999A24-1111-2222-3333-444444444444",
  "30999A24-5555-6666-7777-888888888888",
]
```

The list is ordered from highest to lowest priority. The first configured UUID
that is present in the new, complete screen snapshot is the receiver. This
explicit order may override the macOS main display.

If no configured UUID is online, Rift uses the stable default order:

1. the macOS main display, represented by the first item in `ScreenCache`'s
   ordered screen snapshot;
2. all other displays sorted lexicographically by UUID.

Empty and duplicate UUIDs are configuration validation errors. Configured UUIDs
that are well-formed but currently offline are ignored. When several displays
are removed in one topology change, all of their workspaces migrate to the same
highest-priority receiver.

Receiver selection only considers screens with a user `SpaceId`. If no complete
receiver exists, Rift leaves workspace ownership untouched and waits for a
later complete topology snapshot.

## State migration

The virtual workspace manager will replace the destructive
`migrate_workspaces_off_display` behavior with an in-place rebind operation. It
first resolves the receiver display and gathers all source workspaces, then
updates them as one logical operation.

For every workspace bound to the removed display:

- keep the same `VirtualWorkspaceId` and `WorkspaceNumber`;
- change `VirtualWorkspace.space` to the receiver's current `SpaceId`;
- change `display_for_workspace[workspace_id]` to the receiver UUID;
- leave `workspace_by_number[number]` unchanged;
- update each window registry assignment to the new space while retaining the
  same workspace ID and rule metadata;
- re-key stored floating positions from `(old_space, workspace_id)` to
  `(receiver_space, workspace_id)`.

After all workspaces move, remove the departed display's active and last
workspace entries. Do not overwrite the receiver's active or last entries. The
departed space-to-display mapping is pruned by the existing display-state prune
that runs after migration.

The model operation returns relocation records containing workspace ID, old
space, and new space. This makes the layout layer update its mirrors without
re-deriving migration state.

## Layout-state merge

The existing whole-space `remap_space` cannot be reused: it intentionally drops
all layout entries already associated with the target space. Display removal
needs a merge.

`WorkspaceLayouts` will gain a per-workspace relocation operation. It moves
only keys named by the relocation records from `(old_space, workspace_id)` to
`(receiver_space, workspace_id)`, leaving every pre-existing receiver workspace
entry intact. After moving the entries, the layout engine ensures each migrated
workspace has a layout for the receiver's current screen size. Its existing
layout tree remains the source for cloning a size-specific layout.

Floating windows keep their global floating membership. Active-floating caches
for departed spaces are removed, and the receiver cache is rebuilt from the
receiver's unchanged active workspace. Migrated inactive workspaces are hidden
by the normal layout pass. When one becomes active, existing visibility logic
centres any stored floating rectangle that is no longer visible on the receiver
screen.

## Display topology event flow

On `ScreenParametersChanged`:

1. Preserve and filter fullscreen/transient snapshots as today.
2. Compare old and new display UUID sets.
3. Queue newly missing UUIDs only while the accepted reconfiguration flags
   include `REMOVE` or `DISABLED`. Ignore unflagged missing snapshots, including
   an empty snapshot, and remove any queued UUID that appears in the latest live
   UUID set before considering migration.
4. When the screen/space snapshot is complete, non-duplicated, and has a user
   Space receiver, vacate every departing SpaceId before whole-space
   reconciliation can reuse it:
   - if the receiver already has a live model mapping, rebind all departing
     workspaces to that retained receiver SpaceId first, then reconcile the
     receiver and other retained displays onto their newly reported SpaceIds;
   - if the receiver is genuinely new, register only its reported mapping,
     rebind all departing workspaces to it, and then reconcile the full screen
     snapshot.
   This ordering makes `A@1 + B@2 -> A@2` merge-safe: B's workspace/layout
   entries leave Space 2 before the destructive `remap_space(1, 2)` step.
5. Clear the completed removal queue and prune departed display mappings.
6. Recompute active spaces, expose/finalize layouts without an ordinary app
   refresh, and commit the display topology.
7. The topology commit owns the single layout pass and exactly one
   `GetVisibleWindows` request per registered app. If no commit snapshot exists,
   the pending-relayout helper performs that work once as the explicit fallback;
   it then clears the one-shot flag.

The pending flag must be armed before attempting to consume it. A complete
`ScreenParametersChanged` event is sufficient to consume it; Rift must not
depend on a separate `SpaceChanged` notification, because the notification
layer suppresses that event while a screen refresh is pending.

If a removal snapshot is incomplete or has no eligible receiver, retain its
screen UUID/frame data and the removal queue. A later complete, unique
`SpaceChanged` vector updates those retained screens' SpaceIds and runs the same
receiver selection, collision-safe migration/reconciliation, pruning, topology
commit, and single-refresh completion path. A queued UUID that reappears in an
intervening screen snapshot is cancelled and never replayed.

## Reconnection behavior

Reconnecting the old display does not reclaim any migrated workspace. The
workspace's binding was changed when the display disappeared and remains on the
receiver. The reconnected display is treated as a display without a live
workspace and receives a new default workspace using the existing
`display_default_workspaces`/smallest-unused rules.

## Error handling and invariants

- Validate the receiver and gather all source workspaces before mutating state;
  a missing receiver is a no-op, not a partial migration.
- A removed display with no live workspaces is a successful no-op.
- Workspace numbers remain globally unique because no workspace is recreated.
- A window retains exactly one registry assignment and that assignment's space
  always matches its workspace's space.
- Every migrated workspace resolves through `resolve_workspace(number)` to the
  receiver display and space immediately after migration.
- The receiver's active workspace remains valid and unchanged.
- No layout mirror belonging to a pre-existing receiver workspace is removed.
- Existing transient-missing-display protection remains in place: migration
  only runs for remove/disable reconfiguration flags, and unflagged empty screen
  snapshots do not queue removal candidates.

## Files and responsibilities

- `src/common/config.rs`: deserialize/default/validate
  `display_migration_priority`.
- `rift.default.toml`: document the new setting.
- `src/model/virtual_workspace.rs`: in-place workspace and window-registry
  rebinding; return relocation records.
- `src/layout_engine/workspaces.rs`: merge layout keys for relocated
  workspaces.
- `src/layout_engine/floating.rs`: remove departed-space active cache and
  rebuild receiver active state.
- `src/layout_engine/engine.rs`: coordinate model relocation with layout and
  floating mirrors, including receiver-size layout initialization.
- `src/actor/reactor/events/space.rs`: deterministic receiver selection,
  migration orchestration, and immediate consumption of a complete topology
  refresh.
- `src/actor/reactor/tests.rs` and module-local unit tests: regression coverage.

## Test strategy

1. Configuration tests prove default empty priority, ordered TOML parsing, and
   rejection of empty or duplicate UUID entries.
2. Receiver-selection tests prove explicit order, offline-entry fallback,
   macOS-main default, and UUID ordering for non-main candidates.
3. Virtual workspace tests create workspaces 1/2/3 on a removed display and
   4/5/6 on a receiver, then prove IDs, numbers, windows, rule metadata, active
   receiver state, and resolver results survive rebinding.
4. Layout-engine tests prove moved layout entries coexist with receiver entries
   and are initialized for the receiver size.
5. Reactor integration tests perform a flagged two-display-to-one-display
   removal and prove windows remain assigned to their original workspaces rather
   than the receiver's active workspace.
6. Reconnection tests prove migrated workspaces remain on the receiver and the
   returning display gets a new default workspace.
7. Three-display tests prove configured receiver priority and default main/UUID
   ordering.
8. A topology-refresh regression test proves a complete removal snapshot leaves
   no pending relayout for the next `SpaceChanged` event and that a user's
   post-migration workspace assignments remain unchanged.

The repository baseline before this work is `cargo build` passing and
`cargo test --lib -- --test-threads=1` reporting 196 passed and 2 failed. The
pre-existing failures are
`actor::reactor::tests::it_preserves_layout_after_login_screen` and
`actor::reactor::tests::switch_to_global_slot_survives_display_replug`; the
latter encodes the old destructive reconnect behavior and will be replaced by
the new contract. The unrelated login-layout failure remains outside this
change.
