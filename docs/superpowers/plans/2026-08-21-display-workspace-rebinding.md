# Display Workspace Rebinding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve global workspace identity and membership when a display is removed, deterministically rebind those workspaces to a remaining display, and prevent a delayed second topology refresh.

**Architecture:** Receiver selection stays in the reactor because it owns the ordered screen snapshot and live configuration. `VirtualWorkspaceManager` performs an in-place model rebind and returns explicit relocation records; `LayoutEngine` consumes those records to merge layout/floating mirrors without deleting receiver state. A shared reactor helper consumes topology relayout work as soon as a complete snapshot is committed.

**Tech Stack:** Rust 2024, Serde/TOML configuration, CoreGraphics geometry types, Rift reactor/layout-engine unit and integration tests.

**Spec:** `docs/superpowers/specs/2026-08-21-display-workspace-rebinding-design.md`

## Global Constraints

- Workspace IDs, global numbers, window membership, layout trees, floating membership, stored positions, rule metadata, and last-focused window survive display removal.
- The receiving display's active and last workspace remain unchanged.
- Explicit `display_migration_priority` order overrides the macOS main display; without an online configured UUID, choose the main display and then UUID lexical order.
- Reconnecting a display never moves workspaces back.
- Migration runs only for accepted remove/disable topology snapshots and never for transient missing-display snapshots.
- Do not add `Co-Authored-By` lines to commits.
- Run library tests serially because tests share process state.
- The pre-change baseline has one unrelated persistent failure after the reconnect contract test is updated: `actor::reactor::tests::it_preserves_layout_after_login_screen`.

---

### Task 1: Display migration priority configuration and receiver selection

**Files:**
- Modify: `src/common/config.rs:71-195`
- Modify: `src/actor/reactor/events/space.rs:1-230`
- Modify: `rift.default.toml:166-200`

**Interfaces:**
- Consumes: ordered `&[ScreenInfo]`, where index zero is the macOS main display.
- Produces: `VirtualWorkspaceSettings::display_migration_priority: Vec<String>`.
- Produces: `select_display_migration_receiver(screens: &[ScreenInfo], priority: &[String]) -> Option<MigrationReceiver>` with owned UUID, `SpaceId`, and `CGSize`.

- [x] **Step 1: Write failing configuration tests**

Add tests in `src/common/config.rs` proving the default is empty, TOML order is retained, and empty/duplicate entries are rejected:

```rust
#[test]
fn display_migration_priority_preserves_configured_order() {
    let settings: VirtualWorkspaceSettings = toml::from_str(
        r#"display_migration_priority = ["display-b", "display-a"]"#,
    )
    .unwrap();
    assert_eq!(
        settings.display_migration_priority,
        vec!["display-b".to_string(), "display-a".to_string()]
    );
}

#[test]
fn display_migration_priority_rejects_empty_and_duplicate_uuids() {
    let mut settings = VirtualWorkspaceSettings::default();
    settings.display_migration_priority = vec![
        "display-a".into(),
        "".into(),
        "display-a".into(),
    ];
    let issues = settings.validate();
    assert!(issues.iter().any(|issue| issue.contains("empty display UUID")));
    assert!(issues.iter().any(|issue| issue.contains("duplicate display UUID")));
}
```

- [x] **Step 2: Run the configuration tests and verify RED**

Run:

```bash
cargo test --lib display_migration_priority -- --test-threads=1
```

Expected: compilation fails because `display_migration_priority` does not exist.

- [x] **Step 3: Add the configuration field, default, validation, and example**

Add to `VirtualWorkspaceSettings`:

```rust
/// Ordered display UUIDs used as receivers when another display is removed.
#[serde(default)]
pub display_migration_priority: Vec<String>,
```

Initialize it with `Vec::new()` in `Default`. In `validate`, use a `HashSet` to report empty and duplicate UUID entries. Add the commented setting to `rift.default.toml` directly after `display_default_workspaces`:

```toml
# Preferred receivers when a display is removed, highest priority first.
# Online entries override the macOS main display; unlisted displays fall back
# to main-display-first, then display UUID alphabetical order.
display_migration_priority = []
```

