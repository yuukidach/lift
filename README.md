# Lift

Lift is a lightweight macOS tiling window manager derived from [Rift](https://github.com/acsandmann/rift). It keeps a focused BSP workflow, global numbered workspaces, multi-display support, and a native menu bar indicator.

## Features

- BSP tiling, directional focus and movement, Hyprland-style modal resizing, grouping, swapping, floating, fullscreen, gaps, animations, gestures, and drag-to-swap.
- Ten global workspaces numbered `0`–`9` and ordered `1`–`9`, then `0`, with independent active and previous workspaces per display; `Cmd+-` toggles an unnumbered hidden scratchpad workspace and `Cmd+Shift+-` sends the current window there.
- Identity-safe display migration, hot-reloaded app rules, persistent workspace state, native Mission Control and Stack Line overlays, CLI/IPC subscriptions, and bounded diagnostics.
- A native menu bar indicator that can show workspace numbers or layout previews, active state, and the primary app icon.

Lift uses private macOS APIs but does not require disabling System Integrity Protection.

## Install

Lift requires macOS with “Displays have separate Spaces” enabled, Accessibility permission, Rust, and the Xcode command-line tools.

```bash
git clone https://github.com/yuukidach/lift.git
cd lift
mkdir -p ~/.config/lift
cp lift.default.toml ~/.config/lift/config.toml
scripts/setup-local-signing.sh
scripts/install-local.sh
```

The signing setup runs once. Later installs reuse the same local identity, so macOS normally keeps the existing Accessibility authorization. Universal ad-hoc-signed `lift` and `lift-cli` binaries are also attached to each [GitHub release](https://github.com/yuukidach/lift/releases), but the source install above is recommended when preserving authorization across upgrades matters.

## Menu bar

Enable the native indicator under `settings.ui.menu_bar`. `workspace_scope = "per_display"` is the default and shows only the current display's workspaces without a separator; `workspace_scope = "global"` combines every display and separates their groups.

If the workspace indicator and other menu extras exceed the available width, use [Ice](https://github.com/jordanbaird/Ice) to hide less important items while keeping Lift visible:

```bash
brew install --cask jordanbaird-ice
```

## Usage

Lift reads `~/.config/lift/config.toml`; [lift.default.toml](lift.default.toml) documents every option.

```bash
lift --help
lift-cli --help
```

Diagnostics are stored as a compact, title-free JSONL history and rotate at three 4 MiB files by default:

```bash
lift-cli diagnostics path
lift-cli diagnostics tail --lines 50
```

See [architecture.md](architecture.md) for the implementation boundaries and [docs/upstream-watch.md](docs/upstream-watch.md) for the compact upstream feature-review record.

## License

Lift selectively adapts useful upstream changes instead of merging wholesale. Existing attribution and license history are retained in [LICENSE](LICENSE); Rift began as a fork of [glide-wm](https://github.com/glide-wm/glide).
