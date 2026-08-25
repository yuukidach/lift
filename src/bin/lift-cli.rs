use std::io::{self, Write};
use std::process::{self};

use clap::{Parser, Subcommand, ValueEnum};
use lift::actor::app::WindowId;
use lift::actor::reactor::{self, DisplaySelector};
use lift::common::config::workspace_number_to_global_slot;
use lift::ipc::{LiftCommand, LiftMachClient, LiftRequest, LiftResponse};
use lift::model::layout;
use lift::sys::window_server::WindowServerId;
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "lift-cli",
    version,
    about = "Command-line interface for Lift window manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, ValueEnum)]
enum CliDirection {
    Left,
    Right,
    Up,
    Down,
}

impl From<CliDirection> for layout::Direction {
    fn from(direction: CliDirection) -> Self {
        match direction {
            CliDirection::Left => Self::Left,
            CliDirection::Right => Self::Right,
            CliDirection::Up => Self::Up,
            CliDirection::Down => Self::Down,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Query information from Lift
    Query {
        #[command(subcommand)]
        query: QueryCommands,
    },
    /// Execute commands in Lift
    Execute {
        #[command(subcommand)]
        command: ExecuteCommands,
    },
    /// Event subscription commands
    Subscribe {
        #[command(subcommand)]
        subscribe: SubscribeCommands,
    },
    /// Manage the launchd service for Lift
    Service {
        #[command(subcommand)]
        service: ServiceCommands,
    },
    /// Inspect the local bounded diagnostic history
    Diagnostics {
        #[command(subcommand)]
        diagnostics: DiagnosticsCommands,
    },
}

#[derive(Subcommand)]
enum ServiceCommands {
    /// Install the per-user launchd service
    Install,
    /// Uninstall the per-user launchd service
    Uninstall,
    /// Start (or bootstrap) the service
    Start,
    /// Stop (or bootout/kill) the service
    Stop,
    /// Restart the service (kickstart -k)
    Restart,
}

#[derive(Subcommand)]
enum DiagnosticsCommands {
    /// Print the active diagnostic JSONL file path
    Path,
    /// Print the most recent diagnostic records (works even if Lift is stopped)
    Tail {
        #[arg(long, default_value_t = 50)]
        lines: usize,
    },
}

#[derive(Subcommand)]
enum QueryCommands {
    /// List virtual workspaces (optionally filtered by SpaceId or display UUID)
    Workspaces {
        #[arg(long, conflicts_with = "display_uuid")]
        space_id: Option<u64>,
        #[arg(long, conflicts_with = "space_id")]
        display_uuid: Option<String>,
    },
    /// List windows (optionally filtered by space)
    Windows {
        #[arg(long)]
        space_id: Option<u64>,
    },
    /// List connected displays
    Displays,
    /// Get information about a specific window
    Window { window_id: String },
    /// List running applications
    Applications,
    /// Get layout state for a space
    Layout { space_id: u64 },
    /// Get workspace layout-engine mode(s)
    WorkspaceLayout {
        #[arg(long)]
        space_id: Option<u64>,
        #[arg(long)]
        workspace_id: Option<u64>,
    },
    /// Get performance metrics
    Metrics,
}

#[derive(Subcommand)]
enum ExecuteCommands {
    /// Window management commands
    Window {
        #[command(subcommand)]
        window_cmd: WindowCommands,
    },
    /// Virtual workspace commands
    Workspace {
        #[command(subcommand)]
        workspace_cmd: WorkspaceCommands,
    },
    /// Layout commands
    Layout {
        #[command(subcommand)]
        layout_cmd: LayoutCommands,
    },
    /// Configuration management commands
    Config {
        #[command(subcommand)]
        config_cmd: ConfigCommands,
    },
    /// Mission control commands
    MissionControl {
        #[command(subcommand)]
        mission_cmd: MissionControlCommands,
    },
    /// Display/mouse commands
    Display {
        #[command(subcommand)]
        display_cmd: DisplayCommands,
    },
    /// Save current state and exit Lift
    SaveAndExit,
    /// Print layout tree debugging output in the running Lift instance
    Debug,
    /// Serialize and print runtime state
    Serialize,
    /// Toggle whether the current space is managed by Lift
    ToggleSpaceActivated,
    /// Show timing metrics
    ShowTiming,
}

#[derive(Subcommand)]
enum WindowCommands {
    /// Focus the next window
    Next,
    /// Focus the previous window
    Prev,
    /// Move focus in a direction
    Focus { direction: CliDirection },
    /// Toggle window floating state
    ToggleFloat,
    /// Toggle fullscreen mode (fills the whole screen, ignores outer gaps)
    ToggleFullscreen,
    /// Toggle fullscreen within configured outer gaps (respects outer gaps / fills tiling area)
    ToggleFullscreenWithinGaps,
    /// Grow the current window size (increments by ~5%).
    ResizeGrow,
    /// Shrink the current window size (decrements by ~5%).
    ResizeShrink,
    /// Resize the selected window by a fractional amount.
    /// - Pass a signed floating value: positive to grow, negative to shrink.
    /// - The value is a fraction of the current size (e.g. `0.05` = 5%).
    /// Examples:
    ///   lift-cli execute window resize-by --amount 0.05    # grow by 5%
    ///   lift-cli execute window resize-by --amount -0.10   # shrink by 10%
    ResizeBy { amount: f64 },
    /// Close a window by window server identifier
    Close {
        /// Window Id (window server id or idx from window id)
        #[arg(long)]
        window_id: String,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommands {
    /// Switch to next workspace
    Next { skip_empty: Option<bool> },
    /// Switch to previous workspace
    Prev { skip_empty: Option<bool> },
    /// Switch to a workspace by its digit (0 through 9)
    Switch { workspace_id: usize },
    /// Move a window to a workspace by its digit (0 through 9)
    MoveWindow {
        workspace_id: usize,
        window_id: Option<u32>,
    },
    /// Move a window to the hidden scratchpad workspace on the current display
    MoveWindowHidden { window_id: Option<u32> },
    /// Create a new workspace
    Create,
    /// Switch to the last workspace
    Last,
    /// Toggle the hidden scratchpad workspace on the current display
    ToggleHidden,
}

#[derive(Subcommand)]
enum LayoutCommands {
    /// Move the selected node in a direction
    MoveNode { direction: CliDirection },
    /// Join the selected window with neighbor in a direction
    JoinWindow { direction: CliDirection },
    /// Global orientation toggle that works consistently across layout modes (and between splits/stacks)
    ToggleOrientation,
    /// Unjoin previously joined windows
    Unjoin,
    /// Toggle floating on the focused selection (tree focus)
    ToggleFocusFloat,
    /// Swap two windows by window id (`WindowId { pid: ..., idx: ... }`)
    SwapWindows { a: String, b: String },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Update animation settings
    SetAnimate {
        value: String,
    },
    SetAnimationDuration {
        value: f64,
    },
    SetAnimationFps {
        value: f64,
    },
    SetAnimationEasing {
        value: String,
    },

    /// Update mouse settings
    SetMouseFollowsFocus {
        value: bool,
    },
    SetMouseHidesOnFocus {
        value: bool,
    },
    SetFocusFollowsMouse {
        value: bool,
    },

    SetOuterGaps {
        top: f64,
        left: f64,
        bottom: f64,
        right: f64,
    },
    SetInnerGaps {
        horizontal: f64,
        vertical: f64,
    },

    /// Update workspace settings
    SetWorkspaceNames {
        names: Vec<String>,
    },

    /// Generic set: set an arbitrary config key (dot-separated path) to a JSON value.
    /// Example: lift-cli execute config set --key settings.animate --value true
    Set {
        /// Dot-separated key path (e.g. settings.animate or settings.layout.gaps.outer.top)
        key: String,
        /// Value should be valid JSON (true, 1, "string", {"a":1}), but if it's not valid JSON
        /// it will be treated as a string.
        value: String,
    },

    /// Get current config
    Get,

    /// Save current config to file
    Save,

    /// Reload config from file
    Reload,
}

#[derive(Subcommand)]
enum MissionControlCommands {
    /// Show all workspaces in mission control
    ShowAll,
    /// Show current workspace in mission control
    ShowCurrent,
    /// Dismiss mission control
    Dismiss,
}

#[derive(Subcommand)]
enum DisplayCommands {
    /// Focus a display by direction, index, or UUID.
    Focus {
        /// Direction relative to the current display (left, right, up, down).
        #[arg(long)]
        direction: Option<String>,
        /// Display index (0-based).
        #[arg(long)]
        index: Option<usize>,
        /// Display UUID.
        #[arg(long)]
        uuid: Option<String>,
    },
    /// Move mouse cursor to a display by index (0-based)
    MoveMouseToIndex {
        /// Display index (0-based)
        index: usize,
    },
    /// Move mouse cursor to a display by UUID
    MoveMouseToUuid {
        /// Display UUID
        uuid: String,
    },
    /// Move a window to a display by direction, index, or UUID.
    MoveWindow {
        /// Direction relative to the window's current display (left, right, up, down).
        #[arg(long)]
        direction: Option<String>,
        /// Display index (0-based).
        #[arg(long)]
        index: Option<usize>,
        /// Display UUID.
        #[arg(long)]
        uuid: Option<String>,
        /// Optional window id (window idx); defaults to the focused window if omitted.
        #[arg(long)]
        window_id: Option<u32>,
    },
}

#[derive(Subcommand)]
enum SubscribeCommands {
    /// Subscribe to Mach IPC events
    Mach {
        /// Event to subscribe to (workspace_changed, windows_changed, window_title_changed, stacks_changed, *)
        event: String,
    },
    /// Subscribe to events via CLI command execution
    Cli {
        /// Event to subscribe to (workspace_changed, windows_changed, window_title_changed, stacks_changed, *)
        #[arg(long)]
        event: String,
        /// Command to execute when event occurs
        #[arg(long)]
        command: String,
        /// Arguments to pass to command (event data will be appended as JSON)
        #[arg(long, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Unsubscribe from Mach IPC events
    UnsubMach {
        /// Event to unsubscribe from
        event: String,
    },
    /// Unsubscribe from CLI events
    UnsubCli {
        /// Event to unsubscribe from
        event: String,
    },
    /// List current CLI subscriptions
    ListCli,
}

fn main() {
    sigpipe::reset();
    let cli = Cli::parse();

    let request = match cli.command {
        Commands::Service { .. } => {
            println!(
                "service commands have been moved to the `lift` binary. (for example, `lift service install`)"
            );
            process::exit(0);
        }
        Commands::Diagnostics { diagnostics } => {
            handle_diagnostics_command(diagnostics);
            process::exit(0);
        }
        Commands::Subscribe {
            subscribe: SubscribeCommands::Mach { event },
        } => {
            if let Err(e) = run_mach_subscription(event) {
                eprintln!("Communication error: {}", e);
                eprintln!("Hint: ensure the Lift service is running (try `lift service start`).");
                process::exit(1);
            }
            process::exit(0);
        }
        command => match build_request(command) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Error: {}", e);
                process::exit(1);
            }
        },
    };

    let client = match LiftMachClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to connect to Lift: {}", e);
            process::exit(1);
        }
    };

    // Send request and handle response.
    match client.send_request(&request) {
        Ok(resp) => match resp {
            LiftResponse::Success { data } => {
                if let Err(e) = write_json(
                    &data,
                    std::env::var("LIFT_CLI_PRETTY").map(|v| v != "0").unwrap_or(false),
                ) {
                    eprintln!("Failed to handle response: {}", e);
                    process::exit(1);
                }
            }
            LiftResponse::Error { error } => {
                match serde_json::to_string_pretty(&error) {
                    Ok(pretty) => eprintln!("{}", pretty),
                    Err(_) => eprintln!("Error: {}", error),
                }
                process::exit(1);
            }
            _ => {
                eprintln!("Received an unknown response shape from Lift");
                process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("Communication error: {}", e);
            eprintln!("Hint: ensure the Lift service is running (try `lift service start`).");
            process::exit(1);
        }
    }
}

fn build_request(command: Commands) -> Result<LiftRequest, String> {
    match command {
        Commands::Query { query } => build_query_request(query),
        Commands::Execute { command } => build_execute_request(command),
        Commands::Subscribe { subscribe } => build_subscribe_request(subscribe),
        Commands::Service { .. } => Err(
            "Service commands are handled locally and should not be sent to the Lift server."
                .to_string(),
        ),
        Commands::Diagnostics { .. } => Err(
            "Diagnostics commands are handled locally and should not be sent to the Lift server."
                .to_string(),
        ),
    }
}

fn handle_diagnostics_command(command: DiagnosticsCommands) {
    let path = lift::common::config::diagnostics_file();
    match command {
        DiagnosticsCommands::Path => println!("{}", path.display()),
        DiagnosticsCommands::Tail { lines } => {
            match lift::runtime::diagnostics::tail(&path, lines) {
                Ok(records) => {
                    for record in records {
                        println!("{record}");
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    eprintln!("No diagnostic log exists yet at {}", path.display());
                    process::exit(1);
                }
                Err(error) => {
                    eprintln!("Could not read {}: {error}", path.display());
                    process::exit(1);
                }
            }
        }
    }
}

fn build_query_request(query: QueryCommands) -> Result<LiftRequest, String> {
    match query {
        QueryCommands::Workspaces { space_id, display_uuid } => {
            Ok(LiftRequest::GetWorkspaces { space_id, display_uuid })
        }
        QueryCommands::Windows { space_id } => Ok(LiftRequest::GetWindows { space_id }),
        QueryCommands::Displays => Ok(LiftRequest::GetDisplays),
        QueryCommands::Window { window_id } => Ok(LiftRequest::GetWindowInfo { window_id }),
        QueryCommands::Applications => Ok(LiftRequest::GetApplications),
        QueryCommands::Layout { space_id } => Ok(LiftRequest::GetLayoutState { space_id }),
        QueryCommands::WorkspaceLayout { space_id, workspace_id } => {
            Ok(LiftRequest::GetWorkspaceLayouts { space_id, workspace_id })
        }
        QueryCommands::Metrics => Ok(LiftRequest::GetMetrics),
    }
}

fn build_subscribe_request(sub: SubscribeCommands) -> Result<LiftRequest, String> {
    match sub {
        SubscribeCommands::Mach { event } => Ok(LiftRequest::Subscribe { event }),
        SubscribeCommands::Cli { event, command, args } => {
            Ok(LiftRequest::SubscribeCli { event, command, args })
        }
        SubscribeCommands::UnsubMach { event } => Ok(LiftRequest::Unsubscribe { event }),
        SubscribeCommands::UnsubCli { event } => Ok(LiftRequest::UnsubscribeCli { event }),
        SubscribeCommands::ListCli => Ok(LiftRequest::ListCliSubscriptions),
    }
}

fn build_execute_request(execute: ExecuteCommands) -> Result<LiftRequest, String> {
    let lift_command = match execute {
        ExecuteCommands::Window { window_cmd } => map_window_command(window_cmd)?,
        ExecuteCommands::Workspace { workspace_cmd } => map_workspace_command(workspace_cmd)?,
        ExecuteCommands::Layout { layout_cmd } => map_layout_command(layout_cmd)?,
        ExecuteCommands::Config { config_cmd } => map_config_command(config_cmd)?,
        ExecuteCommands::MissionControl { mission_cmd } => {
            map_mission_control_command(mission_cmd)?
        }
        ExecuteCommands::Display { display_cmd } => map_display_command(display_cmd)?,
        ExecuteCommands::SaveAndExit => {
            LiftCommand::Reactor(reactor::Command::Reactor(reactor::ReactorCommand::SaveAndExit))
        }
        ExecuteCommands::Debug => {
            LiftCommand::Reactor(reactor::Command::Reactor(reactor::ReactorCommand::Debug))
        }
        ExecuteCommands::Serialize => {
            LiftCommand::Reactor(reactor::Command::Reactor(reactor::ReactorCommand::Serialize))
        }
        ExecuteCommands::ToggleSpaceActivated => LiftCommand::Reactor(reactor::Command::Reactor(
            reactor::ReactorCommand::ToggleSpaceActivated,
        )),
        ExecuteCommands::ShowTiming => LiftCommand::Reactor(reactor::Command::Metrics(
            lift::common::log::MetricsCommand::ShowTiming,
        )),
    };

    if let LiftCommand::Config(lift::common::config::ConfigCommand::GetConfig) = &lift_command {
        return Ok(LiftRequest::GetConfig);
    }

    let maybe_config_json = match &lift_command {
        LiftCommand::Config(cfg_cmd) => match serde_json::to_string(cfg_cmd) {
            Ok(s) => Some(s),
            Err(_) => None,
        },
        _ => None,
    };

    let command_str = serde_json::to_string(&lift_command)
        .map_err(|e| format!("Failed to serialize command: {}", e))?;

    if let Some(cfg_json) = maybe_config_json {
        Ok(LiftRequest::ExecuteCommand {
            command: command_str,
            args: vec!["__apply_config__".to_string(), cfg_json],
        })
    } else {
        Ok(LiftRequest::ExecuteCommand {
            command: command_str,
            args: vec![],
        })
    }
}

fn map_window_command(cmd: WindowCommands) -> Result<LiftCommand, String> {
    use layout::LayoutCommand as LC;
    match cmd {
        WindowCommands::Next => Ok(LiftCommand::Reactor(reactor::Command::Layout(LC::NextWindow))),
        WindowCommands::Prev => Ok(LiftCommand::Reactor(reactor::Command::Layout(LC::PrevWindow))),
        WindowCommands::Focus { direction } => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::MoveFocus(direction.into()),
        ))),
        WindowCommands::ToggleFloat => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ToggleWindowFloating,
        ))),
        WindowCommands::ToggleFullscreen => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ToggleFullscreen,
        ))),
        WindowCommands::ToggleFullscreenWithinGaps => Ok(LiftCommand::Reactor(
            reactor::Command::Layout(LC::ToggleFullscreenWithinGaps),
        )),
        WindowCommands::ResizeGrow => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ResizeWindowGrow,
        ))),
        WindowCommands::ResizeShrink => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ResizeWindowShrink,
        ))),
        WindowCommands::ResizeBy { amount } => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ResizeWindowBy { amount },
        ))),
        WindowCommands::Close { window_id } => {
            let wsid = parse_window_server_id(&window_id)?;
            Ok(LiftCommand::Reactor(reactor::Command::Reactor(
                reactor::ReactorCommand::CloseWindow { window_server_id: Some(wsid) },
            )))
        }
    }
}

