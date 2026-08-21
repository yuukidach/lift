# rift — Claude Code Working Notes

This repo is a Rust macOS tiling window manager (personal fork of `acsandmann/rift`).
`main` is trunk; the `upstream` remote is reference-only.

## Build and reload

Changes don't take effect in the running WM until you rebuild AND kickstart the
launchd service:

```bash
/Users/dash/bin/rift-build                                    # cargo build --release + install + codesign
launchctl kickstart -k gui/$(id -u)/git.acsandmann.rift       # reload service
launchctl list | grep -i rift                                 # confirm PID changed
```

`rift-build` may print `Bootstrap failed: 5: Input/output error` when the
service was already loaded — benign. The codesign identifier
`git.acsandmann.rift` is intentional (macOS TCC accessibility permissions
persist across rebuilds); do not rename it.

## Tests and checks

```bash
cargo test --lib -- --test-threads=1                          # serial; some tests share state
cargo check                                                   # type/borrow check
```

Baseline as of Phase 4 completion: `240 passed; 0 failed; 1 ignored`.

## Workspace data model (post-Phase-4 redesign — 2026-05)

The virtual workspace manager (`src/model/virtual_workspace.rs`) was rewritten
from a per-space slot pool to a **Hyprland-style per-display model**. The
source of truth is four HashMaps:

| Field | Key | Value | Purpose |
| --- | --- | --- | --- |
| `workspace_by_number` | `WorkspaceNumber` (1..=10) | `VirtualWorkspaceId` | Global Cmd+N → which workspace |
| `display_for_workspace` | `VirtualWorkspaceId` | display UUID | Workspace's bound display |
| `active_workspace_per_display` | display UUID | `WorkspaceNumber` | Per-display active workspace |
| `last_workspace_per_display` | display UUID | `WorkspaceNumber` | Per-display previous (for switch-to-last) |

Plus the SlotMap `workspaces: SlotMap<VirtualWorkspaceId, VirtualWorkspace>`
where each workspace owns its `.space: SpaceId` and `.number: WorkspaceNumber`.

`window_to_workspace: HashMap<WindowId, VirtualWorkspaceId>` — flat. A window
has at most one workspace; derive its SpaceId via the workspace.

**Invariants:**

- One workspace per (number, display) — `workspace_by_number` enforces global single-owner
- Workspace's `.space` is the authoritative space ownership; everything else mirrors
- Display UUIDs survive replug (macOS reissues SpaceId, not display UUID); SpaceIds do not
- A display binding stays sticky while that display is online. When a display
  disappears, rebind its workspaces to the selected online receiver in place:
  preserve their IDs, global numbers, window membership, layouts, and receiver
  active/last state; never reclaim them when the display reappears
- Workspaces are created lazily (one default per display) and destroyed when last window leaves AND not active anywhere

## Destroy helper trio — pick carefully

Three workspace-destruction paths exist with different contracts. Wrong choice
silently corrupts the active/last tables.

| Helper | Active-anywhere guard? | Scrubs active/last tables? | Use when |
| --- | --- | --- | --- |
| `destroy_workspace_no_rebuild(ws_id)` | No | No | You've already verified safety AND already scrubbed the per-display tables yourself. **Rare.** |
| `destroy_workspace_purge_active(ws_id)` | No (forces destruction) | Yes | `remap_space` and other paths that must destroy regardless of active state. Display removal rebinds workspaces instead. |
| `destroy_workspace_if_ephemeral(ws_id)` / `destroy_ephemeral_workspaces(iter)` | Yes (refuses if active) | Yes (via no_rebuild + ephemeral guard) | Normal lifecycle — window removed, may or may not be safe to destroy. **Default choice.** |

If you're tempted to call `destroy_workspace_no_rebuild` directly, you almost
certainly want `purge_active` or the ephemeral path instead.

## Synthetic UUID convention

When `set_active_workspace(space, ws)` runs before `set_space_display(space,
real_uuid)` (callers may activate before display discovery), the manager uses
`__space_{space_id}` as a placeholder UUID in `display_for_workspace` /
`active_workspace_per_display` / `last_workspace_per_display`. When the real
UUID arrives via `set_space_display`, the manager rewrites all entries
keyed by the synthetic UUID to the real one.

Implications:
- Synthetic UUIDs start with `__` (real macOS UUIDs look like `30999A24-1D87-...`); the prefix is collision-proof
- `set_space_display(space, None)` defensively scrubs orphans from the three downstream tables when no other space references the prior UUID

## Recently deleted (do not look for these)

Phase 4 removed the legacy slot-pool model. If you see these names in older
plan docs, prior conversations, or upstream code, they're gone:

- Fields: `workspaces_by_space`, `active_workspace_per_space`, `ws_home_display`, `slot_displays`, `default_workspace_count`, `default_workspace_names`, `default_workspace`
- Methods: `resolve_slot_target`, `record_slot_home`, `slot_workspace`, `workspace_slot`, `occupied_slots`, `display_uuid_with_slot_active`, `display_uuid_with_slot_windows`, `build_slot_target`, `space_for_display_uuid_internal`, `rebuild_new_model_mirrors`, `workspace_for_window_any`, `workspaces_for_window`
- Config: `slot_displays`, `default_workspace_count`, `workspace_names`, `default_workspace` (in `VirtualWorkspaceSettings`); replaced by `display_default_workspaces: HashMap<String, WorkspaceNumber>`

`workspace_for_window` now takes `(WindowId)` only — no SpaceId.

## Where work lives

- `docs/superpowers/specs/` — design docs (input to plans)
- `docs/superpowers/plans/` — implementation plans (current: `2026-05-15-hyprland-workspaces.md`)
- `src/model/virtual_workspace.rs` — the model rewritten in Phase 4
- `src/layout_engine/engine.rs` — wiring between virtual workspaces and the tile layout
- `src/actor/reactor/` — event loop; `events/command.rs` handles user commands
- `src/sys/screen.rs` — SpaceId / display UUID abstractions over CGSpaces

## Workflow conventions

- Subagent-Driven Development: per-task implementer + two-stage review (spec
  compliance → code quality). Plans use `- [ ]` checkbox steps.
- Do not skip hooks (`--no-verify`) on commits
- Do not amend published commits; create new ones for follow-up fixes
- Commit message convention: `<area>: <imperative summary>` (e.g.
  `vwm: drop SpaceId from window_to_workspace key`)