- [x] **Step 4: Run configuration tests and verify GREEN**

Run:

```bash
cargo test --lib display_migration_priority -- --test-threads=1
```

Expected: all matching tests pass.

- [x] **Step 5: Write failing receiver-selection tests**

Add a local `#[cfg(test)]` module to `src/actor/reactor/events/space.rs`. Construct three complete `ScreenInfo` values in ordered main-first order and assert literal receiver UUIDs:

```rust
#[test]
fn configured_receiver_priority_overrides_main_display() {
    let screens = test_screens(&["display-main", "display-b", "display-a"]);
    let receiver = select_display_migration_receiver(
        &screens,
        &["offline".into(), "display-a".into(), "display-b".into()],
    )
    .unwrap();
    assert_eq!(receiver.display_uuid, "display-a");
}

#[test]
fn receiver_defaults_to_main_then_uuid_order() {
    let screens = test_screens(&["display-main", "display-z", "display-a"]);
    assert_eq!(
        select_display_migration_receiver(&screens, &[]).unwrap().display_uuid,
        "display-main"
    );

    let incomplete_main = vec![ScreenInfo { space: None, ..screens[0].clone() }, screens[1].clone(), screens[2].clone()];
    assert_eq!(
        select_display_migration_receiver(&incomplete_main, &[])
            .unwrap()
            .display_uuid,
        "display-a"
    );
}
```

- [x] **Step 6: Run receiver-selection tests and verify RED**

Run:

```bash
cargo test --lib receiver_priority -- --test-threads=1
cargo test --lib receiver_defaults -- --test-threads=1
```

Expected: compilation fails because the selection helper and result type do not exist.

- [x] **Step 7: Implement deterministic receiver selection**

Add a private owned result type and pure helper in `space.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
struct MigrationReceiver {
    display_uuid: String,
    space: SpaceId,
    size: CGSize,
}

fn select_display_migration_receiver(
    screens: &[ScreenInfo],
    priority: &[String],
) -> Option<MigrationReceiver> {
    let live = |screen: &&ScreenInfo| screen.space.is_some();
    for uuid in priority {
        if let Some(screen) = screens.iter().filter(live).find(|screen| &screen.display_uuid == uuid)
        {
            return Some(MigrationReceiver {
                display_uuid: screen.display_uuid.clone(),
                space: screen.space.unwrap(),
                size: screen.frame.size,
            });
        }
    }
    let screen = screens
        .first()
        .filter(|screen| screen.space.is_some())
        .or_else(|| {
            screens
                .iter()
                .filter(|screen| screen.space.is_some())
                .min_by(|a, b| a.display_uuid.cmp(&b.display_uuid))
        })?;
    Some(MigrationReceiver {
        display_uuid: screen.display_uuid.clone(),
        space: screen.space.unwrap(),
        size: screen.frame.size,
    })
}
```

Keep the helper side-effect free; topology orchestration is Task 4.

- [x] **Step 8: Run Task 1 tests and commit**

Run:

```bash
cargo fmt --all -- --check
cargo test --lib display_migration_priority -- --test-threads=1
cargo test --lib receiver_ -- --test-threads=1
```

Expected: all matching tests pass. Then commit:

```bash
git add src/common/config.rs src/actor/reactor/events/space.rs rift.default.toml
git commit -m "config: add display migration priority"
```

---

### Task 2: In-place virtual workspace rebinding

**Files:**
- Modify: `src/model/virtual_workspace.rs:200-250, 700-805, 1390-1505, 2880-end`

**Interfaces:**
- Consumes: a removed display UUID and an online receiver UUID already mapped to a live space.
- Produces: public `WorkspaceRelocation { workspace_id: VirtualWorkspaceId, old_space: SpaceId, new_space: SpaceId }`.
- Produces: `VirtualWorkspaceManager::rebind_workspaces_to_display(&mut self, dead_uuid: &str, receiver_uuid: &str) -> Vec<WorkspaceRelocation>`.

- [x] **Step 1: Write a failing model test for identity-preserving rebinding**

Create two display mappings, workspaces 1/2/3 on the source and 4/5/6 on the receiver, assign literal windows, set source rule metadata, and capture receiver active/last state. The key assertions are:

