use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::bsp::Axis;
use super::geometry::Rect;
use super::ids::{
    ApplicationId, DisplayId, Generation, GroupId, SpaceId, WindowId, WorkspaceId,
    WorkspaceNumber,
};
use super::interaction::{DragSnapshot, MissionControlPhase};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplaySnapshot {
    pub id: DisplayId,
    pub frame: Rect,
    pub space: Option<SpaceId>,
    pub is_active_context: bool,
    pub active_workspace: Option<WorkspaceId>,
    pub last_workspace: Option<WorkspaceId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupSnapshot {
    pub id: GroupId,
    pub axis: Axis,
    pub windows: Vec<WindowId>,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub id: WorkspaceId,
    pub number: WorkspaceNumber,
    pub name: String,
    pub display: DisplayId,
    pub groups: Vec<GroupSnapshot>,
    pub floating_windows: Vec<WindowId>,
    pub last_tiled_window: Option<WindowId>,
    pub last_floating_window: Option<WindowId>,
    pub layout_frames: BTreeMap<WindowId, Rect>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    pub id: WindowId,
    pub workspace: Option<WorkspaceId>,
    pub frame: Rect,
    pub title: String,
    pub application_name: Option<String>,
    pub platform_id: Option<u32>,
    pub floating: bool,
    pub minimized: bool,
    pub fullscreen: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplicationSnapshot {
    pub id: ApplicationId,
    pub bundle_id: Option<String>,
    pub name: String,
    pub frontmost: bool,
    pub window_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CoreSnapshot {
    pub revision: u64,
    pub platform_generation: Generation,
    pub displays: Vec<DisplaySnapshot>,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub windows: Vec<WindowSnapshot>,
    pub applications: Vec<ApplicationSnapshot>,
    pub focused_window: Option<WindowId>,
    pub drag: DragSnapshot,
    pub mission_control: MissionControlPhase,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UiSnapshot {
    pub revision: u64,
    pub active_workspace_by_display: BTreeMap<DisplayId, WorkspaceId>,
    pub focused_window: Option<WindowId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedWorkspace {
    pub id: WorkspaceId,
    pub number: WorkspaceNumber,
    pub display: DisplayId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    pub schema_version: u16,
    pub workspaces: Vec<PersistedWorkspace>,
}
