# Hyprland-style dynamic workspaces with per-display binding

**Date:** 2026-05-15
**Status:** design draft, awaiting user approval
**Author:** brainstormed in collaboration

## Problem

The current workspace model pre-allocates 10 slots × N displays = 10N workspaces. The "global slot" hotkeys (Cmd+1..0) resolve to a per-slot display via a 6-step priority chain (`resolve_slot_target` in `src/model/virtual_workspace.rs`):

1. `slot_displays` config pin
2. display whose currently-active workspace IS slot N
3. `ws_home_display` (last display where slot N was switched to)
4. display whose slot N has any windows
5. source display (where the keypress came from)
6. any active display

This priority chain lets a slot "float" across displays based on transient activity. After a display unplug/replug, the new SpaceId reshuffles which candidates are valid, and the priority order can pick the wrong display — producing the symptom: "Cmd+1 stops jumping to the secondary display after replug."

The fix is **not** a smarter priority chain. The fundamental mismatch is that workspaces are not entities with stable display identity — they're indices into per-display arrays. The user wants Hyprland's model:

> Workspaces are dynamic objects. Each is bound to one display at creation. The binding is the source of truth.

## Conceptual model

A **workspace** is a positive integer identifier with:

- **At most one** display binding at any time
- **Ephemeral lifetime**: exists from creation until its last window closes
- **Sticky binding** during its lifetime (immutable while it has windows AND the bound display is still online)
- **No persistence** across rift restarts or display reconnects — "permanent only as long as the display config doesn't change"

A workspace exists ⟺ it has ≥1 window OR it is currently the active workspace of some display.

The pre-allocated slot pool, `default_workspace_count`, `workspace_names`, and `slot_displays` config all go away.

## Behavior

### Cmd+N (`switch_to_workspace N`)

```
ws N exists on focused display     → focus first window of ws N
ws N exists on a different display → focus jumps to that display; ws N becomes its active ws
ws N does not exist                → create ws N on focused display, mark active, focus
```

The ws **never moves** between displays as a result of this command. Cross-display "switch" is really "focus jumps to where the ws lives."

### Cmd+Shift+N (`move_window_to_workspace N`)

```
ws N exists                  → window detaches from its current ws, attaches to ws N.
                               If ws N's bound display ≠ source window's display, the
                               window's *physical position* migrates to ws N's display.
                               ws N's binding is NOT rebound to the source display.
ws N does not exist          → create ws N on the source window's display, move window into it
```

Focus does **not** follow the window across displays. The user stays where they were.

### Cmd+Backquote / Cmd+Tab (`prev_workspace` / `next_workspace`)

Cycles only across workspaces bound to the **focused display**. Skips empty (i.e. non-existent) numbers. Never causes a cross-display jump.

### Cmd+Shift+Tab (`switch_to_last_workspace`)

Switches to the previous workspace of the **focused display**. Per-display history, not global. (Same scoping rule as prev/next.)

### App rules (`app_rules` config)

When a matching app launches:

- If rule's target ws **exists** → window is assigned to that ws on its bound display
- If rule's target ws **does not exist** → ws is created on the **focused display**, window is assigned

This is the same rule as Cmd+N for non-existent ws — the focused display is the implicit "anchor."

## Display lifecycle

### Startup

For each display detected at startup, in display-index order, create one default workspace numbered consecutively starting at 1:

```
display-1 → ws 1 (active)
display-2 → ws 2 (active)
display-3 → ws 3 (active)
...
```

Existing windows are assigned to the default workspace of whichever display they are currently on.

A `display_default_workspaces` config (renamed from `slot_displays`) lets the user override which **default** workspace number each display gets at startup:

```toml
display_default_workspaces = { "30999A24-...-secondary" = 1 }
```

This says "if this UUID is online at startup, its default ws is 1, not the next consecutive number." Other displays get the lowest available unused number. The pin only applies at startup creation — it does not constrain mid-session ws creation, and it does not "pull" ws 1 back if it gets recreated on another display later.

