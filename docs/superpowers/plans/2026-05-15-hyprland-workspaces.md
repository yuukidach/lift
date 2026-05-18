# Hyprland-style workspaces — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace rift's slot-pool workspace model with dynamic, ephemeral workspaces bound to one display at creation, eliminating the post-replug "Cmd+1 wrong display" bug.

**Architecture:** Phased refactor. Phase 1 adds new state alongside old + flips resolution (fixes the bug minimally). Phase 2 layers Hyprland behaviors. Phase 3 implements ephemeral lifecycle and display events. Phase 4 removes the now-dead old infrastructure. Phase 5 updates CLI/sketchybar/user-config.

**Tech Stack:** Rust, slotmap, serde, ron (restore file), test-log.

**Spec:** `docs/superpowers/specs/2026-05-15-hyprland-workspaces-design.md`

**Build/test workflow** (per project memory):
- Build & deploy to running WM: `rift-build && launchctl kickstart -k gui/$UID/git.acsandmann.rift.hotload`
- Run unit tests: `cargo test -- --test-threads=1` (reactor tests touch global state)
- Smoke test in WM: described per-task; user's repro is "press Cmd+1 from each display, verify it lands on ws 1's bound display, especially after a display replug."

**Rule for every commit:** the build must succeed and the existing test suite must pass. The internal data model is allowed to be inconsistent (old/new state out of sync) ONLY if no behavior depends on it during that commit.

---

## File Structure

| File | Phase touching it | Responsibility |
|------|-------------------|----------------|
| `src/model/virtual_workspace.rs` | 1, 2, 3, 4 | The manager. Restructured incrementally. |
| `src/common/config.rs` | 1, 4 | `VirtualWorkspaceSettings` schema. |
| `src/actor/reactor/events/command.rs` | 1, 2 | `handle_command_switch_to_global_slot` + LayoutCommand dispatch. |
| `src/layout_engine/engine.rs` | 2, 3 | `handle_virtual_workspace_command`, broadcast emitters. |
| `src/actor/reactor.rs` | 3 | Display add/remove propagation. |
| `src/actor/reactor/events/space.rs` | 3 | Space attach/detach hooks. |
| `src/actor/reactor/query.rs` | 5 | Workspace query handler. |
| `src/ipc/protocol.rs` | 5 | `GetWorkspaces` request shape. |
| `src/bin/rift-cli.rs` | 5 | CLI flag parsing. |
| `src/actor/reactor/tests.rs` | 1, 2, 3 | Integration tests. |
| `~/.config/rift/config.toml` | 5 | User config migration. |
| `~/.config/sketchybar/plugins/rift.sh` | 5 | Bar render script. |
| `~/.config/sketchybar/sketchybarrc` | 5 | Click script fix. |

---

## Phase 1 — Foundation + bug fix (minimal change)

Add new state alongside old. Switch resolution to use it. The existing test suite continues to pass. The replug bug is fixed.

### Task 1.1: Add `display_default_workspaces` config field

**Files:**
- Modify: `src/common/config.rs:67-97` (`VirtualWorkspaceSettings`)

- [ ] **Step 1: Add the new field next to `slot_displays`**

In `VirtualWorkspaceSettings`, add (keep `slot_displays` for now — both will coexist this phase):

```rust
/// Per-display default workspace number assigned at startup. Replaces
/// `slot_displays` (which inverts the mapping). Example:
///   display_default_workspaces = { "30999A24-..." = 1 }
/// Means "if this display is online at startup, its default ws is 1."
/// Mid-session ws creation is not constrained by this map.
#[serde(default)]
pub display_default_workspaces: HashMap<String, usize>,
```

- [ ] **Step 2: Build, verify warnings**

```bash
cargo build 2>&1 | grep -E "(warning|error)" | head -20
```

Expected: builds clean (the field is unused so far, but `#[serde(default)]` keeps it from breaking deserialization).

- [ ] **Step 3: Commit**

```bash
git add src/common/config.rs
git commit -m "config: add display_default_workspaces field (replaces slot_displays)"
```

---

### Task 1.2: Add `WorkspaceNumber` type alias and new state fields

**Files:**
- Modify: `src/model/virtual_workspace.rs` (struct definitions around line 173-219)

- [ ] **Step 1: Add the type alias near the top of the file (after the `new_key_type!` macro)**

```rust
/// Global workspace identifier exposed to the user (the digit on the hotkey).
/// 0..=9 today; the type is `usize` for indexing convenience.
pub type WorkspaceNumber = usize;
```

- [ ] **Step 2: Add new fields to `VirtualWorkspaceManager`** (line 174 onward)

```rust
    /// New model: number → ws id. The `Some` variant means the ws exists.
    /// Mirrored from `workspaces_by_space` during phase 1, source of truth
    /// after phase 4.
    #[serde(skip)]
    workspace_by_number: HashMap<WorkspaceNumber, VirtualWorkspaceId>,
    /// New model: ws id → bound display UUID. Immutable during ws lifetime
    /// (under stable display config).
    #[serde(skip)]
    display_for_workspace: HashMap<VirtualWorkspaceId, String>,
    /// New model: per-display active ws number. Mirrors
    /// `active_workspace_per_space` during phase 1.
    #[serde(skip)]
    active_workspace_per_display: HashMap<String, WorkspaceNumber>,
    /// New model: per-display previous ws number for switch_to_last.
    #[serde(skip)]
    last_workspace_per_display: HashMap<String, WorkspaceNumber>,
    /// Phase-1 config field. Same data as `display_default_workspaces` but
    /// stored UUID-keyed for fast lookup at startup.
    #[serde(skip)]
    display_default_workspaces: HashMap<String, WorkspaceNumber>,
```

- [ ] **Step 3: Initialize the new fields in `new_with_config` (around line 247-269)**

```rust
        let mut manager = Self {
            workspaces: SlotMap::default(),
            workspaces_by_space: HashMap::default(),
            active_workspace_per_space: HashMap::default(),
            window_to_workspace: HashMap::default(),
            ws_home_display: HashMap::default(),
            slot_displays: config.slot_displays.clone(),
            display_uuid_for_space: HashMap::default(),
            window_rule_floating: HashMap::default(),
            last_rule_decision: HashMap::default(),
            floating_positions: HashMap::default(),
            workspace_counter: 1,
            app_rules: config.app_rules.clone(),
            app_rule_regex_cache: Vec::new(),
            max_workspaces,
            default_workspace_count: config.default_workspace_count,
            default_workspace_names: config.workspace_names.clone(),
            default_workspace,
            workspace_auto_back_and_forth: config.workspace_auto_back_and_forth,
            workspace_rules: config.workspace_rules.clone(),
            default_layout_mode: layout_settings.mode,
            layout_settings: layout_settings.clone(),
            // NEW:
            workspace_by_number: HashMap::default(),
            display_for_workspace: HashMap::default(),
            active_workspace_per_display: HashMap::default(),
            last_workspace_per_display: HashMap::default(),
            display_default_workspaces: config.display_default_workspaces.clone(),
        };
```