fn parse_window_server_id(input: &str) -> Result<WindowServerId, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("window_server_id cannot be empty".to_string());
    }

    let value = if trimmed.starts_with("0x") {
        u32::from_str_radix(trimmed.trim_start_matches("0x"), 16)
            .map_err(|_| format!("Invalid hexadecimal window server id: {}", trimmed))?
    } else {
        trimmed.parse().map_err(|_| format!("Invalid window server id: {}", trimmed))?
    };
    Ok(WindowServerId::new(value))
}

fn parse_window_id(input: &str) -> Result<WindowId, String> {
    WindowId::from_debug_string(input.trim()).ok_or_else(|| {
        format!(
            "Invalid window id '{}'; expected `WindowId {{ pid: 123, idx: 456 }}`",
            input
        )
    })
}

fn map_workspace_command(cmd: WorkspaceCommands) -> Result<LiftCommand, String> {
    use layout::LayoutCommand as LC;
    match cmd {
        WorkspaceCommands::Next { skip_empty } => Ok(LiftCommand::Reactor(
            reactor::Command::Layout(LC::NextWorkspace(skip_empty)),
        )),
        WorkspaceCommands::Prev { skip_empty } => Ok(LiftCommand::Reactor(
            reactor::Command::Layout(LC::PrevWorkspace(skip_empty)),
        )),
        WorkspaceCommands::Switch { workspace_id } => {
            let slot = workspace_number_to_global_slot(workspace_id)
                .ok_or_else(|| format!("workspace number must be in 0..=9, got {workspace_id}"))?;
            let cmd = LC::SwitchToGlobalSlot(slot);
            Ok(LiftCommand::Reactor(reactor::Command::Layout(cmd)))
        }
        WorkspaceCommands::MoveWindow { workspace_id, window_id } => {
            let slot = workspace_number_to_global_slot(workspace_id)
                .ok_or_else(|| format!("workspace number must be in 0..=9, got {workspace_id}"))?;
            Ok(LiftCommand::Reactor(reactor::Command::Layout(
                LC::MoveWindowToWorkspace { workspace: slot, window_id },
            )))
        }
        WorkspaceCommands::MoveWindowHidden { window_id } => Ok(LiftCommand::Reactor(
            reactor::Command::Layout(LC::MoveWindowToHiddenWorkspace { window_id }),
        )),
        WorkspaceCommands::Create => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::CreateWorkspace,
        ))),
        WorkspaceCommands::Last => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::SwitchToLastWorkspace,
        ))),
        WorkspaceCommands::ToggleHidden => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ToggleHiddenWorkspace,
        ))),
    }
}

