# Lift

Lift is a focused macOS tiling window manager derived from
[upstream Rift](https://github.com/acsandmann/rift). Its name means “lite Rift”:
the fork keeps the features used by this project and removes compatibility for
layouts and workflows it does not use.

## Features

- BSP tiling with directional focus, movement, resizing, grouping, swapping,
  constraints, and configurable gaps.
- Global virtual workspaces 1–10 with per-display active/last state and window
  movement between workspaces.
- Multi-display and native Spaces handling, including topology-safe migration
  when displays disconnect or macOS reuses Space IDs.
- Floating and fullscreen windows, focus-follows-mouse, pointer warping/hiding,
  drag-to-swap, animations, gestures, and haptics.
- Menu bar controls, custom Mission Control, Stack Line, and grouped-window
  indicators.
- Application rules with configuration hot reload.
- `lift-cli`, Mach IPC subscriptions, service management, metrics, diagnostics,
  recording, and replay.
- No requirement to disable System Integrity Protection.

Lift requires macOS “Displays have separate Spaces” to be enabled.

## Configuration

The default configuration is [lift.default.toml](lift.default.toml). Lift reads
`~/.config/lift/config.toml` unless `--config` supplies another path.

```bash
mkdir -p ~/.config/lift
cp lift.default.toml ~/.config/lift/config.toml
```

The current command surfaces are visible through:

```bash
lift --help
lift-cli --help
```

## Diagnostic history

Lift keeps a compact JSONL history of user commands, routing decisions, and
observed workspace/window state. Window titles are intentionally omitted. The
default rotation is three 4 MiB files, so the history cannot grow without
bound.

```bash
lift-cli diagnostics path
lift-cli diagnostics tail --lines 50
```

The limits can be changed with `settings.diagnostics` in the configuration.
The `tail` command reads the newest records across rotated files and works even
when the Lift service is stopped.

## Stable local signing

An ad-hoc signature identifies only one exact build, so replacing the binary
can make macOS request Accessibility permission again. For local development,
create a persistent signing identity once and use the guarded installer for
all later builds:

```bash
scripts/setup-local-signing.sh
scripts/install-local.sh
```

The first command creates a non-extractable private key in the login keychain.
Changing to that identity requires one final Accessibility authorization. The
installer then preserves the same designated requirement across builds and
refuses to install a binary whose identity falls back to a per-build `cdhash`.

## Architecture

- [Target core architecture](docs/design/2026-08-25-lift-core-architecture-design.md)
- [Migration plans](docs/plans/)
- [Compact upstream feature watch](docs/upstream-watch.md)
- [Current architecture](architecture.md)

## Upstream and license

Lift selectively studies upstream Rift instead of merging it wholesale. The
fork retains its existing copyright and license history; see [LICENSE](LICENSE).
Rift itself began as a fork of
[glide-wm](https://github.com/glide-wm/glide), and both projects use private
macOS APIs informed by work from projects such as yabai.