- [ ] **Step 4: Mirror the same in `update_settings` (around line 287)**

After the existing `self.slot_displays = config.slot_displays.clone();` line, add:

```rust
        self.display_default_workspaces = config.display_default_workspaces.clone();
```

- [ ] **Step 5: Build**

```bash
cargo build
```

Expected: clean build (new fields are unused so far).

- [ ] **Step 6: Commit**

```bash
git add src/model/virtual_workspace.rs
git commit -m "vwm: add WorkspaceNumber and new-model state fields (unused)"
```

---

### Task 1.3: Mirror old state into new tables on every mutation

The new `workspace_by_number`, `display_for_workspace`, and `active_workspace_per_display` must reflect the old data structures so phase-1 reads from them are correct.

**Approach:** After every mutation that could change the (slot ↔ workspace ↔ display) relationship, call a single `rebuild_new_model_mirrors()` helper that walks the old data and rebuilds the new tables. Cheaper than instrumenting each mutation point.

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Add the rebuild helper as a private method on `VirtualWorkspaceManager`**

Add somewhere logical (e.g. after `set_space_display` around line 537):

```rust
    /// Phase-1 helper: rebuild `workspace_by_number`, `display_for_workspace`,
    /// `active_workspace_per_display` from the old per-space state. Cheap
    /// (≤10 entries per display today). Called after any mutation that could
    /// move a workspace or change which slot it occupies. Will be deleted in
    /// phase 4 when the new tables become the source of truth.
    fn rebuild_new_model_mirrors(&mut self) {
        self.workspace_by_number.clear();
        self.display_for_workspace.clear();
        self.active_workspace_per_display.clear();

        for (space, ids) in &self.workspaces_by_space {
            let Some(display_uuid) = self.display_uuid_for_space.get(space).cloned() else {
                continue;
            };
            for (slot, ws_id) in ids.iter().enumerate() {
                // The slot_displays config pin wins for which display "owns"
                // a slot; without a pin, the rule is "first display we see
                // for this slot." Old priority order, preserved as mirror.
                if let Some(pinned) = self.slot_displays.get(&slot) {
                    if pinned == &display_uuid {
                        self.workspace_by_number.insert(slot, *ws_id);
                        self.display_for_workspace.insert(*ws_id, display_uuid.clone());
                    }
                } else if !self.workspace_by_number.contains_key(&slot) {
                    self.workspace_by_number.insert(slot, *ws_id);
                    self.display_for_workspace.insert(*ws_id, display_uuid.clone());
                }
            }
            if let Some((_, active)) = self.active_workspace_per_space.get(space) {
                if let Some(slot) = ids.iter().position(|id| id == active) {
                    self.active_workspace_per_display.insert(display_uuid, slot);
                }
            }
        }
    }
```

- [ ] **Step 2: Call it from every relevant mutation**

Add a call at the END of these methods:
- `update_settings` (line 275-311) — call `self.rebuild_new_model_mirrors();` at the end
- `set_space_display` (line 527-536) — same
- `remap_space` (line 538-onward) — same, at the end
- `set_active_workspace` (search for `pub fn set_active_workspace` in the file) — same
- Any other public method that adds/removes workspaces or changes their display assignment. Use grep:
  ```bash
  grep -n "workspaces_by_space\|active_workspace_per_space\|workspaces.insert\|workspaces.remove" src/model/virtual_workspace.rs
  ```
  For each `&mut self` method that mutates these, add a `self.rebuild_new_model_mirrors();` call as the last line.

- [ ] **Step 3: Build**

```bash
cargo build
```

Expected: clean build. New fields are populated but not yet read.

- [ ] **Step 4: Commit**

```bash
git add src/model/virtual_workspace.rs
git commit -m "vwm: mirror new-model state from old tables on every mutation"
```

---

### Task 1.4: Add `resolve_workspace` (the new lookup)

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Add the new resolution method**

Place it next to the old `resolve_slot_target`:

```rust
    /// New-model resolver: returns the (workspace_id, display_uuid, space_id)
    /// for workspace number `n`, if it exists. No priority chain — single
    /// HashMap lookup.
    pub fn resolve_workspace(&self, n: WorkspaceNumber) -> Option<SlotTarget> {
        let workspace_id = *self.workspace_by_number.get(&n)?;
        let display_uuid = self.display_for_workspace.get(&workspace_id)?.clone();
        let space = self.space_for_display_uuid_internal(&display_uuid)?;
        // Per-space index: position of this ws in workspaces_by_space[space].
        // Phase 1 needs this so the fallback to existing per-space switch
        // machinery still works. Goes away in phase 4.
        let per_space_index = self
            .workspaces_by_space
            .get(&space)?
            .iter()
            .position(|id| *id == workspace_id)?;
        Some(SlotTarget {
            space,
            workspace_id,
            per_space_index,
            display_uuid,
        })
    }
```

- [ ] **Step 2: Build**

```bash
cargo build
```

- [ ] **Step 3: Commit**

```bash
git add src/model/virtual_workspace.rs
git commit -m "vwm: add resolve_workspace (new-model lookup, unused)"
```

---

### Task 1.5: Failing test for the post-replug bug

The original symptom: after a display unplug+replug, Cmd+1 stops landing on the secondary display even though slot 1 is pinned there in config.

**Files:**
- Modify: `src/actor/reactor/tests.rs`

- [ ] **Step 1: Read the existing `switch_to_global_slot_homes_to_owning_display` test (lines 1490-1558)** to understand the fixture pattern.

- [ ] **Step 2: Add a new test below it that simulates a display replug**

```rust
#[test]
fn switch_to_global_slot_survives_display_replug() {
    // Reproduces the user-reported bug: after a display unplug + replug
    // (which gives the display a fresh SpaceId), Cmd+1 must still land on
    // the secondary display when slot 1 is pinned to its UUID.
    let TwoSpaceFixture {
        mut reactor,
        screen1: _,
        screen2,
        space1,
        space2: original_space2,
    } = two_space_fixture();

    // Lazy-init both spaces.
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(original_space2);

    // Pin slot 1 to screen2's display via the new config field.
    let secondary_uuid = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .space_display(original_space2)
        .expect("space2 has a display uuid")
        .to_owned();

    let mut new_settings = crate::common::config::VirtualWorkspaceSettings::default();
    new_settings.display_default_workspaces.insert(secondary_uuid.clone(), 1);
    reactor
        .layout_manager
        .layout_engine
        .update_virtual_workspace_settings(&new_settings);

    // Simulate the replug: feed a screen_params_event with the same display
    // UUIDs but a fresh SpaceId for screen2.
    let new_space2 = SpaceId::new(999);
    reactor.handle_event(screen_params_event(
        vec![screen1.frame, screen2.frame],
        vec![Some(space1), Some(new_space2)],
        vec![],
    ));

    // Lazy-init the new space2 so it has workspaces.
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(new_space2);

    // Press Cmd+1 from screen1 (the non-pinned display).
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::SwitchToGlobalSlot(1))));

    // Slot 1 should be active on screen2's NEW SpaceId, not screen1.
    let new_space2_active = reactor
        .layout_manager
        .layout_engine
        .active_workspace(new_space2);
    let new_space2_workspaces = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .list_workspaces(new_space2);

    assert_eq!(
        new_space2_active,
        Some(new_space2_workspaces[1]),
        "slot 1 must land on the pinned display after replug"
    );
}
```