#[cfg(test)]
mod workspace_number_tests {
    use super::*;

    #[test]
    fn cli_workspace_numbers_follow_the_digit_row() {
        let request = map_workspace_command(WorkspaceCommands::Switch { workspace_id: 1 });
        let Ok(LiftCommand::Reactor(reactor::Command::Layout(
            layout::LayoutCommand::SwitchToGlobalSlot(slot),
        ))) = request
        else {
            panic!("expected global workspace switch");
        };
        assert_eq!(slot, 0);

        let request = map_workspace_command(WorkspaceCommands::MoveWindow {
            workspace_id: 0,
            window_id: Some(42),
        });
        let Ok(LiftCommand::Reactor(reactor::Command::Layout(
            layout::LayoutCommand::MoveWindowToWorkspace { workspace, window_id },
        ))) = request
        else {
            panic!("expected workspace move");
        };
        assert_eq!(workspace, 9);
        assert_eq!(window_id, Some(42));

        let request = map_workspace_command(WorkspaceCommands::ToggleHidden);
        assert!(matches!(
            request,
            Ok(LiftCommand::Reactor(reactor::Command::Layout(
                layout::LayoutCommand::ToggleHiddenWorkspace
            )))
        ));

        let request =
            map_workspace_command(WorkspaceCommands::MoveWindowHidden { window_id: Some(42) });
        assert!(matches!(
            request,
            Ok(LiftCommand::Reactor(reactor::Command::Layout(
                layout::LayoutCommand::MoveWindowToHiddenWorkspace { window_id: Some(42) }
            )))
        ));

        assert!(map_workspace_command(WorkspaceCommands::Switch { workspace_id: 10 }).is_err());
    }
}