```rust
let relocations = manager.rebind_workspaces_to_display("display-a", "display-b");

assert_eq!(relocations.len(), 3);
for (number, original_id) in [(1, ws1), (2, ws2), (3, ws3)] {
    let target = manager.resolve_workspace(number).unwrap();
    assert_eq!(target.workspace_id, original_id);
    assert_eq!(target.display_uuid, "display-b");
    assert_eq!(target.space, receiver_space);
}
assert_eq!(manager.active_workspace(receiver_space), Some(ws5));
assert_eq!(manager.workspace_for_window(window_on_ws2), Some(ws2));
assert_eq!(
    manager.window_registry().get().workspace_info_for_window(window_on_ws2),
    Some(WindowWorkspaceInfo { space: receiver_space, workspace_id: ws2 })
);
assert!(manager.window_registry().get().rule_floating(window_on_ws2));
assert!(manager.window_registry().get().last_rule_decision(window_on_ws2));
```

Also assert that workspaces 4/5/6 still resolve to their original IDs and that a missing receiver produces an empty relocation vector with no state changes.

- [x] **Step 2: Run the model test and verify RED**

Run:

```bash
cargo test --lib rebind_workspaces_to_display -- --test-threads=1
```

Expected: compilation fails because the relocation type and method do not exist.

- [x] **Step 3: Implement the relocation type and atomic preflight**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceRelocation {
    pub workspace_id: VirtualWorkspaceId,
    pub old_space: SpaceId,
    pub new_space: SpaceId,
}
```

In `rebind_workspaces_to_display`, return immediately when UUIDs match, resolve the receiver space before mutation, and gather source workspace IDs through `workspace_ids_for_display(dead_uuid)`. Build all relocation records before changing state.

- [x] **Step 4: Implement in-place model updates**

For each relocation:

```rust
let windows: Vec<WindowId> = self.workspaces[relocation.workspace_id].windows().collect();
self.workspaces[relocation.workspace_id].space = relocation.new_space;
self.display_for_workspace
    .insert(relocation.workspace_id, receiver_uuid.to_string());