- [ ] **Step 3: Run the test, verify it FAILS**

```bash
cargo test --test-threads=1 switch_to_global_slot_survives_display_replug 2>&1 | tail -30
```

Expected: FAIL. The current resolver does not honor `display_default_workspaces` (it only knows `slot_displays`); even if it did, the priority chain post-replug picks the wrong space.

- [ ] **Step 4: Commit the failing test**

```bash
git add src/actor/reactor/tests.rs
git commit -m "test: failing repro for post-replug Cmd+1 routing bug"
```

---

### Task 1.6: Migrate `slot_displays` → `display_default_workspaces` on config load (alongside)

**Files:**
- Modify: `src/model/virtual_workspace.rs` (`new_with_config` and `update_settings`)

- [ ] **Step 1: After loading `display_default_workspaces` in `new_with_config`, also fold in the old `slot_displays` for backwards compat**

Replace the `display_default_workspaces: config.display_default_workspaces.clone(),` line in `new_with_config` with a small computation:

```rust
            display_default_workspaces: {
                let mut m = config.display_default_workspaces.clone();
                // Backwards compat: slot_displays = { 1 = "uuid" } reads as
                // display_default_workspaces = { "uuid" = 1 }. New keys win.
                for (slot, uuid) in &config.slot_displays {
                    m.entry(uuid.clone()).or_insert(*slot);
                }
                m
            },
```

- [ ] **Step 2: Same migration in `update_settings`**

Replace the `self.display_default_workspaces = config.display_default_workspaces.clone();` line:

```rust
        self.display_default_workspaces = {
            let mut m = config.display_default_workspaces.clone();
            for (slot, uuid) in &config.slot_displays {
                m.entry(uuid.clone()).or_insert(*slot);
            }
            m
        };
```

- [ ] **Step 3: Build**

```bash
cargo build
```

- [ ] **Step 4: Commit**

```bash
git add src/model/virtual_workspace.rs
git commit -m "vwm: backwards-compat slot_displays into display_default_workspaces"
```

---

### Task 1.7: Have `rebuild_new_model_mirrors` honor `display_default_workspaces`

The current mirror uses old `slot_displays` for pin priority. Switch to the new field so the test's pin is respected.

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Replace the pin-checking block in `rebuild_new_model_mirrors`**

Replace:

```rust
                if let Some(pinned) = self.slot_displays.get(&slot) {
                    if pinned == &display_uuid {
                        self.workspace_by_number.insert(slot, *ws_id);
                        self.display_for_workspace.insert(*ws_id, display_uuid.clone());
                    }
                } else if !self.workspace_by_number.contains_key(&slot) {
```

With:

```rust
                let pinned_for_this_display = self
                    .display_default_workspaces
                    .get(&display_uuid)
                    .copied();
                if pinned_for_this_display == Some(slot) {
                    // This slot is the pinned default for this display.
                    // Always wins, even over any prior assignment.
                    self.workspace_by_number.insert(slot, *ws_id);
                    self.display_for_workspace.insert(*ws_id, display_uuid.clone());
                } else if !self.workspace_by_number.contains_key(&slot) {
```

- [ ] **Step 2: Build**

```bash
cargo build
```

- [ ] **Step 3: Commit**

```bash
git add src/model/virtual_workspace.rs
git commit -m "vwm: mirror honors display_default_workspaces pin"
```

---

### Task 1.8: Switch `handle_command_switch_to_global_slot` to new resolver

This is the bug fix.

**Files:**
- Modify: `src/actor/reactor/events/command.rs:136-180`

- [ ] **Step 1: Replace the body of `handle_command_switch_to_global_slot`**

Old:
```rust
        let target = reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_slot_target(slot, source_uuid.as_deref());
```