fn map_layout_command(cmd: LayoutCommands) -> Result<LiftCommand, String> {
    use layout::LayoutCommand as LC;
    match cmd {
        LayoutCommands::MoveNode { direction } => Ok(LiftCommand::Reactor(
            reactor::Command::Layout(LC::MoveNode(direction.into())),
        )),
        LayoutCommands::JoinWindow { direction } => Ok(LiftCommand::Reactor(
            reactor::Command::Layout(LC::JoinWindow(direction.into())),
        )),
        LayoutCommands::ToggleOrientation => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ToggleOrientation,
        ))),
        LayoutCommands::Unjoin => {
            Ok(LiftCommand::Reactor(reactor::Command::Layout(LC::UnjoinWindows)))
        }
        LayoutCommands::ToggleFocusFloat => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::ToggleFocusFloating,
        ))),
        LayoutCommands::SwapWindows { a, b } => Ok(LiftCommand::Reactor(reactor::Command::Layout(
            LC::SwapWindows(parse_window_id(&a)?, parse_window_id(&b)?),
        ))),
    }
}

fn map_config_command(cmd: ConfigCommands) -> Result<LiftCommand, String> {
    use lift::common::config::{AnimationEasing, ConfigCommand};

    let cfg_cmd = match cmd {
        ConfigCommands::SetAnimate { value } => {
            let bool_value = match value.to_lowercase().as_str() {
                "true" | "on" => true,
                "false" | "off" => false,
                _ => return Err(format!("Invalid boolean value: {}. Use true/false", value)),
            };
            ConfigCommand::SetAnimate(bool_value)
        }
        ConfigCommands::SetAnimationDuration { value } => {
            ConfigCommand::SetAnimationDuration(value)
        }
        ConfigCommands::SetAnimationFps { value } => ConfigCommand::SetAnimationFps(value),
        ConfigCommands::SetAnimationEasing { value } => {
            let easing = match value.as_str() {
                "ease_in_out" => AnimationEasing::EaseInOut,
                "linear" => AnimationEasing::Linear,
                "ease_in_sine" => AnimationEasing::EaseInSine,
                "ease_out_sine" => AnimationEasing::EaseOutSine,
                "ease_in_out_sine" => AnimationEasing::EaseInOutSine,
                "ease_in_quad" => AnimationEasing::EaseInQuad,
                "ease_out_quad" => AnimationEasing::EaseOutQuad,
                "ease_in_out_quad" => AnimationEasing::EaseInOutQuad,
                "ease_in_cubic" => AnimationEasing::EaseInCubic,
                "ease_out_cubic" => AnimationEasing::EaseOutCubic,
                "ease_in_out_cubic" => AnimationEasing::EaseInOutCubic,
                "ease_in_quart" => AnimationEasing::EaseInQuart,
                "ease_out_quart" => AnimationEasing::EaseOutQuart,
                "ease_in_out_quart" => AnimationEasing::EaseInOutQuart,
                "ease_in_quint" => AnimationEasing::EaseInQuint,
                "ease_out_quint" => AnimationEasing::EaseOutQuint,
                "ease_in_out_quint" => AnimationEasing::EaseInOutQuint,
                "ease_in_expo" => AnimationEasing::EaseInExpo,
                "ease_out_expo" => AnimationEasing::EaseOutExpo,
                "ease_in_out_expo" => AnimationEasing::EaseInOutExpo,
                "ease_in_circ" => AnimationEasing::EaseInCirc,
                "ease_out_circ" => AnimationEasing::EaseOutCirc,
                "ease_in_out_circ" => AnimationEasing::EaseInOutCirc,
                _ => return Err(format!("Invalid animation easing: {}", value)),
            };
            ConfigCommand::SetAnimationEasing(easing)
        }
        ConfigCommands::SetMouseFollowsFocus { value } => {
            ConfigCommand::SetMouseFollowsFocus(value)
        }
        ConfigCommands::SetMouseHidesOnFocus { value } => {
            ConfigCommand::SetMouseHidesOnFocus(value)
        }
        ConfigCommands::SetFocusFollowsMouse { value } => {
            ConfigCommand::SetFocusFollowsMouse(value)
        }
        ConfigCommands::SetOuterGaps { top, left, bottom, right } => {
            ConfigCommand::SetOuterGaps { top, left, bottom, right }
        }
        ConfigCommands::SetInnerGaps { horizontal, vertical } => {
            ConfigCommand::SetInnerGaps { horizontal, vertical }
        }
        ConfigCommands::SetWorkspaceNames { names } => ConfigCommand::SetWorkspaceNames(names),
        ConfigCommands::Set { key, value } => {
            let parsed_value: Value = match serde_json::from_str(&value) {
                Ok(v) => v,
                Err(_) => Value::String(value.clone()),
            };
            ConfigCommand::Set { key, value: parsed_value }
        }
        ConfigCommands::Get => ConfigCommand::GetConfig,
        ConfigCommands::Save => ConfigCommand::SaveConfig,
        ConfigCommands::Reload => ConfigCommand::ReloadConfig,
    };

    Ok(LiftCommand::Config(cfg_cmd))
}

