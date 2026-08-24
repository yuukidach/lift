use serde::{Deserialize, Serialize};

use crate::actor::app::WindowId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Default, Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    #[default]
    Horizontal,
    Vertical,
    HorizontalStack,
    VerticalStack,
}

impl LayoutKind {
    pub const fn is_group(self) -> bool {
        matches!(self, Self::HorizontalStack | Self::VerticalStack)
    }
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutCommand {
    NextWindow,
    PrevWindow,
    MoveFocus(Direction),
    MoveNode(Direction),
    JoinWindow(Direction),
    ToggleOrientation,
    UnjoinWindows,
    ToggleFocusFloating,
    ToggleWindowFloating,
    ToggleFullscreen,
    ToggleFullscreenWithinGaps,
    ResizeWindowGrow,
    ResizeWindowShrink,
    ResizeWindowBy { amount: f64 },
    NextWorkspace(Option<bool>),
    PrevWorkspace(Option<bool>),
    SwitchToWorkspace(usize),
    SwitchToGlobalSlot(usize),
    MoveWindowToWorkspace {
        workspace: usize,
        window_id: Option<u32>,
    },
    CreateWorkspace,
    SwitchToLastWorkspace,
    SwapWindows(WindowId, WindowId),
}

#[must_use]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventResponse {
    pub raise_windows: Vec<WindowId>,
    pub focus_window: Option<WindowId>,
    pub boundary_hit: Option<Direction>,
}
