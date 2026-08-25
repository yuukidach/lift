# Lift

Lift is a focused macOS tiling window manager derived from [Rift](https://github.com/acsandmann/rift). It keeps the features used by this fork while simplifying the architecture and removing unused layouts and workflows.

## Features

- BSP tiling with directional focus, movement, resizing, grouping, swapping, constraints, and configurable gaps.
- Ten global virtual workspaces numbered `0`–`9` and ordered as `1`–`9`, then `0`, with per-display active/last state and cross-display window movement.
- Multi-display and native Spaces support with identity-safe migration when displays change or macOS reuses Space IDs.
- Floating and fullscreen windows, focus and pointer controls, drag-to-swap, animations, gestures, and haptics.
- Hot-reloaded application rules, a native menu bar indicator with per-display workspace groups, independent active highlights, and primary App icons, optional Mission Control UI, Stack Line, and grouped-window indicators.
- `lift-cli`, Mach IPC subscriptions, service management, metrics, recording, replay, and bounded diagnostics.

Lift uses private macOS APIs but does not require disabling System Integrity Protection.

## Requirements

- macOS with “Displays have separate Spaces” enabled.
- Accessibility permission for the installed `lift` binary.
- Rust and the Xcode command-line tools when building from source.

## Install

Create the configuration:

```bash
mkdir -p ~/.config/lift
cp lift.default.toml ~/.config/lift/config.toml
```

Create a persistent local signing identity once, then build, install to `~/bin`, and start the LaunchAgent:

```bash
scripts/setup-local-signing.sh
scripts/install-local.sh
```

Grant Accessibility permission after the first signed install. Later builds installed with the same identity retain the same macOS designated requirement and normally do not need authorization again.

## Usage

The default configuration is [lift.default.toml](lift.default.toml). Lift reads `~/.config/lift/config.toml` unless `--config` specifies another path.

```bash
lift --help
lift-cli --help
```

## Diagnostics

Lift records a compact JSONL history of commands, routing decisions, and workspace/window state without window titles. The default rotation keeps three 4 MiB files.

```bash
lift-cli diagnostics path
lift-cli diagnostics tail --lines 50
```

Limits are configured through `settings.diagnostics`. The tail command also works while the Lift service is stopped.

## Documentation

- [Architecture](architecture.md)
- [Core design](docs/design/2026-08-25-lift-core-architecture-design.md)
- [Migration plans](docs/plans/)
- [Upstream feature watch](docs/upstream-watch.md)

## Upstream and license

Lift selectively adopts useful upstream Rift changes instead of merging upstream wholesale. Existing copyright and license history is retained in [LICENSE](LICENSE); Rift itself began as a fork of [glide-wm](https://github.com/glide-wm/glide).