fn map_mission_control_command(cmd: MissionControlCommands) -> Result<LiftCommand, String> {
    match cmd {
        MissionControlCommands::ShowAll => Ok(LiftCommand::Reactor(reactor::Command::Reactor(
            reactor::ReactorCommand::ShowMissionControlAll,
        ))),
        MissionControlCommands::ShowCurrent => Ok(LiftCommand::Reactor(reactor::Command::Reactor(
            reactor::ReactorCommand::ShowMissionControlCurrent,
        ))),
        MissionControlCommands::Dismiss => Ok(LiftCommand::Reactor(reactor::Command::Reactor(
            reactor::ReactorCommand::DismissMissionControl,
        ))),
    }
}

fn map_display_command(cmd: DisplayCommands) -> Result<LiftCommand, String> {
    match cmd {
        DisplayCommands::Focus { direction, index, uuid } => {
            let selector = build_display_selector(direction, index, uuid)?;
            Ok(LiftCommand::Reactor(reactor::Command::Reactor(
                reactor::ReactorCommand::FocusDisplay(selector),
            )))
        }
        DisplayCommands::MoveMouseToIndex { index } => {
            Ok(LiftCommand::Reactor(reactor::Command::Reactor(
                reactor::ReactorCommand::MoveMouseToDisplay(DisplaySelector::Index(index)),
            )))
        }
        DisplayCommands::MoveMouseToUuid { uuid } => {
            Ok(LiftCommand::Reactor(reactor::Command::Reactor(
                reactor::ReactorCommand::MoveMouseToDisplay(DisplaySelector::Uuid(uuid)),
            )))
        }
        DisplayCommands::MoveWindow {
            direction,
            index,
            uuid,
            window_id,
        } => Ok(LiftCommand::Reactor(reactor::Command::Reactor(
            reactor::ReactorCommand::MoveWindowToDisplay {
                selector: build_display_selector(direction, index, uuid)?,
                window_id,
            },
        ))),
    }
}