This preserves the intent of the user's existing `slot_displays` setting (`{ 1 = "...secondary" }`) under the new model.

### Display unplug

All windows on the unplugged display migrate to the **currently-active** workspace of the remaining (focused) display. Workspaces bound to the unplugged display lose their identity — the numbers are "freed" and can be re-created later.

If multiple displays remain, the choice of receiving display is the focused one. (Future work: more nuanced redistribution.)

### Display replug

Treated as a brand-new display with a new SpaceId. No state restoration. The replugged display gets a fresh default workspace using the lowest available unused number.

This is intentional: per the user's "permanent only when displays don't change" principle, we don't try to be cleverer than the OS.

## Data model

### Removed

```rust
pub const GLOBAL_WORKSPACE_SLOTS: usize = 10;       // gone
struct VirtualWorkspaceManager {
    workspaces_by_space: HashMap<SpaceId, Vec<VirtualWorkspaceId>>, // gone
    ws_home_display: HashMap<usize, String>,                         // gone
    slot_displays: HashMap<usize, String>,                           // gone
    default_workspace_count: usize,                                  // gone
    default_workspace_names: Vec<String>,                            // gone
    default_workspace: usize,                                        // gone
}
struct VirtualWorkspaceSettings {
    pub default_workspace_count: usize,                              // gone (config)
    pub workspace_names: Vec<String>,                                // gone (config)
    pub slot_displays: HashMap<usize, String>,                       // gone (config)
}
struct SlotTarget { ... }                            // gone (no per_space_index needed)
fn resolve_slot_target(...) -> Option<SlotTarget>    // gone
fn record_slot_home(...)                             // gone
fn slot_workspace(...) / fn workspace_slot(...) / fn occupied_slots(...)  // gone
```

### Added

```rust
pub type WorkspaceNumber = u32;  // 0..=N; user hotkeys cover 0-9 today

struct VirtualWorkspaceManager {
    /// Sole source of truth: number -> ws id (when alive).
    workspace_by_number: HashMap<WorkspaceNumber, VirtualWorkspaceId>,
    /// Sole source of truth: ws id -> bound display UUID (immutable during ws life).
    display_for_workspace: HashMap<VirtualWorkspaceId, String>,
    /// Per-display active workspace number. Replaces active_workspace_per_space.
    active_workspace_per_display: HashMap<String, WorkspaceNumber>,
    /// Per-display "previous" for switch_to_last_workspace.
    last_workspace_per_display: HashMap<String, WorkspaceNumber>,
    workspaces: SlotMap<VirtualWorkspaceId, VirtualWorkspace>,  // unchanged
    window_to_workspace: HashMap<WindowId, VirtualWorkspaceId>, // simplified key (no SpaceId)
    display_uuid_for_space: HashMap<SpaceId, String>,           // unchanged (mirror)
}

struct VirtualWorkspace {
    number: WorkspaceNumber,
    display_uuid: String,
    windows: HashSet<WindowId>,
    last_focused: Option<WindowId>,
    layout_system: LayoutSystemKind,
    layout_mode: LayoutMode,
}
```

### Resolution becomes trivial

```rust
fn resolve(&self, n: WorkspaceNumber) -> Option<(VirtualWorkspaceId, &str)> {
    let id = self.workspace_by_number.get(&n)?;
    let display = self.display_for_workspace.get(id)?;
    Some((*id, display.as_str()))
}
```

No priority chain. No display-state-dependent disambiguation. The `replug bug` cannot recur.

### Lifecycle hooks

- **Create**: `(number, display_uuid)` pair → new `VirtualWorkspaceId`, registered in both maps
- **Destroy** (last window removed AND not active anywhere): drop from both maps, remove ws
- **Display unplug**: iterate `display_for_workspace`, find all bound to dead display, migrate each ws's windows to receiving display's active ws, then destroy

## "Focused display" semantics

Several rules above use "the focused display" as the implicit anchor. Definition:

1. The display containing the currently-focused window, if any
2. Else the display the cursor is over
3. Else the first online display in display-index order

