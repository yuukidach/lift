use serde::{Deserialize, Serialize};

use crate::actor::app::WindowId as ActorWindowId;
use crate::core::bsp::Axis;
use crate::core::geometry::Rect;
use crate::core::ids::{GroupId, SpaceId, WindowId, WorkspaceId, WorkspaceNumber};
use crate::core::snapshot::{CoreSnapshot, WorkspaceSnapshot};
use crate::model::server::{WindowData, WindowLayoutPosition, WorkspaceData};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceLayoutView {
    pub id: WorkspaceId,
    pub number: u8,
    pub name: String,
    pub is_active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DisplayView {
    pub uuid: String,
    pub name: Option<String>,
    pub screen_id: Option<u32>,
    pub frame: Rect,
    pub space: Option<u64>,
    pub is_active_space: bool,
    pub is_active_context: bool,
    pub active_space_ids: Vec<u64>,
    pub inactive_space_ids: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplicationView {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub name: String,
    pub is_frontmost: bool,
    pub window_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutStateView {
    pub space_id: u64,
    pub mode: String,
    pub groups: Vec<LayoutGroupView>,
    pub floating_windows: Vec<ActorWindowId>,
    pub tiled_windows: Vec<ActorWindowId>,
    pub frames: Vec<LayoutWindowFrameView>,
    pub focused_window: Option<ActorWindowId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LayoutGroupView {
    pub id: GroupId,
    pub axis: Axis,
    pub windows: Vec<ActorWindowId>,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutWindowFrameView {
    pub window: ActorWindowId,
    pub frame: Rect,
}

pub fn workspaces(
    snapshot: &CoreSnapshot,
    space: Option<SpaceId>,
    display: Option<&str>,
) -> Vec<WorkspaceData> {
    let display = match (space, display) {
        (Some(space), _) => {
            snapshot.displays.iter().find(|candidate| candidate.space == Some(space))
        }
        (None, Some(display)) => {
            snapshot.displays.iter().find(|candidate| candidate.id.0 == display)
        }
        (None, None) => crate::interfaces::ui::active_context_display(snapshot),
    };
    display
        .map(|display| crate::interfaces::ui::workspace_data_for_display(snapshot, display))
        .unwrap_or_default()
}

pub fn displays(snapshot: &CoreSnapshot) -> Vec<DisplayView> {
    snapshot
        .displays
        .iter()
        .map(|display| DisplayView {
            uuid: display.id.0.clone(),
            name: None,
            screen_id: None,
            frame: display.frame,
            space: display.space.map(|space| space.0),
            is_active_space: display.space.is_some(),
            is_active_context: display.is_active_context,
            active_space_ids: display.space.map(|space| vec![space.0]).unwrap_or_default(),
            inactive_space_ids: Vec::new(),
        })
        .collect()
}

pub fn windows(snapshot: &CoreSnapshot, space: Option<SpaceId>) -> Vec<WindowData> {
    let active_workspace = space.and_then(|space| {
        snapshot
            .displays
            .iter()
            .find(|display| display.space == Some(space))
            .and_then(|display| display.active_workspace)
    });
    let mut windows = snapshot
        .windows
        .iter()
        .filter(|window| {
            space.is_none()
                || active_workspace.is_some_and(|workspace| window.workspace == Some(workspace))
        })
        .map(|window| {
            let mut data = crate::interfaces::ui::window_data(snapshot, window);
            data.layout_position = window_layout_position(snapshot, window.id);
            data
        })
        .collect::<Vec<_>>();
    windows.sort_by_key(|window| {
        let core_id = WindowId::new(crate::core::ids::ApplicationId(window.id.pid), window.id.idx);
        let workspace_index = snapshot
            .windows
            .iter()
            .find(|candidate| candidate.id == core_id)
            .and_then(|candidate| candidate.workspace)
            .and_then(|id| snapshot.workspaces.iter().find(|workspace| workspace.id == id))
            .map(|workspace| {
                let display_index = snapshot
                    .displays
                    .iter()
                    .position(|display| display.id == workspace.display)
                    .unwrap_or(usize::MAX);
                let number_index =
                    workspace.number.map(WorkspaceNumber::global_slot).unwrap_or(usize::MAX);
                (display_index, number_index)
            })
            .unwrap_or((usize::MAX, usize::MAX));
        window
            .layout_position
            .map(|position| {
                (
                    workspace_index.0,
                    workspace_index.1,
                    0,
                    position.group_index,
                    position.window_index,
                )
            })
            .unwrap_or((workspace_index.0, workspace_index.1, 1, usize::MAX, usize::MAX))
    });
    windows
}

pub fn window(snapshot: &CoreSnapshot, id: WindowId) -> Option<WindowData> {
    snapshot.windows.iter().find(|window| window.id == id).map(|window| {
        let mut data = crate::interfaces::ui::window_data(snapshot, window);
        data.layout_position = window_layout_position(snapshot, window.id);
        data
    })
}

fn window_layout_position(
    snapshot: &CoreSnapshot,
    window: WindowId,
) -> Option<WindowLayoutPosition> {
    let workspace = snapshot.windows.iter().find(|candidate| candidate.id == window)?.workspace?;
    snapshot
        .workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)?
        .groups
        .iter()
        .enumerate()
        .find_map(|(group_index, group)| {
            group
                .windows
                .iter()
                .position(|candidate| *candidate == window)
                .map(|window_index| WindowLayoutPosition {
                    group_id: group.id,
                    group_index,
                    window_index,
                })
        })
}

pub fn applications(snapshot: &CoreSnapshot) -> Vec<ApplicationView> {
    snapshot
        .applications
        .iter()
        .map(|application| ApplicationView {
            pid: application.id.0,
            bundle_id: application.bundle_id.clone(),
            name: application.name.clone(),
            is_frontmost: application.frontmost,
            window_count: application.window_count,
        })
        .collect()
}

pub fn layout_state(snapshot: &CoreSnapshot, space: SpaceId) -> Option<LayoutStateView> {
    let workspace = active_workspace(snapshot, space)?;
    Some(layout_state_for_workspace(snapshot, &workspace, space))
}

pub fn layout_state_for_workspace(
    snapshot: &CoreSnapshot,
    workspace: &WorkspaceSnapshot,
    space: SpaceId,
) -> LayoutStateView {
    let to_actor = |window: WindowId| ActorWindowId {
        pid: window.application.0,
        idx: window.index,
    };
    LayoutStateView {
        space_id: space.0,
        mode: "bsp".to_string(),
        groups: workspace
            .groups
            .iter()
            .map(|group| LayoutGroupView {
                id: group.id,
                axis: group.axis,
                windows: group.windows.iter().copied().map(to_actor).collect(),
                selected: group.selected,
            })
            .collect(),
        floating_windows: workspace.floating_windows.iter().copied().map(to_actor).collect(),
        tiled_windows: workspace
            .groups
            .iter()
            .flat_map(|group| group.windows.iter().copied())
            .map(to_actor)
            .collect(),
        frames: workspace
            .layout_frames
            .iter()
            .map(|(window, frame)| LayoutWindowFrameView {
                window: to_actor(*window),
                frame: *frame,
            })
            .collect(),
        focused_window: snapshot
            .focused_window
            .filter(|window| {
                workspace.groups.iter().any(|group| group.windows.contains(window))
                    || workspace.floating_windows.contains(window)
            })
            .map(to_actor),
    }
}

pub fn workspace_layouts(
    snapshot: &CoreSnapshot,
    space: Option<SpaceId>,
    workspace: Option<WorkspaceId>,
) -> Vec<WorkspaceLayoutView> {
    let display_filter = space.and_then(|space| {
        snapshot
            .displays
            .iter()
            .find(|display| display.space == Some(space))
            .map(|display| display.id.clone())
    });
    if space.is_some() && display_filter.is_none() {
        return Vec::new();
    }

    let active = snapshot
        .displays
        .iter()
        .filter_map(|display| display.active_workspace)
        .collect::<std::collections::BTreeSet<_>>();
    let mut layouts = snapshot
        .workspaces
        .iter()
        .filter(|item| display_filter.as_ref().is_none_or(|display| &item.display == display))
        .filter(|item| workspace.is_none_or(|workspace| item.id == workspace))
        .filter_map(|item| {
            Some(WorkspaceLayoutView {
                id: item.id,
                number: item.number?.get(),
                name: item.name.clone(),
                is_active: active.contains(&item.id),
            })
        })
        .collect::<Vec<_>>();
    layouts.sort_by_key(|item| {
        WorkspaceNumber::try_from(item.number)
            .map(WorkspaceNumber::global_slot)
            .unwrap_or(usize::MAX)
    });
    layouts
}

pub fn metrics(snapshot: &CoreSnapshot) -> serde_json::Value {
    let workspace_stats = snapshot
        .workspaces
        .iter()
        .map(|workspace| {
            let count = workspace.groups.iter().map(|group| group.windows.len()).sum::<usize>()
                + workspace.floating_windows.len();
            (workspace.id.0.to_string(), count)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    serde_json::json!({
        "revision": snapshot.revision,
        "windows_managed": snapshot.windows.len(),
        "workspaces": snapshot.workspaces.len(),
        "applications": snapshot.applications.len(),
        "screens": snapshot.displays.len(),
        "workspace_stats": workspace_stats,
    })
}

pub fn active_workspace(snapshot: &CoreSnapshot, space: SpaceId) -> Option<WorkspaceSnapshot> {
    let id = snapshot
        .displays
        .iter()
        .find(|display| display.space == Some(space))?
        .active_workspace?;
    workspace(snapshot, id)
}

pub fn workspace(snapshot: &CoreSnapshot, id: WorkspaceId) -> Option<WorkspaceSnapshot> {
    snapshot.workspaces.iter().find(|workspace| workspace.id == id).cloned()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::core::bsp::Axis;
    use crate::core::geometry::Rect;
    use crate::core::ids::{ApplicationId, DisplayId, Generation, GroupId, WorkspaceNumber};
    use crate::core::snapshot::{
        ApplicationSnapshot, DisplaySnapshot, GroupSnapshot, WindowSnapshot,
    };

    fn fixture() -> CoreSnapshot {
        let display = DisplayId("main".into());
        let workspace = WorkspaceId(7);
        let window = WindowId::new(ApplicationId(42), NonZeroU32::new(1).unwrap());
        CoreSnapshot {
            revision: 3,
            platform_generation: Generation(2),
            displays: vec![DisplaySnapshot {
                id: display.clone(),
                frame: Rect::new(0.0, 0.0, 1000.0, 800.0).unwrap(),
                space: Some(SpaceId(9)),
                is_active_context: true,
                active_workspace: Some(workspace),
                last_workspace: None,
            }],
            workspaces: vec![WorkspaceSnapshot {
                id: workspace,
                number: Some(WorkspaceNumber::try_from(1).unwrap()),
                name: "Main".into(),
                display,
                groups: vec![GroupSnapshot {
                    id: GroupId(3),
                    axis: Axis::Horizontal,
                    windows: vec![window],
                    selected: 0,
                }],
                floating_windows: vec![],
                last_tiled_window: None,
                last_floating_window: None,
                layout_frames: std::collections::BTreeMap::from([(
                    window,
                    Rect::new(10.0, 20.0, 900.0, 700.0).unwrap(),
                )]),
            }],
            windows: vec![WindowSnapshot {
                id: window,
                workspace: Some(workspace),
                frame: Rect::new(0.0, 0.0, 1000.0, 800.0).unwrap(),
                title: "Terminal".into(),
                application_name: Some("Terminal".into()),
                platform_id: Some(99),
                floating: false,
                minimized: false,
                fullscreen: false,
            }],
            applications: vec![ApplicationSnapshot {
                id: ApplicationId(42),
                bundle_id: Some("com.example.Terminal".into()),
                name: "Terminal".into(),
                frontmost: true,
                window_count: 1,
            }],
            focused_window: Some(window),
            drag: Default::default(),
            mission_control: Default::default(),
        }
    }

    #[test]
    fn workspace_layouts_use_stable_ids_and_snapshot_names() {
        let snapshot = fixture();

        let layouts = workspace_layouts(&snapshot, Some(SpaceId(9)), Some(WorkspaceId(7)));

        assert_eq!(layouts, vec![WorkspaceLayoutView {
            id: WorkspaceId(7),
            number: 1,
            name: "Main".into(),
            is_active: true,
        }]);
        assert!(workspace_layouts(&snapshot, Some(SpaceId(99)), None).is_empty());
    }

    #[test]
    fn query_projection_filters_by_space_without_a_mutation_queue() {
        let snapshot = fixture();
        assert_eq!(workspaces(&snapshot, Some(SpaceId(9)), None).len(), 1);
        assert_eq!(workspaces(&snapshot, None, Some("main")).len(), 1);
        assert_eq!(windows(&snapshot, Some(SpaceId(9))).len(), 1);
        assert!(workspaces(&snapshot, Some(SpaceId(10)), None).is_empty());
        assert!(workspaces(&snapshot, None, Some("missing")).is_empty());
        assert!(windows(&snapshot, Some(SpaceId(10))).is_empty());
    }

    #[test]
    fn public_query_views_are_json_safe_and_keep_the_cli_shape() {
        let snapshot = fixture();

        let workspaces = serde_json::to_value(workspaces(&snapshot, None, None)).unwrap();
        assert_eq!(workspaces[0]["number"], 1);
        assert_eq!(workspaces[0]["is_active"], true);
        assert_eq!(workspaces[0]["window_count"], 1);
        assert_eq!(workspaces[0]["windows"][0]["id"]["pid"], 42);

        let displays = serde_json::to_value(displays(&snapshot)).unwrap();
        assert_eq!(displays[0]["uuid"], "main");
        assert_eq!(displays[0]["space"], 9);

        let windows = serde_json::to_value(windows(&snapshot, None)).unwrap();
        assert_eq!(windows[0]["window_server_id"], 99);
        assert_eq!(windows[0]["bundle_id"], "com.example.Terminal");
        assert_eq!(windows[0]["layout_position"]["group_id"], 3);
        assert_eq!(windows[0]["layout_position"]["group_index"], 0);
        assert_eq!(windows[0]["layout_position"]["window_index"], 0);

        let applications = serde_json::to_value(applications(&snapshot)).unwrap();
        assert_eq!(applications[0]["pid"], 42);
        assert_eq!(applications[0]["is_frontmost"], true);

        let layout = serde_json::to_value(layout_state(&snapshot, SpaceId(9)).unwrap()).unwrap();
        assert_eq!(layout["mode"], "bsp");
        assert_eq!(layout["tiled_windows"][0]["pid"], 42);
    }

    #[test]
    fn window_query_uses_bsp_group_and_window_order() {
        let mut snapshot = fixture();
        let logical_first = WindowId::new(ApplicationId(42), NonZeroU32::new(3).unwrap());
        let logical_second = WindowId::new(ApplicationId(42), NonZeroU32::new(2).unwrap());
        let workspace = snapshot.workspaces[0].id;
        snapshot.workspaces[0].groups[0].windows = vec![logical_first, logical_second];
        snapshot.windows = [logical_second, logical_first]
            .into_iter()
            .map(|id| WindowSnapshot {
                id,
                workspace: Some(workspace),
                frame: Rect::new(0.0, 0.0, 500.0, 400.0).unwrap(),
                title: format!("Window {}", id.index),
                application_name: Some("Terminal".into()),
                platform_id: Some(id.index.get()),
                floating: false,
                minimized: false,
                fullscreen: false,
            })
            .collect();

        let queried = windows(&snapshot, Some(SpaceId(9)));
        assert_eq!(queried.iter().map(|window| window.id.idx).collect::<Vec<_>>(), [
            logical_first.index,
            logical_second.index,
        ]);
        assert_eq!(queried[0].layout_position.unwrap().window_index, 0);
        assert_eq!(queried[1].layout_position.unwrap().window_index, 1);
    }
}
