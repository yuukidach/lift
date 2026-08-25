use serde::{Deserialize, Serialize};

use super::ids::{DisplayId, WindowId, WorkspaceNumber};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Window(WindowCommand),
    Workspace(WorkspaceCommand),
    Display(DisplayCommand),
    MissionControl(MissionControlCommand),
    Diagnostics(DiagnosticsCommand),
    ReloadConfig,
    SaveAndExit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowCommand {
    Activate {
        window: WindowId,
    },
    Focus {
        direction: Direction,
        window: Option<WindowId>,
    },
    Move {
        direction: Direction,
        window: Option<WindowId>,
    },
    Resize {
        amount: f64,
        window: Option<WindowId>,
    },
    ResizeDirectional {
        direction: Direction,
        window: Option<WindowId>,
    },
    ToggleFocusLayer {
        window: Option<WindowId>,
    },
    ToggleFloating {
        window: Option<WindowId>,
    },
    ToggleFullscreen {
        window: Option<WindowId>,
        within_gaps: bool,
    },
    Join {
        direction: Direction,
        window: Option<WindowId>,
    },
    Unjoin {
        window: Option<WindowId>,
    },
    ToggleOrientation {
        window: Option<WindowId>,
    },
    Swap {
        first: WindowId,
        second: WindowId,
    },
    Close(Option<WindowId>),
    Next {
        window: Option<WindowId>,
    },
    Previous {
        window: Option<WindowId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCommand {
    Activate(WorkspaceNumber),
    ActivateOrCreate {
        workspace: WorkspaceNumber,
        display: DisplayId,
    },
    MoveWindow {
        workspace: WorkspaceNumber,
        window: Option<WindowId>,
    },
    MoveWindowToHidden {
        display: DisplayId,
        window: Option<WindowId>,
    },
    Next {
        display: DisplayId,
        skip_empty: bool,
    },
    Previous {
        display: DisplayId,
        skip_empty: bool,
    },
    Last {
        display: DisplayId,
    },
    ToggleHidden {
        display: DisplayId,
    },
    Create {
        display: DisplayId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayCommand {
    Focus(Direction),
    MoveWindow {
        direction: Direction,
        window: Option<WindowId>,
    },
    MoveWindowTo {
        display: DisplayId,
        window: Option<WindowId>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionControlCommand {
    ShowAll,
    ShowCurrent,
    Dismiss,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticsCommand {
    PrintTree,
    ShowTiming,
    StartRecording,
    StopRecording,
}
