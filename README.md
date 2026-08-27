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

## Configuration

Lift reads `~/.config/lift/config.toml` and reloads changes when `settings.hot_reload = true`. Copy [lift.default.toml](lift.default.toml) for the complete option and command reference.

| Section | Purpose |
| --- | --- |
| `[settings]` | Focus, animation, diagnostics, startup commands |
| `[settings.layout]` | Gaps and per-display layout overrides |
| `[settings.ui.menu_bar]` | Native menu bar indicator |
| `[virtual_workspaces]` | Workspace, display, and app placement rules |
| `[modifier_combinations]` | Reusable modifier aliases |
| `[keys]` | Shortcut-to-command mappings |

Keys use `Cmd`, `Alt`, `Ctrl`, and `Shift`; `Meta` is an alias for `Cmd`. Values are either a command name or a command with arguments:

```toml
[virtual_workspaces]
app_rules = [
  { app_id = "com.tencent.xinWeChat", workspace = 8 },
  { app_id = "com.electron.lark", workspace = 9, floating = true, position = { x = 0.5, y = 0.5 }, size = { w = 1100, h = 760 }, focus = true },
]

[keys]
"Cmd + 1" = { switch_to_workspace = 1 }
"Cmd + Shift + 1" = { move_window_to_workspace = 1 }
"Cmd + Left" = { move_focus = "left" }
"Cmd + Shift + Left" = { move_node = "left" }
"Cmd + Shift + Space" = { toggle_window_floating = { center = true, size = "smart" } }
"Cmd + Enter" = { exec = ["/usr/bin/open", "-a", "Terminal"] }
```

App workspace rules apply when a window is first assigned; moving it manually takes precedence. Floating rules may set normalized `position`, logical-point `size`, and one-shot `focus`. Use `lift-cli query applications` for bundle IDs and `lift-cli query displays` for display UUIDs.

## Default shortcuts

| Shortcut | Action |
| --- | --- |
| `Alt+H/J/K/L` | Focus left/down/up/right |
| `Alt+Shift+H/J/K/L` | Move window left/down/up/right |
| `Alt+0…3` | Switch to workspace 0–3 |
| `Alt+Shift+0…3` | Move window to workspace 0–3 |
| `Alt+Tab` | Return to the previous workspace |
| `Cmd+-` | Toggle the hidden workspace |
| `Cmd+Shift+-` | Move the current window to the hidden workspace |
| `Alt+Shift+Arrow` | Join with the window in that direction |
| `Alt+/` / `Alt+Ctrl+E` | Toggle orientation / unjoin |
| `Alt+Shift+Space` | Toggle floating |
| `Alt+Ctrl+Shift+Space` | Temporarily focus the floating layer |
| `Alt+F` / `Alt+Shift+F` | Toggle fullscreen / fullscreen within gaps |
| `Alt+R` | Enter resize mode; use arrows, then `Esc` or `Enter` |
| `Alt+Z` | Toggle Lift management for the current macOS Space |
| `Alt+Enter` | Open Terminal |
| `Alt+Shift+D` / `Alt+Ctrl+S` | Print layout debug data / serialize state |
| `Alt+Ctrl+Q` | Save state and exit |

All workspace commands accept digits `0`–`9`; the bundled file binds 0–3 as examples. Edit `[keys]` to replace or extend any shortcut.

## CLI and diagnostics

```bash
lift --help
lift-cli --help
lift-cli diagnostics tail --lines 50
```

Diagnostics are title-free and rotate at three 4 MiB files by default. Run `lift-cli diagnostics path` to locate them.

See [architecture.md](architecture.md) for the implementation boundaries and [docs/upstream-watch.md](docs/upstream-watch.md) for the compact upstream feature-review record.

## License

Lift selectively adapts useful upstream changes instead of merging wholesale. Existing attribution and license history are retained in [LICENSE](LICENSE); Rift began as a fork of [glide-wm](https://github.com/glide-wm/glide).
