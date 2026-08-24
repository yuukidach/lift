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