New (drop the `source_uuid` argument since the new resolver doesn't need it):
```rust
        let _ = source_uuid;  // No longer used; resolution is display-pin based.
        let target = reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(slot);
```

Keep the rest of the function unchanged (the `target` shape is the same `SlotTarget` so downstream code still works).

- [ ] **Step 2: Run the failing test from Task 1.5; verify it passes**

```bash
cargo test --test-threads=1 switch_to_global_slot_survives_display_replug 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 3: Run the full test suite to verify no regressions**

```bash
cargo test --test-threads=1 2>&1 | tail -40
```

Expected: all tests pass. The two existing `switch_to_global_slot_*` tests should still pass (their semantics — pin via record_slot_home + per-space-index resolution — are still served by `rebuild_new_model_mirrors` when `slot_displays` is set).

- [ ] **Step 4: If the existing `switch_to_global_slot_homes_to_owning_display` test fails**

It uses `record_slot_home(1, owning_uuid)` rather than `display_default_workspaces`. The new resolver has no concept of "home" recording. Update the test to use config instead:

Replace:
```rust
    reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .record_slot_home(1, owning_uuid);
```

With:
```rust
    let mut new_settings = crate::common::config::VirtualWorkspaceSettings::default();
    new_settings.display_default_workspaces.insert(owning_uuid, 1);
    reactor
        .layout_manager
        .layout_engine
        .update_virtual_workspace_settings(&new_settings);
```

Re-run, verify pass.

- [ ] **Step 5: Smoke test in the running WM**

```bash
rift-build && launchctl kickstart -k gui/$UID/git.acsandmann.rift.hotload
```

Then manually:
1. Press Cmd+1 from main display — focus jumps to whichever display has slot 1
2. Press Cmd+1 from secondary display — focus jumps the same way (idempotent)
3. Unplug secondary monitor
4. Plug it back in
5. Press Cmd+1 from main display — focus must jump to secondary

Expected: step 5 works. Bug fixed.

- [ ] **Step 6: Commit**

```bash
git add src/actor/reactor/events/command.rs src/actor/reactor/tests.rs
git commit -m "fix: SwitchToGlobalSlot uses display-pin resolver, survives replug"
```

---

**End of Phase 1.** The replug bug is fixed. The system retains the old slot-pool model internally; the new resolver is a thin layer on top. Existing config (`slot_displays`) still works.

---

## Phase 2 — Hyprland behaviors

Add: focus-jump-on-cross-display-switch, create-on-demand, prev/next strictly per-display.

### Task 2.1: Failing test — Cmd+N creates ws if it doesn't exist

The current `slot_workspace`-based check returns None for a non-existent slot, causing the command to be a no-op. We want it to create the ws on the focused display.

**Files:**
- Modify: `src/actor/reactor/tests.rs`

- [ ] **Step 1: Add the test**

```rust
#[test]
fn switch_to_global_slot_creates_workspace_when_absent() {
    // Cmd+N for a workspace number that doesn't exist must create it on the
    // focused display, not be a no-op.
    let TwoSpaceFixture {
        mut reactor,
        screen1: _,
        screen2: _,
        space1,
        space2,
    } = two_space_fixture();

    // Force lazy init.
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space1);
    let _ = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager_mut()
        .list_workspaces(space2);

    // No display has slot 7 active anywhere, no pin in config, no ws_home_display.
    // Pre-condition: slot 7 unresolvable.
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(7)
            .is_some(),
        "phase 1 still has slot 7 pre-allocated; this assertion will flip in phase 4"
    );

    // After we press Cmd+7, slot 7 must be active on space1 (the focused
    // display in the fixture).
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::SwitchToGlobalSlot(7))));

    let target = reactor
        .layout_manager
        .layout_engine
        .virtual_workspace_manager()
        .resolve_workspace(7)
        .expect("slot 7 must resolve after Cmd+7");
    let active = reactor.layout_manager.layout_engine.active_workspace(target.space);
    assert_eq!(active, Some(target.workspace_id), "slot 7 should be active");
}
```

- [ ] **Step 2: Run, verify it currently passes (because phase 1 still pre-allocates)**

```bash
cargo test --test-threads=1 switch_to_global_slot_creates_workspace_when_absent 2>&1 | tail -10
```

Expected: PASS in phase 1 (pre-allocation makes slot 7 always exist). Will become a real test of create-on-demand in phase 4. Mark this test with a comment to that effect.

- [ ] **Step 3: Commit**

```bash
git add src/actor/reactor/tests.rs
git commit -m "test: scaffold for create-on-demand (passes vacuously in phase 1)"
```

---

### Task 2.2: Cross-display switch focuses the target display

Currently, after `SwitchToGlobalSlot` resolves the target space, the code doesn't move display focus. It only changes the active workspace on the target's space and (in the fast path) focuses a window on that screen. We want explicit display focus when source ≠ target.

**Files:**
- Modify: `src/actor/reactor/events/command.rs:136-180`

- [ ] **Step 1: Read `handle_command_reactor_focus_display` (search for `pub fn handle_command_reactor_focus_display` in command.rs)** to see how display focus is moved.

- [ ] **Step 2: After resolving target, if source display ≠ target display, focus the target display first**

Insert this between the `resolve_workspace` call and the existing fast-path / full-switch logic:

```rust
        // If the target's display is not where the user is, jump focus to
        // the target display BEFORE doing the workspace switch. The
        // subsequent active-workspace change will land on the right space.
        let source_uuid = reactor
            .workspace_command_space()
            .and_then(|space| reactor.display_uuid_for_space(space));
        if source_uuid.as_deref() != Some(&target.display_uuid) {
            Self::handle_command_reactor_focus_display(
                reactor,
                &DisplaySelector::Uuid(target.display_uuid.clone()),
            );
        }
```

(`source_uuid` was removed in Task 1.8; reintroduce it here for the cross-display check.)

- [ ] **Step 3: Build**

```bash
cargo build
```

If `DisplaySelector::Uuid` doesn't exist, find the right variant by reading the `DisplaySelector` enum (`grep -n "enum DisplaySelector" src`).

- [ ] **Step 4: Smoke test in the running WM**

After build + kickstart:
1. Have windows visible on display-1 and display-2
2. Cursor on display-1
3. Press Cmd+1 (assuming slot 1 is on display-2)
4. Cursor and focus should jump to display-2

Expected: focus moves to display-2.

- [ ] **Step 5: Commit**

```bash
git add src/actor/reactor/events/command.rs
git commit -m "vwm: SwitchToGlobalSlot focuses target display before switching"
```

---

### Task 2.3: prev/next workspace cycles per-display only

Currently `NextWorkspace` / `PrevWorkspace` cycle the workspaces of the focused space — which is per-display by default in rift's data model, but only because each display has its own SlotMap of workspaces. After phase 4 this breaks. Lock in the per-display semantics now via a sketch.

**Files:**
- Read: `src/layout_engine/engine.rs:2061-2101` (NextWorkspace, PrevWorkspace handlers)

- [ ] **Step 1: Confirm current behavior is per-space (= per-display) by reading the handlers.** No code change needed in phase 2 — this is just a checkpoint.

- [ ] **Step 2: Add a comment to both handlers documenting the per-display contract**

Above each handler, add:

```rust
// Contract: cycles only across workspaces bound to the focused display.
// Never causes a cross-display jump. See spec: 2026-05-15-hyprland-workspaces-design.md
```

- [ ] **Step 3: Commit**

```bash
git add src/layout_engine/engine.rs
git commit -m "doc: per-display contract for NextWorkspace/PrevWorkspace"
```

---

### Task 2.4: switch_to_last_workspace per-display

Same scoping. Currently per-space, which equals per-display. Document.

**Files:**
- Read: `src/layout_engine/engine.rs:2261-2268` (`SwitchToLastWorkspace` handler)

- [ ] **Step 1: Add the same per-display contract comment above the handler**

- [ ] **Step 2: Commit**

```bash
git add src/layout_engine/engine.rs
git commit -m "doc: per-display contract for SwitchToLastWorkspace"
```

---

**End of Phase 2.** Cross-display focus-jump works. Per-display cycling is documented. The rest of the Hyprland behaviors (create-on-demand, ephemeral) are deferred to phases 3-4 because they require lifecycle changes.

---

## Phase 3 — Ephemeral lifecycle + display lifecycle

Auto-destroy ws when last window removed. Startup: each display gets one default ws (no pre-allocation). Display unplug migrates windows.

### Task 3.1: Switch from per-space pre-allocation to per-display single default

The current `update_settings` and `list_workspaces` functions guarantee `default_workspace_count` (10) workspaces per space. We want one per display at startup.

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Find `list_workspaces` (search for `pub fn list_workspaces`)** — it lazy-creates the per-space slot. Read it to understand what it does.

- [ ] **Step 2: Find the lazy-init code path inside `list_workspaces` and shrink the count**

Currently it likely does something like:
```rust
while workspaces.len() < self.default_workspace_count {
    self.workspaces_by_space.get_mut(&space).unwrap().push(/* new ws */);
}
```

Replace with a single workspace creation, numbered per the rule:
- If this display has a pin in `display_default_workspaces`, use that number
- Else use the next available unused number (smallest non-negative integer not in `workspace_by_number`)

```rust
        // Phase 3: one default workspace per display, numbered:
        //   1. From display_default_workspaces if pinned
        //   2. Else smallest unused WorkspaceNumber across all displays
        let display_uuid = self.display_uuid_for_space.get(&space).cloned();
        let target_num = display_uuid
            .as_ref()
            .and_then(|u| self.display_default_workspaces.get(u).copied())
            .unwrap_or_else(|| {
                let mut n = 0;
                while self.workspace_by_number.contains_key(&n) {
                    n += 1;
                }
                n
            });
        // ... existing creation code, but only ONE workspace, with name
        // matching target_num as a string
