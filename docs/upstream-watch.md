---
remote: upstream
branch: main
last_observed: 74ce00dd8cb6cf15cba3ef7459af70ddff51a562
observed_at: 2026-08-27
---

# Upstream Rift Watch

## Scope

- Keep: event-tap recovery; macOS write reliability; richer app rules; typed
  IPC ideas; identity-safe persistence; diagnostics; relevant performance work.
- Ignore: scrolling/master-stack layouts; removed compatibility commands;
  `SpaceId`-owned virtual workspaces.

## Pending candidates

- `a39581e838fdce33981b4452272388fc49eac981`, `e546861f6899384288940dc76dce5bbcd2fc70eb` — reference typed IPC commands, queries, and events — paths: `crates/rift-protocol/`, `src/ipc.rs` — status: review
- `c096f26fa440ac697962aaeb76be55bbb3076dbf` — reference restore matching without persisting `SpaceId` — paths: `src/layout_engine/engine/persistence.rs` — status: review

## Adopted

- `2484722dd989a434d9c994618816003278e14b1c` — removed ineffective WindowServer update suppression from animations; Lift also uses wall-clock frame catch-up and avoids resize writes for position-only movement — status: adapted
- `74ce00d` — preserve double-quoted command arguments — status: adapted to Lift's grouped command parser while retaining existing escape behavior
- `a99c898` — restrict focus-follows-mouse targets to regular layer-zero windows — status: adapted to Lift's WindowServer-backed hover path
- `fad6498` — suppress spurious app activation while waking from loginwindow — status: adapted to Lift's lifecycle activation gate
- `b84dda4` — keep AX observer callback contexts alive through queued notifications — status: adapted with Arc-owned callbacks and observer-lifetime subscription contexts
- `38572a0` — recreate invalidated event taps — status: adapted with Mach-port invalidation recovery, generation guards, and Lift's existing watchdog reconciliation
- `5e10972` — retry failed AX position writes once after verifying the observed frame — status: adapted across direct, batch, and final animation writes
- `54ac143` / `6c64d8b` — configurable centered/sized manual floating and initial app-rule position, size, and focus — status: adapted to Core-owned floating frames and first-assignment rules
- `7d026c4` — `layout_changed` IPC subscriptions — status: adapted to committed BSP snapshots with groups and frames
- `38eacff` — logical window query position and ordering — status: adapted as BSP `group_index` / `window_index` instead of column / row

## Last review

- Range: `be8afef6036c77b67b4c49725ced6414601d63b0..74ce00dd8cb6cf15cba3ef7459af70ddff51a562`
- Result: Reliability fixes and Lift-relevant window behavior were selectively adapted; typed IPC and identity-safe persistence remain candidates, and no upstream merge is planned.