Operations that need a focused display when none is determinable (e.g. cold-start app rule firing before any window has focus) fall through to (3).

## Persistence

The save/restore path (`save_and_exit` → restore on startup, bound to `Cmd+Ctrl+Q`) is retained as a **best-effort** cross-restart courtesy. It does NOT contradict the "no persistence across display config changes" principle:

- Serialize: `workspaces` (each carrying its `number` and `display_uuid`), plus `window_to_workspace`
- Restore: only restore workspaces whose `display_uuid` matches a currently-online display. Drop the rest. Numbers from dropped restored workspaces become available for fresh creation.
- Conflicts (restore wants ws 1 on display A, default-startup also created ws 1 on display A): restore wins — it has windows worth preserving, defaults don't.

If displays change between save and restore, restored state is partially or fully discarded. That's the contract.

## Sketchybar implications

The current `sketchybarrc` pre-allocates 10 items per display (`space.1.1` … `space.1.0`, `space.2.1` … `space.2.0`) and `rift.sh` updates all 10 every event.

With dynamic ws, we can't pre-allocate. Two options:

1. **Pre-allocate the upper bound (e.g. items for ws 0-9 per display)**, hide items for non-existent ws. Simple, but limits to a fixed range. Practical given current hotkeys.
2. **Add/remove items dynamically each render**. More flexible, more flicker risk, more sketchybar churn.

Recommendation: **Option 1**. The hotkey row already constrains the user-facing workspace numbers to 0-9. The bar pre-allocates `space.$display.0` … `space.$display.9` and toggles `drawing=on/off` based on existence. `rift.sh` queries `rift-cli query workspaces --display-uuid <UUID>` and emits the toggle batch.

`rift-cli query workspaces` will need updating to support `--display-uuid` (today it accepts `--space-id`).

## Config breaking changes

Removed config keys (will warn on load if present):

```toml
[virtual_workspaces]
default_workspace_count = 10        # ignored
workspace_names = [...]             # ignored
```

Renamed config key (with semantic narrowing):

```toml
# OLD: rigid pin — slot N's display always resolves to UUID
slot_displays = { 1 = "30999A24-..." }

# NEW: startup-default pin — UUID's default ws number at boot is 1
# (no mid-session enforcement; does not pull ws 1 back if recreated elsewhere)
display_default_workspaces = { "30999A24-..." = 1 }
```

`auto_assign_windows`, `preserve_focus_per_workspace`, `workspace_auto_back_and_forth`, `app_rules` — all retained.

User's current `~/.config/rift/config.toml`:

```toml
slot_displays = { 1 = "30999A24-1D87-4975-983A-6CEAB8B93C8A" }
```

Migrates to:

```toml
display_default_workspaces = { "30999A24-1D87-4975-983A-6CEAB8B93C8A" = 1 }
```

Same intent ("ws 1 lives on secondary at startup"), narrower contract (only at startup, not as a permanent leash).

## Out of scope

- Workspace numbers beyond the hotkey range (no UI to reach them; can extend later)
- Cross-restart binding persistence (intentional simplification)
- Smart redistribution on unplug (e.g. honoring "preferred fallback display")
- Named workspaces (`workspace_names` is gone; pure numeric only)
- `workspace = N, monitor:HDMI-A-1` style config pinning (Hyprland has this; out of scope for v1)

## Test plan (sketch — full plan deferred to writing-plans phase)

- Unit: `workspace_by_number` insertion/removal, display binding immutability, ephemeral GC on last-window-removed
- Integration: Cmd+N across both create-on-focus and focus-jump branches; Cmd+Shift+N across both create-on-source and existing branches; prev/next strictly per-display
- Integration: display unplug migrates windows + frees ws numbers; replug starts clean
- Reproduce the original bug: replug, press Cmd+1 — must always land on whichever display ws 1 is currently bound to (or create on focused display if no ws 1 exists)

## Open questions

None. All key questions resolved during brainstorming. Implementation will surface more.