```

You'll need to inspect the actual `list_workspaces` (and any `update_settings` lazy-create loop) and adapt the loop to produce exactly one ws per space. Keep `target_num` for the name.

- [ ] **Step 3: Update `update_settings` similarly** — find the `while self.workspaces_by_space.get(&space).unwrap().len() < target_count` loop (currently at line 295) and make it a one-shot.

- [ ] **Step 4: Build**

```bash
cargo build
```

- [ ] **Step 5: Run the test suite**

```bash
cargo test --test-threads=1 2>&1 | tail -40
```

Expected: many tests fail — they assume 10 workspaces per space. We will fix them in subsequent steps. **Don't commit yet.** First triage:

```bash
cargo test --test-threads=1 2>&1 | grep -E "(test .* FAILED|^test result)" | head -30
```

- [ ] **Step 6: Update `switch_to_global_slot_ignored_when_slot_empty` (around tests.rs:1561)**

The test asserts: "a slot beyond the per-space workspace count cannot be resolved." This semantic is replaced by: "a slot for which no ws exists yet creates one on focused display." Mark the test ignored or rewrite for new behavior:

```rust
#[test]
#[ignore = "Phase 3+ creates on demand; replaced by switch_to_global_slot_creates_workspace_when_absent"]
fn switch_to_global_slot_ignored_when_slot_empty() { ... }
```

- [ ] **Step 7: Update `switch_to_global_slot_creates_workspace_when_absent` (Task 2.1)**

Now the precondition assertion `assert!(... .is_some(), "phase 1 still has slot 7 pre-allocated")` is wrong. Flip it:

```rust
    assert!(
        reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(7)
            .is_none(),
        "slot 7 should not exist before pressing Cmd+7"
    );
```

- [ ] **Step 8: Implement create-on-demand in `handle_command_switch_to_global_slot`**

In `command.rs:136-...`, when `resolve_workspace` returns None, create a new ws on the focused display, then re-resolve.

```rust
        let target = match reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(slot)
        {
            Some(t) => t,
            None => {
                // Create on focused display.
                let focused_uuid = reactor
                    .workspace_command_space()
                    .and_then(|space| reactor.display_uuid_for_space(space))
                    .or_else(|| {
                        // Fallback: cursor display, then first online display.
                        reactor
                            .space_for_cursor_screen()
                            .and_then(|sp| reactor.display_uuid_for_space(sp))
                    });
                let Some(uuid) = focused_uuid else {
                    warn!(slot, "SwitchToGlobalSlot: no focused display to create on");
                    return;
                };
                let space = reactor
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager()
                    .space_for_display(&uuid);
                let Some(space) = space else {
                    warn!(slot, "SwitchToGlobalSlot: cannot find space for display");
                    return;
                };
                reactor
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager_mut()
                    .create_workspace_with_number(slot, &uuid, space);
                // Now re-resolve.
                reactor
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager()
                    .resolve_workspace(slot)
                    .expect("just created")
            }
        };
```

- [ ] **Step 9: Implement `space_for_display` and `create_workspace_with_number` on `VirtualWorkspaceManager`**

Add to virtual_workspace.rs:

```rust
    /// Reverse lookup: SpaceId for a display UUID.
    pub fn space_for_display(&self, uuid: &str) -> Option<SpaceId> {
        self.space_for_display_uuid_internal(uuid)
    }

    /// Phase 3: explicit ws creation with a chosen number on a chosen display.
    /// Used by `SwitchToGlobalSlot` create-on-demand.
    pub fn create_workspace_with_number(
        &mut self,
        number: WorkspaceNumber,
        display_uuid: &str,
        space: SpaceId,
    ) -> VirtualWorkspaceId {
        let name = number.to_string();
        let mode = self.resolve_layout_mode_for_workspace(number, &name);
        let ws = VirtualWorkspace::new(name, space, mode, &self.layout_settings);
        let id = self.workspaces.insert(ws);
        self.workspaces_by_space.entry(space).or_default().push(id);
        self.rebuild_new_model_mirrors();
        id
    }
```

- [ ] **Step 10: Re-run tests**

```bash
cargo test --test-threads=1 2>&1 | grep -E "(FAILED|^test result)" | head -30
```

Expected: most tests pass. Triage remaining failures and fix.

- [ ] **Step 11: Smoke test in the running WM**

```bash
rift-build && launchctl kickstart -k gui/$UID/git.acsandmann.rift.hotload
```

Manual:
1. Each display has one ws at startup
2. Cmd+5 from display-1 — creates ws 5 on display-1
3. Cmd+5 from display-2 — focus jumps to display-1 (ws 5 is there)
4. Cmd+8 from display-2 — creates ws 8 on display-2
5. Cmd+5 again — back to display-1, ws 5

- [ ] **Step 12: Commit**

```bash
git add src/model/virtual_workspace.rs src/actor/reactor/events/command.rs src/actor/reactor/tests.rs
git commit -m "vwm: per-display single default ws + create-on-demand for Cmd+N"
```

---

### Task 3.2: Ephemeral — destroy ws when last window removed

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Find `remove_window_from_workspace` (or whichever method handles window removal from a ws)**

```bash
grep -n "remove_window\|window_destroyed" src/model/virtual_workspace.rs | head -20
```

- [ ] **Step 2: Failing test first**

In `tests.rs`:

```rust
#[test]
fn empty_workspace_destroyed_unless_active() {
    let TwoSpaceFixture { mut reactor, space1, .. } = two_space_fixture();
    let _ = reactor.layout_manager.layout_engine.virtual_workspace_manager_mut().list_workspaces(space1);

    // Create ws 7 with one window via SwitchToGlobalSlot + place a window.
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::SwitchToGlobalSlot(7))));
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(99, make_windows(1)));
    let win = WindowId::new(99, 1);

    // Switch back to ws 1, then close the window. ws 7 must be destroyed.
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::SwitchToGlobalSlot(1))));
    reactor.handle_event(Event::WindowDestroyed(win));

    assert!(
        reactor.layout_manager.layout_engine.virtual_workspace_manager().resolve_workspace(7).is_none(),
        "ws 7 should be destroyed after last window removed and not active"
    );
}
```

Run, expect FAIL.

- [ ] **Step 3: Implement destruction**

After window removal in the relevant method:

```rust
        // Ephemeral: destroy ws if it has no windows AND is not active anywhere.
        if let Some(ws) = self.workspaces.get(ws_id) {
            if ws.windows.is_empty() {
                let active_anywhere = self
                    .active_workspace_per_space
                    .values()
                    .any(|(_, active)| *active == ws_id);
                if !active_anywhere {
                    self.destroy_workspace(ws_id);
                }
            }
        }
