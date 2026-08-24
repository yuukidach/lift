use serde::{Deserialize, Serialize};

use super::command::Command;
use super::config::CoreConfig;
use super::constraints::WindowConstraints;
use super::effect::EffectCompletion;
use super::geometry::Rect;
use super::ids::{DisplayId, Generation, SpaceId, WindowId};
use super::interaction::DragObservation;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Input {
    Observation(Observation),
    Command(Command),
    EffectCompleted(EffectCompletion),
    Timer(TimerEvent),
    ConfigReloaded(CoreConfig),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    PlatformSnapshot(PlatformSnapshotObservation),
    DisplayTopology(DisplayTopologyObservation),
    FocusChanged { window: Option<WindowId> },
    Drag(DragObservation),
    MissionControl { active: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayTopologyObservation {
    pub generation: Generation,
    pub displays: Vec<DisplayObservation>,
    pub active_display: Option<DisplayId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlatformSnapshotObservation {
    pub generation: Generation,
    pub displays: Vec<DisplayObservation>,
    pub active_display: Option<DisplayId>,
    pub windows: Vec<WindowObservation>,
    pub focused_window: Option<WindowId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayObservation {
    pub id: DisplayId,
    pub frame: Rect,
    pub space: Option<SpaceId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowObservation {
    pub id: WindowId,
    pub frame: Rect,
    pub display: Option<DisplayId>,
    pub platform_id: Option<u32>,
    pub app_id: Option<String>,
    pub app_name: Option<String>,
    pub title: String,
    pub ax_role: Option<String>,
    pub ax_subrole: Option<String>,
    pub minimized: bool,
    pub fullscreen: bool,
    pub constraints: WindowConstraints,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimerEvent {
    pub id: u64,
}