fn build_display_selector(
    direction: Option<String>,
    index: Option<usize>,
    uuid: Option<String>,
) -> Result<DisplaySelector, String> {
    let provided =
        direction.is_some() as usize + index.is_some() as usize + uuid.is_some() as usize;
    if provided != 1 {
        return Err(
            "display selection requires exactly one of --direction, --index, or --uuid".to_string(),
        );
    }

    if let Some(direction) = direction {
        let parsed_direction = parse_focus_direction(&direction)?;
        Ok(DisplaySelector::Direction(parsed_direction))
    } else if let Some(index) = index {
        Ok(DisplaySelector::Index(index))
    } else if let Some(uuid) = uuid {
        Ok(DisplaySelector::Uuid(uuid))
    } else {
        unreachable!("At least one selector value is guaranteed to be provided")
    }
}

fn parse_focus_direction(value: &str) -> Result<layout::Direction, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" => Ok(layout::Direction::Left),
        "right" => Ok(layout::Direction::Right),
        "up" => Ok(layout::Direction::Up),
        "down" => Ok(layout::Direction::Down),
        other => Err(format!(
            "Invalid focus direction '{}'; must be left, right, up, or down",
            other
        )),
    }
}

fn write_json(value: &Value, pretty: bool) -> Result<(), String> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut writer = io::BufWriter::new(&mut handle);

    if pretty {
        serde_json::to_writer_pretty(&mut writer, value).map_err(|e| e.to_string())?;
    } else {
        serde_json::to_writer(&mut writer, value).map_err(|e| e.to_string())?;
    }
    writer.write_all(b"\n").map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

fn run_mach_subscription(event: String) -> Result<(), String> {
    let pretty = std::env::var("LIFT_CLI_PRETTY").map(|v| v != "0").unwrap_or(false);
    let client = LiftMachClient::connect()?;
    let subscription = client.subscribe(event)?;

    loop {
        let event_payload = subscription.recv_event()?;
        // Exit cleanly when output is closed by the consumer.
        if let Err(e) = write_json(&event_payload, pretty) {
            if e.contains("Broken pipe") {
                return Ok(());
            }
            return Err(format!("Failed to write event output: {e}"));
        }
    }
}