```

Add `destroy_workspace`:

```rust
    fn destroy_workspace(&mut self, ws_id: VirtualWorkspaceId) {
        let Some(ws) = self.workspaces.remove(ws_id) else { return; };
        if let Some(ids) = self.workspaces_by_space.get_mut(&ws.space) {
            ids.retain(|id| *id != ws_id);
        }
        self.window_to_workspace.retain(|_, id| *id != ws_id);
        self.rebuild_new_model_mirrors();
    }
```

- [ ] **Step 4: Run test, expect PASS**

- [ ] **Step 5: Commit**

```bash
git add src/model/virtual_workspace.rs src/actor/reactor/tests.rs
git commit -m "vwm: ephemeral ws — destroy when last window removed and not active"
```

---

### Task 3.3: Cmd+Shift+N create-on-demand on source display

**Files:**
- Modify: `src/layout_engine/engine.rs:2136-2249` (`MoveWindowToWorkspace` handler)

- [ ] **Step 1: Read the existing handler** to understand how it resolves the target ws.

- [ ] **Step 2: Failing test in `tests.rs`**

```rust
#[test]
fn move_window_to_workspace_creates_target_on_source_display() {
    let TwoSpaceFixture { mut reactor, space1, .. } = two_space_fixture();
    let _ = reactor.layout_manager.layout_engine.virtual_workspace_manager_mut().list_workspaces(space1);
    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(50, make_windows(1)));
    let win = WindowId::new(50, 1);

    // ws 4 doesn't exist. Move window to ws 4.
    reactor.handle_event(Event::Command(Command::Layout(LayoutCommand::MoveWindowToWorkspace {
        workspace: WorkspaceSelector::Index(4),
        window_id: Some(win),
    })));

    let target = reactor.layout_manager.layout_engine.virtual_workspace_manager().resolve_workspace(4)
        .expect("ws 4 should be created");
    assert_eq!(target.space, space1, "ws 4 should be on the source display");
}
```

Run, expect FAIL.

- [ ] **Step 3: In the handler, when target ws can't be resolved by selector, create on source**

Find the spot where `WorkspaceSelector::Index` is resolved and `None` is returned. Replace the `None` path with:

```rust
                None => {
                    // Source window's display is the source. Create on it.
                    let source_space = self
                        .virtual_workspace_manager()
                        .workspace_for_window(window_space, window_id)
                        .and_then(|ws_id| self.virtual_workspace_manager().workspace_space(ws_id))
                        .unwrap_or(window_space);
                    let source_uuid = self
                        .virtual_workspace_manager()
                        .space_display(source_space)
                        .map(str::to_owned);
                    if let Some(uuid) = source_uuid {
                        self.virtual_workspace_manager_mut().create_workspace_with_number(
                            n,
                            &uuid,
                            source_space,
                        );
                        // Re-resolve — must succeed now.
                        self.virtual_workspace_manager().workspace_by_number_lookup(n)
                    } else {
                        None
                    }
                }
```

(Adjust to the actual method names. You may need to add `workspace_by_number_lookup` as a small accessor returning `Option<VirtualWorkspaceId>` from `workspace_by_number`.)

- [ ] **Step 4: Run test, expect PASS**

- [ ] **Step 5: Smoke test in WM**

After build:
1. Open a window on display-1
2. Cmd+Shift+5 (ws 5 doesn't exist) — window moves into ws 5 (still on display-1)
3. Cursor still on display-1, focus unchanged

- [ ] **Step 6: Commit**

```bash
git add src/layout_engine/engine.rs src/model/virtual_workspace.rs src/actor/reactor/tests.rs
git commit -m "vwm: MoveWindowToWorkspace creates target on source display if absent"
```

---

### Task 3.4: Display unplug migrates windows + frees ws numbers

**Files:**
- Read: `src/actor/reactor.rs` and `src/actor/reactor/events/space.rs` to find display-removal hooks.

- [ ] **Step 1: Find where the reactor learns a display is gone** — likely in space topology updates. Grep for `ScreenParametersChanged` or `display_topology` event handlers.

- [ ] **Step 2: Failing test in `tests.rs`**

```rust
#[test]
fn display_unplug_migrates_windows_to_remaining_display() {
    let TwoSpaceFixture { mut reactor, screen1, screen2, space1, space2 } = two_space_fixture();
    let _ = reactor.layout_manager.layout_engine.virtual_workspace_manager_mut().list_workspaces(space1);
    let _ = reactor.layout_manager.layout_engine.virtual_workspace_manager_mut().list_workspaces(space2);

    let mut apps = Apps::new();
    reactor.handle_events(apps.make_app(60, make_windows(1)));
    let win = WindowId::new(60, 1);
    // Move win to space2 (some way to do this — TODO read the test fixtures to see how)

    // Unplug screen2 by sending a screen_params_event with only screen1.
    reactor.handle_event(screen_params_event(
        vec![screen1.frame],
        vec![Some(space1)],
        vec![],
    ));

    // Window should still be alive, on space1's active ws.
    let active_on_space1 = reactor.layout_manager.layout_engine.active_workspace(space1);
    let ws = reactor.layout_manager.layout_engine.virtual_workspace_manager()
        .workspace_for_window(space1, win)
        .expect("window should be on space1 now");
    assert_eq!(Some(ws), active_on_space1);

    // ws bound to (now-gone) space2: should be cleaned up. Specifically, the
    // count of workspaces should reflect only space1's.
    let nums: Vec<_> = reactor.layout_manager.layout_engine.virtual_workspace_manager()
        .all_workspace_numbers().collect();
    assert!(!nums.is_empty());
    // No ws should be associated with space2 anymore.
    for n in &nums {
        let target = reactor.layout_manager.layout_engine.virtual_workspace_manager()
            .resolve_workspace(*n).unwrap();
        assert_ne!(target.space, space2);
    }
}
```

Add `all_workspace_numbers` accessor to `VirtualWorkspaceManager`:
```rust
    pub fn all_workspace_numbers(&self) -> impl Iterator<Item = WorkspaceNumber> + '_ {
        self.workspace_by_number.keys().copied()
    }