for window_id in windows {
    self.window_registry.get_mut().assign_window_to_workspace(
        window_id,
        WindowWorkspaceInfo {
            space: relocation.new_space,
            workspace_id: relocation.workspace_id,
        },
    );
}
if let Some(positions) = self
    .floating_positions
    .remove(&(relocation.old_space, relocation.workspace_id))
{
    self.floating_positions
        .insert((relocation.new_space, relocation.workspace_id), positions);
}
```

After the loop remove only `active_workspace_per_display[dead_uuid]` and `last_workspace_per_display[dead_uuid]`. Do not clear rule metadata, modify `workspace_by_number`, or touch receiver active/last state. Replace the old destructive method rather than retaining two competing display-removal contracts.

- [x] **Step 5: Run model tests and verify GREEN**

Run:

```bash
cargo test --lib model::virtual_workspace::tests::rebind_workspaces_to_display -- --test-threads=1
cargo test --lib model::virtual_workspace::tests -- --test-threads=1
```

Expected: the new test and all virtual-workspace tests pass.

- [x] **Step 6: Commit the model change**

```bash
git add src/model/virtual_workspace.rs
git commit -m "workspace: preserve identity during display removal"
```

---

### Task 3: Merge relocated layout and floating state

**Files:**
- Modify: `src/layout_engine/workspaces.rs:1-175`
- Modify: `src/layout_engine/floating.rs:1-130`
- Modify: `src/layout_engine/engine.rs:990-1110`

**Interfaces:**
- Consumes: `WorkspaceRelocation` values from Task 2 and receiver screen size from Task 1.
- Produces: `WorkspaceLayouts::relocate_workspace(old_space, new_space, workspace_id)`.
- Produces: `FloatingManager::remove_active_space(space)`.
- Produces: `LayoutEngine::rebind_workspaces_to_display(dead_uuid, receiver_uuid, receiver_size) -> Vec<WorkspaceRelocation>`.

- [x] **Step 1: Write a failing layout-map merge test**

In `src/layout_engine/workspaces.rs`, create layout entries for source workspace IDs 1/2 and receiver workspace IDs 4/5 using real layout trees. Relocate only the source IDs and assert:

```rust
layouts.relocate_workspace(source, receiver, ws1);
layouts.relocate_workspace(source, receiver, ws2);
assert!(layouts.active(source, ws1).is_none());
assert!(layouts.active(source, ws2).is_none());
assert!(layouts.active(receiver, ws1).is_some());
assert!(layouts.active(receiver, ws2).is_some());
assert_eq!(layouts.active(receiver, ws4), Some(receiver_ws4_layout));
assert_eq!(layouts.active(receiver, ws5), Some(receiver_ws5_layout));
```

This test catches accidental reuse of destructive `remap_space`, which would remove ws4/ws5.

- [x] **Step 2: Run the merge test and verify RED**

Run:

```bash
cargo test --lib workspace_relocation_merges_layout_entries -- --test-threads=1
```

Expected: compilation fails because `relocate_workspace` does not exist.

- [x] **Step 3: Implement per-workspace layout relocation**

Add the narrowly scoped method:

```rust
pub(crate) fn relocate_workspace(
    &mut self,
    old_space: SpaceId,
    new_space: SpaceId,
    workspace_id: VirtualWorkspaceId,
) {
    if old_space == new_space {
        return;
    }
    if let Some(info) = self.map.remove(&(old_space, workspace_id)) {
        self.map.insert((new_space, workspace_id), info);
    }
}
```

Do not alter the existing destructive `remap_space`; reconnect remapping still needs its replace-target semantics.

- [x] **Step 4: Write a failing layout-engine coordination test**

Create two initialized spaces with a workspace and tiled window on each, then call the desired engine API. Assert both original workspace IDs have active layout entries under the receiver and the receiver's active workspace is unchanged:

```rust
let moved = engine.rebind_workspaces_to_display(
    "display-a",
    "display-b",
    CGSize::new(1920.0, 1080.0),
);
assert_eq!(moved.len(), source_workspace_count);
assert_eq!(engine.active_workspace(receiver_space), Some(receiver_active));
assert!(engine.workspace_layouts.active(receiver_space, source_ws).is_some());
assert!(engine.workspace_layouts.active(receiver_space, receiver_ws).is_some());
```

- [x] **Step 5: Run the engine test and verify RED**

Run:

```bash
cargo test --lib layout_engine_rebinds_workspaces_without_replacing_receiver_layouts -- --test-threads=1
```

Expected: compilation fails because the engine wrapper does not exist.

- [x] **Step 6: Implement floating-cache cleanup and the engine wrapper**

Add `FloatingManager::remove_active_space`:

```rust
pub(crate) fn remove_active_space(&mut self, space: SpaceId) {
    self.active_floating_windows.remove(&space);
}
```

Implement the engine wrapper in this order:

1. call the VWM rebind and return early if no relocations;
2. remove each distinct old-space active-floating cache;
3. relocate each named workspace-layout entry;
4. ensure a receiver-size layout for each relocated workspace using its existing tree;
5. rebuild the receiver active-floating cache from
   `virtual_workspace_manager.windows_in_active_workspace(receiver_space)`.

Return the relocation vector for reactor diagnostics and tests.

- [x] **Step 7: Run Task 3 tests and commit**

Run:

```bash
cargo fmt --all -- --check
cargo test --lib workspace_relocation_merges_layout_entries -- --test-threads=1
cargo test --lib layout_engine_rebinds_workspaces_without_replacing_receiver_layouts -- --test-threads=1
cargo test --lib layout_engine::engine::tests -- --test-threads=1
```

Expected: all matching tests pass. Then commit:

```bash
git add src/layout_engine/workspaces.rs src/layout_engine/floating.rs src/layout_engine/engine.rs
git commit -m "layout: merge workspaces onto remaining display"
```

---

### Task 4: Reactor removal orchestration and immediate topology refresh

**Files:**
- Modify: `src/actor/reactor/events/space.rs:180-480`
- Modify: `src/actor/reactor/tests.rs:2530-2630, 4090-4270`

**Interfaces:**
- Consumes: `select_display_migration_receiver`, `LayoutEngine::rebind_workspaces_to_display`, and `display_migration_priority` from Tasks 1-3.
- Produces: `finish_pending_topology_relayout_if_ready(reactor: &mut Reactor) -> bool`.
- Preserves: `transient_missing_display_snapshot_does_not_migrate_workspaces` behavior.

- [x] **Step 1: Replace the old unplug test with a failing identity-preservation test**

Update `display_unplug_migrates_windows_to_remaining_display` to create multiple workspaces on the departing display and preserve their IDs. After a real REMOVE churn and complete one-display snapshot, assert literal behavior:

```rust
assert_eq!(vwm.workspace_for_window(window_on_ws2), Some(ws2));
assert_eq!(vwm.workspace_for_window(window_on_ws3), Some(ws3));
assert_eq!(vwm.resolve_workspace(2).unwrap().space, receiver_space);
assert_eq!(vwm.resolve_workspace(3).unwrap().space, receiver_space);
assert_eq!(engine.active_workspace(receiver_space), Some(receiver_active_before));
assert_ne!(vwm.workspace_for_window(window_on_ws2), Some(receiver_active_before));
```

Also assert the workspace IDs resolve unchanged and receiver-owned workspaces remain present.

- [x] **Step 2: Add failing priority and reconnect integration tests**

For three displays, configure `display_migration_priority = ["test-display-1"]`, remove `test-display-2`, and assert its workspace moves to space 2 rather than the main display's space 1.

Rewrite `switch_to_global_slot_survives_display_replug` around the new contract:

```rust
// REMOVE display 1 with proper churn flags.
assert_eq!(vwm.resolve_workspace(1).unwrap().space, space1);

// ADD the same UUID back with a fresh SpaceId.
assert_eq!(vwm.resolve_workspace(1).unwrap().space, space1);
assert_ne!(engine.active_workspace(new_space2), Some(original_ws1));
```

The returning display must own a newly-created smallest-unused default workspace.

- [x] **Step 3: Add a failing delayed-refresh regression test**

After a flagged removal with a complete screen snapshot, assert:

```rust
assert!(matches!(reactor.display_topology_manager.state(), TopologyState::Stable));
assert!(!reactor.pending_space_change_manager.topology_relayout_pending);
```

Move a window to another migrated workspace, send a duplicate `SpaceChanged` snapshot, and assert its workspace assignment remains unchanged. Drain app requests after the topology event and assert the duplicate event does not enqueue another `Request::GetVisibleWindows`.

- [x] **Step 4: Run the integration tests and verify RED**

Run:

```bash
cargo test --lib display_unplug_preserves_global_workspaces -- --test-threads=1
cargo test --lib display_removal_uses_configured_receiver_priority -- --test-threads=1
cargo test --lib display_replug_does_not_reclaim_migrated_workspaces -- --test-threads=1
cargo test --lib complete_topology_snapshot_does_not_defer_refresh -- --test-threads=1
```

Expected: assertions fail against destructive migration and stale pending behavior.

- [x] **Step 5: Reorder display-removal orchestration**

In `handle_screen_parameters_changed`:

1. compute `dead_uuids` and the owned receiver selection before moving `screens`;
2. arm `topology_relayout_pending` as soon as `should_trigger_topology` is true;
3. retain old display mappings until new screen spaces have been reconciled;
4. after `reconcile_spaces_with_display_history`, call the engine rebind once per dead UUID using the receiver UUID and screen size;
5. then call `prune_display_state` with the new active UUID list;
6. continue recompute/finalize logic.

This ordering lets a simultaneously-added receiver obtain its real UUID/space mapping before source workspaces move, while preserving departed mappings long enough to locate source workspaces.

- [x] **Step 6: Implement shared pending-relayout completion**

Add a helper that returns without clearing when any current screen lacks a space, spaces are duplicated, or display topology is still churning/awaiting commit. Otherwise it clears the flag, requests visible windows once, and performs the existing topology layout update:

```rust
fn finish_pending_topology_relayout_if_ready(reactor: &mut Reactor) -> bool {
    if !reactor.pending_space_change_manager.topology_relayout_pending
        || reactor.display_topology_manager.is_churning_or_awaiting_commit()
        || reactor.space_manager.screens.is_empty()
        || reactor.space_manager.screens.iter().any(|screen| screen.space.is_none())
    {
        return false;
    }
    let mut unique = HashSet::default();
    if reactor
        .space_manager
        .screens
        .iter()
        .filter_map(|screen| screen.space)
        .any(|space| !unique.insert(space))
    {
        return false;
    }
    reactor.pending_space_change_manager.topology_relayout_pending = false;
    reactor.force_refresh_all_windows();
    let _ = reactor.update_layout_or_warn_with(
        false,
        false,
        "Layout update failed after topology change",
    );
    true
}
```

Call `maybe_commit_display_topology_snapshot` before this helper at the end of both complete screen-parameter and space-change paths. Delete the old late arming block and inline SpaceChanged consumer.

- [x] **Step 7: Run integration and regression tests and verify GREEN**

Run:

```bash
cargo test --lib display_unplug_preserves_global_workspaces -- --test-threads=1
cargo test --lib display_removal_uses_configured_receiver_priority -- --test-threads=1
cargo test --lib display_replug_does_not_reclaim_migrated_workspaces -- --test-threads=1
cargo test --lib complete_topology_snapshot_does_not_defer_refresh -- --test-threads=1
cargo test --lib transient_missing_display_snapshot_does_not_migrate_workspaces -- --test-threads=1
cargo test --lib normal_macos_space_switch_does_not_arm_topology_relayout -- --test-threads=1
cargo test --lib fullscreen_space_in_screen_params_does_not_trigger_topology_relayout -- --test-threads=1
```

Expected: all matching tests pass.

- [x] **Step 8: Commit reactor behavior**

```bash
git add src/actor/reactor/events/space.rs src/actor/reactor/tests.rs
git commit -m "fix: preserve workspaces across display removal"
```

---

### Task 5: Full verification and documentation consistency

**Files:**
- Modify: `CLAUDE.md:20-70`
- Modify: `architecture.md:240-290`
- Modify: `docs/superpowers/plans/2026-08-21-display-workspace-rebinding.md` (checkbox progress only)

**Interfaces:**
- Consumes: all behavior from Tasks 1-4.
- Produces: formatted, type-checked code and a recorded verification result that distinguishes the known unrelated baseline failure.

- [x] **Step 1: Update stale lifecycle documentation**

In `CLAUDE.md`, add the display-removal invariant beside the workspace data-model invariants. In `architecture.md`, replace statements that say display removal destroys workspaces or frees their numbers. State that display binding is sticky while online and is reassigned, not destroyed, when the display disappears. Do not edit unrelated architecture sections.

- [ ] **Step 2: Run formatting and static checks**

Run:

```bash
cargo fmt --all -- --check
cargo check
git diff --check
```

Expected: all commands exit zero; existing dead-code warnings may remain.

- [x] **Step 3: Run all relevant tests excluding the known unrelated baseline failure**

Run:

```bash
cargo test --lib -- --test-threads=1 --skip actor::reactor::tests::it_preserves_layout_after_login_screen
```

Expected: all non-skipped tests pass with zero failures.

- [x] **Step 4: Re-run the full baseline command**

Run:

```bash
cargo test --lib -- --test-threads=1
```

Expected: either all tests pass, or the only failure is the pre-existing
`actor::reactor::tests::it_preserves_layout_after_login_screen`. Any other failure belongs to this change and must be fixed before completion.

- [x] **Step 5: Review the final diff against the spec**

Confirm all of these with the actual diff and test output:

- source workspace IDs and global numbers survive;
- receiver-owned workspaces/layouts survive;
- receiver active/last state survives;
- configured and default priority paths are covered;
- reconnect does not reclaim;
- transient snapshots do not migrate;
- complete topology leaves no delayed pending refresh;
- no unrelated production refactor was introduced.

- [x] **Step 6: Commit documentation or verification-driven adjustments**

```bash
git add CLAUDE.md architecture.md docs/superpowers/plans/2026-08-21-display-workspace-rebinding.md
git commit -m "docs: describe display workspace rebinding"
```

Only include files that actually changed; do not create an empty commit.