```

Run, expect FAIL.

- [ ] **Step 3: Implement migration**

In whichever event handler responds to display removal, add a call to a new method `migrate_workspaces_off_display`:

```rust
    /// Phase 3: when display `dead_uuid` goes offline, move all of its
    /// workspaces' windows to the active ws of `receiver_uuid`, then destroy
    /// the dead workspaces (freeing their numbers for re-use).
    pub fn migrate_workspaces_off_display(
        &mut self,
        dead_uuid: &str,
        receiver_uuid: &str,
    ) {
        let dead_ws: Vec<VirtualWorkspaceId> = self
            .display_for_workspace
            .iter()
            .filter(|(_, uuid)| uuid.as_str() == dead_uuid)
            .map(|(id, _)| *id)
            .collect();
        let receiver_space = match self.space_for_display_uuid_internal(receiver_uuid) {
            Some(s) => s,
            None => return,
        };
        let receiver_ws = match self.active_workspace_per_space.get(&receiver_space) {
            Some((_, active)) => *active,
            None => return,
        };
        for dead_id in dead_ws {
            let windows: Vec<WindowId> = self
                .workspaces
                .get(dead_id)
                .map(|ws| ws.windows().collect())
                .unwrap_or_default();
            for win in windows {
                if let Some(ws) = self.workspaces.get_mut(receiver_ws) {
                    ws.add_window(win);
                }
                // Update window_to_workspace; the key includes a space, which
                // is changing. Rewrite the entry.
                self.window_to_workspace.retain(|(_, w), _| *w != win);
                self.window_to_workspace.insert((receiver_space, win), receiver_ws);
            }
            self.destroy_workspace(dead_id);
        }
    }
```

Wire it into the display-removal hook. The receiver should be the focused remaining display (per spec). For now, pick the first remaining display by display index for simplicity; if needed, refine later.

- [ ] **Step 4: Run test, expect PASS**

- [ ] **Step 5: Smoke test**

1. Open windows on both displays
2. Disconnect secondary
3. Windows from secondary appear on main, on its active ws
4. Reconnect secondary
5. Secondary comes up empty with a fresh default ws

- [ ] **Step 6: Commit**

```bash
git add src/model/virtual_workspace.rs src/actor/reactor.rs src/actor/reactor/events/space.rs src/actor/reactor/tests.rs
git commit -m "vwm: display unplug migrates windows + frees ws numbers"
```

---

**End of Phase 3.** Lifecycle is correct: ws are ephemeral, displays come and go cleanly.

---

## Phase 4 — Storage refactor + cleanup

Now that the new model is the actual source of truth (used by all behavior), drop the legacy fields and migrate `window_to_workspace` to `WindowId`-only key.

### Task 4.1: Make `workspace_by_number` the source of truth

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Audit all reads of `workspaces_by_space`**

```bash
grep -n "workspaces_by_space" src/model/virtual_workspace.rs
```

For each read that powers an external query (e.g. `list_workspaces`, `workspace_index`, etc.), rewrite it to read from `workspace_by_number` filtered by display. Some examples:

```rust
    pub fn list_workspaces(&mut self, space: SpaceId) -> Vec<VirtualWorkspaceId> {
        let Some(uuid) = self.display_uuid_for_space.get(&space).cloned() else {
            return vec![];
        };
        // Lazy-init the display's default ws if absent.
        if !self.has_workspace_for_display(&uuid) {
            self.ensure_default_workspace_for_display(&uuid, space);
        }
        let mut nums: Vec<WorkspaceNumber> = self
            .display_for_workspace
            .iter()
            .filter(|(_, u)| u.as_str() == uuid)
            .filter_map(|(id, _)| {
                self.workspace_by_number.iter()
                    .find_map(|(n, wsid)| (*wsid == *id).then_some(*n))
            })
            .collect();
        nums.sort();
        nums.into_iter().filter_map(|n| self.workspace_by_number.get(&n).copied()).collect()
    }
```

You'll need helpers `has_workspace_for_display` and `ensure_default_workspace_for_display`.

- [ ] **Step 2: Build, run tests**

```bash
cargo test --test-threads=1 2>&1 | tail -30
```

Iterate until passing.

- [ ] **Step 3: Commit**

```bash
git add src/model/virtual_workspace.rs
git commit -m "vwm: workspace_by_number is source of truth for list/lookup"
```

---

### Task 4.2: Drop `rebuild_new_model_mirrors` and the legacy fields

Now that nothing reads `workspaces_by_space`, `slot_displays`, `ws_home_display`, `active_workspace_per_space` from outside the rebuild, we can delete the rebuild and the fields.

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Remove `rebuild_new_model_mirrors` and all its call sites**

```bash
grep -n "rebuild_new_model_mirrors" src/model/virtual_workspace.rs
```

Delete the helper and every call.

- [ ] **Step 2: Update `set_active_workspace` to write directly to `active_workspace_per_display`**

```rust
    pub fn set_active_workspace(
        &mut self,
        space: SpaceId,
        ws_id: VirtualWorkspaceId,
    ) -> Option<()> {
        let uuid = self.display_uuid_for_space.get(&space)?.clone();
        let number = self.workspace_by_number.iter()
            .find_map(|(n, id)| (*id == ws_id).then_some(*n))?;
        let prev = self.active_workspace_per_display.insert(uuid.clone(), number);
        if let Some(p) = prev {
            if p != number {
                self.last_workspace_per_display.insert(uuid, p);
            }
        }
        Some(())
    }
```

- [ ] **Step 3: Delete the legacy fields from the struct**

Remove from `VirtualWorkspaceManager`:
- `workspaces_by_space`
- `active_workspace_per_space`
- `ws_home_display`
- `slot_displays`
- `default_workspace_count`
- `default_workspace_names`
- `default_workspace`

Remove from `VirtualWorkspaceSettings`:
- `default_workspace_count`
- `workspace_names`
- `default_workspace`
- `slot_displays`

Update the constructor to drop the cloned references.

- [ ] **Step 4: Delete the legacy methods**

- `resolve_slot_target`
- `record_slot_home`
- `slot_workspace`
- `workspace_slot`
- `occupied_slots`
- `display_uuid_with_slot_active`
- `display_uuid_with_slot_windows`
- `build_slot_target`
- `space_for_display_uuid_internal` (replaced by `space_for_display`)

- [ ] **Step 5: Cargo check, fix call sites**

```bash
cargo check 2>&1 | tail -50
```

For each call site of a removed method/field, replace with the new equivalent. The compiler will guide you.

- [ ] **Step 6: Run tests**

```bash
cargo test --test-threads=1 2>&1 | tail -40
```

- [ ] **Step 7: Update user config (warn user and migrate)**

User's `~/.config/rift/config.toml` still has `slot_displays = { 1 = "..." }`. Since we deleted the field, deserialization will fail unless we keep a compat shim. Two options:

A. Add an `#[serde(deserialize_with = "...")]` shim on `display_default_workspaces` that also accepts the old `slot_displays` key (more code).

B. Hard-break, tell user to migrate manually.

Pick **B** since this is the user's personal fork and they're driving the change.

- [ ] **Step 8: Commit (still local)**

```bash
git add src/model/virtual_workspace.rs src/common/config.rs src/actor/reactor/events/command.rs src/layout_engine/engine.rs
git commit -m "vwm: drop legacy slot-pool data structures and methods"
```

---

### Task 4.3: Simplify `window_to_workspace` key from `(SpaceId, WindowId)` to `WindowId`

**Files:**
- Modify: `src/model/virtual_workspace.rs`

- [ ] **Step 1: Change the field type**

```rust
    pub window_to_workspace: HashMap<WindowId, VirtualWorkspaceId>,
```

- [ ] **Step 2: Cargo check, fix all call sites**

```bash
cargo check 2>&1 | tail -50
```

For each `(space, win)` lookup, drop `space`. The display binding lives on the workspace, not on the window's space identifier.

- [ ] **Step 3: Update `workspace_for_window` signature**

```rust
    pub fn workspace_for_window(&self, _space: SpaceId, win: WindowId) -> Option<VirtualWorkspaceId> {
        // _space kept for ABI compat with callers that still pass it; ignored.
        self.window_to_workspace.get(&win).copied()
    }
```

(Or drop the `space` param entirely if you also update all callers — preferred, but more diff.)

- [ ] **Step 4: Run tests**

- [ ] **Step 5: Commit**

```bash
git add src/model/virtual_workspace.rs
git commit -m "vwm: drop SpaceId from window_to_workspace key"
```

---

**End of Phase 4.** Internal data model matches the spec. `virtual_workspace.rs` should be substantially smaller (target: <2000 lines, down from 2554).

---

## Phase 5 — CLI + sketchybar + user config

### Task 5.1: Add `--display-uuid` to `rift-cli query workspaces`

**Files:**
- Modify: `src/bin/rift-cli.rs:60-65` (CLI arg)
- Modify: `src/ipc/protocol.rs:8-10` (request shape)
- Modify: `src/ipc.rs:261-263` (handler dispatch)
- Modify: `src/actor/reactor/query.rs:155-156` (query backend)

- [ ] **Step 1: CLI parser (rift-cli.rs)**

Replace the existing `Workspaces` variant:

```rust
    Workspaces {
        #[arg(long, conflicts_with = "display_uuid")]
        space_id: Option<u64>,
        #[arg(long, conflicts_with = "space_id")]
        display_uuid: Option<String>,
    },
```

- [ ] **Step 2: Protocol (protocol.rs)**

```rust
    GetWorkspaces {
        space_id: Option<u64>,
        display_uuid: Option<String>,
    },
```

- [ ] **Step 3: Handler (ipc.rs ~line 261)**

Update dispatch to forward `display_uuid` to `reactor.query_workspaces()`.

- [ ] **Step 4: Query backend (query.rs ~line 155)**

```rust
fn handle_workspace_query(
    &self,
    space_id_param: Option<u64>,
    display_uuid_param: Option<String>,
) -> Vec<WorkspaceData> {
    let space = match (space_id_param, display_uuid_param) {
        (Some(id), _) => Some(SpaceId::new(id)),
        (None, Some(uuid)) => self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .space_for_display(&uuid),
        (None, None) => self.workspace_command_space(),
    };
    // ... rest unchanged
}
```

- [ ] **Step 5: Build, smoke test**

```bash
cargo build --release && cp target/release/rift-cli ~/bin/rift-cli
~/bin/rift-cli query displays
~/bin/rift-cli query workspaces --display-uuid <UUID>
```

Expected: returns workspaces for that display.

- [ ] **Step 6: Commit**

```bash
git add src/bin/rift-cli.rs src/ipc/protocol.rs src/ipc.rs src/actor/reactor/query.rs
git commit -m "cli: query workspaces accepts --display-uuid"
```

---

### Task 5.2: Update sketchybar plugin to use --display-uuid + show only existing ws

**Files:**
- Modify: `~/.config/sketchybar/plugins/rift.sh`
- Modify: `~/.config/sketchybar/sketchybarrc`

- [ ] **Step 1: Update `rift.sh` to query by UUID instead of space-id**

Read current state:
```bash
cat ~/.config/sketchybar/plugins/rift.sh
```

In `render_display`, change:
```bash
rift_data=$(~/bin/rift-cli query workspaces --space-id "$space_id" 2>/dev/null)
```
to:
```bash
rift_data=$(~/bin/rift-cli query workspaces --display-uuid "$display_uuid" 2>/dev/null)
```

Update the function signature and the call sites in the dispatch logic at the bottom.

- [ ] **Step 2: Toggle `drawing` based on existence**

In the python parsing, also emit:
```python
print(f'WS_{name}_EXISTS=1')
```

In bash, default `EXISTS=0` and use it to toggle drawing per item:
```bash
if [ "$EXISTS" = "1" ]; then
  args+=(--set "space.${display_idx}.${sid}" drawing=on ...)
else
  args+=(--set "space.${display_idx}.${sid}" drawing=off)
fi
```

- [ ] **Step 3: Fix the click_script in `sketchybarrc` (it uses `--index $sid` which is wrong)**

```bash
click_script="~/bin/rift-cli execute workspace switch $sid"
```

(positional arg, not flag)

- [ ] **Step 4: Reload sketchybar**

```bash
sketchybar --reload
```

Verify: each display's bar shows only existing ws. Empty workspace numbers are hidden.

- [ ] **Step 5: Commit (these are dotfiles, not in this repo — skip git)**

Just save the files. No commit needed in `rift` repo.

---

### Task 5.3: Migrate `~/.config/rift/config.toml`

**Files:**
- Modify: `~/.config/rift/config.toml`

- [ ] **Step 1: Replace `slot_displays = { 1 = "..." }` with the new key**

```toml
display_default_workspaces = { "30999A24-1D87-4975-983A-6CEAB8B93C8A" = 1 }
```

- [ ] **Step 2: Remove obsolete keys** (delete these lines if present):

```toml
default_workspace_count = 10
workspace_names = ["0", "1", ..., "9"]
default_workspace = ...
```

- [ ] **Step 3: Verify rift reloads cleanly**

```bash
rift-build && launchctl kickstart -k gui/$UID/git.acsandmann.rift.hotload
~/bin/rift-cli query workspaces --display-uuid "<MAIN-UUID>"
~/bin/rift-cli query workspaces --display-uuid "<SECONDARY-UUID>"
```

Expected: each shows the default ws for its display.

- [ ] **Step 4: Final acceptance test**

Manual cycle:
1. Cmd+1, Cmd+2, ... Cmd+9 — each creates a ws on the focused display
2. Switch displays, repeat — works
3. Cmd+1 from elsewhere — focus jumps to ws 1's display
4. Close all windows on a ws, switch away, switch back — ws gets a fresh default
5. Unplug secondary, replug, Cmd+1 — still lands on whichever display has ws 1

- [ ] **Step 5: Commit any final code adjustments**

```bash
cd /Users/dash/projects/rift
git status
# If anything in src/ is dirty:
git add -A
git commit -m "wrap up Hyprland workspace refactor"
```

---

## Self-review checklist (post-implementation)

- [ ] Spec requirements covered (skim each section, point to a task)
- [ ] No placeholders / TBDs in committed code
- [ ] Type names consistent across tasks (e.g. `WorkspaceNumber`, `display_for_workspace`)
- [ ] Test count net-positive (added > removed)
- [ ] Manual smoke at end of each phase passed
- [ ] `cargo clippy` clean (or no NEW warnings)
- [ ] Final `git log --oneline main..HEAD` reads as a coherent story
