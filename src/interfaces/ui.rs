use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::actor::app::WindowId;
use crate::core::geometry::Rect;
use crate::core::snapshot::{CoreSnapshot, DisplaySnapshot, WindowSnapshot, WorkspaceSnapshot};
use crate::model::server::{WindowData, WorkspaceData};
use crate::sys::app::WindowInfo;
use crate::sys::window_server::WindowServerId;

#[derive(Clone, Debug)]
pub struct MenuBarDisplayData {
    pub frame: Rect,
    pub workspaces: Vec<WorkspaceData>,
}

pub fn active_context_display(snapshot: &CoreSnapshot) -> Option<&DisplaySnapshot> {
    snapshot
        .displays
        .iter()
        .find(|display| display.is_active_context)
        .or_else(|| {
            let workspace = snapshot
                .focused_window
                .and_then(|window| snapshot.windows.iter().find(|item| item.id == window))
                .and_then(|window| window.workspace)?;
            let display = &snapshot.workspaces.iter().find(|item| item.id == workspace)?.display;
            snapshot.displays.iter().find(|item| &item.id == display)
        })
        .or_else(|| snapshot.displays.first())
}

pub fn active_workspace(snapshot: &CoreSnapshot) -> Option<&WorkspaceSnapshot> {
    let workspace = active_context_display(snapshot)?.active_workspace?;
    snapshot.workspaces.iter().find(|item| item.id == workspace)
}

pub fn window_data(snapshot: &CoreSnapshot, window: &WindowSnapshot) -> WindowData {
    let application = snapshot
        .applications
        .iter()
        .find(|application| application.id == window.id.application);
    WindowData {
        id: WindowId {
            pid: window.id.application.0,
            idx: window.id.index,
        },
        is_floating: window.floating,
        is_focused: snapshot.focused_window == Some(window.id),
        app_name: window.application_name.clone(),
        info: WindowInfo {
            is_standard: true,
            is_root: true,
            is_minimized: window.minimized,
            is_resizable: true,
            min_size: None,
            max_size: None,
            title: window.title.clone(),
            frame: CGRect::new(
                CGPoint::new(window.frame.origin.x, window.frame.origin.y),
                CGSize::new(window.frame.size.width, window.frame.size.height),
            ),
            sys_id: window.platform_id.map(WindowServerId::new),
            bundle_id: application.and_then(|application| application.bundle_id.clone()),
            path: None,
            ax_role: None,
            ax_subrole: None,
        },
    }
}

pub fn workspace_data(snapshot: &CoreSnapshot) -> Vec<WorkspaceData> {
    let Some(display) = active_context_display(snapshot) else {
        return Vec::new();
    };
    workspace_data_for_display(snapshot, display)
}

pub fn grouped_workspace_data(
    snapshot: &CoreSnapshot,
    preferred_display_order: &[String],
) -> (Vec<WorkspaceData>, Vec<usize>) {
    let mut workspaces = Vec::new();
    let mut display_starts = Vec::new();
    for display in menu_bar_display_data(snapshot, preferred_display_order) {
        if display.workspaces.is_empty() {
            continue;
        }
        if !workspaces.is_empty() {
            display_starts.push(workspaces.len());
        }
        workspaces.extend(display.workspaces);
    }
    (workspaces, display_starts)
}

pub fn menu_bar_display_data(
    snapshot: &CoreSnapshot,
    preferred_display_order: &[String],
) -> Vec<MenuBarDisplayData> {
    let mut displays = snapshot.displays.iter().collect::<Vec<_>>();
    displays.sort_by(|left, right| {
        let configured_rank = |display: &DisplaySnapshot| {
            preferred_display_order.iter().position(|uuid| uuid == &display.id.0)
        };
        match (configured_rank(left), configured_rank(right)) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left
                .frame
                .origin
                .x
                .total_cmp(&right.frame.origin.x)
                .then_with(|| left.frame.origin.y.total_cmp(&right.frame.origin.y)),
        }
    });
    displays
        .into_iter()
        .map(|display| MenuBarDisplayData {
            frame: display.frame,
            workspaces: workspace_data_for_display(snapshot, display),
        })
        .collect()
}

pub fn workspace_data_for_display(
    snapshot: &CoreSnapshot,
    display: &DisplaySnapshot,
) -> Vec<WorkspaceData> {
    let mut workspaces = snapshot
        .workspaces
        .iter()
        .filter(|workspace| workspace.display == display.id && workspace.number.is_some())
        .collect::<Vec<_>>();
    workspaces.sort_by_key(|workspace| workspace.number.expect("numbered workspace").global_slot());
    workspaces
        .into_iter()
        .enumerate()
        .map(|(index, workspace)| {
            let is_active = display.active_workspace == Some(workspace.id);
            let windows = workspace
                .groups
                .iter()
                .flat_map(|group| group.windows.iter())
                .chain(workspace.floating_windows.iter())
                .filter_map(|window| snapshot.windows.iter().find(|item| item.id == *window))
                .map(|window| {
                    let mut data = window_data(snapshot, window);
                    if !is_active && let Some(frame) = workspace.layout_frames.get(&window.id) {
                        data.info.frame = CGRect::new(
                            CGPoint::new(frame.origin.x, frame.origin.y),
                            CGSize::new(frame.size.width, frame.size.height),
                        );
                    }
                    data
                })
                .collect::<Vec<_>>();
            WorkspaceData {
                id: workspace.id.0.to_string(),
                index,
                number: workspace.number.expect("numbered workspace").get() as usize,
                name: workspace.name.clone(),
                is_active,
                window_count: windows.len(),
                windows,
            }
        })
        .collect()
}

pub fn active_workspace_windows(snapshot: &CoreSnapshot) -> Vec<WindowData> {
    let Some(workspace) = active_workspace(snapshot) else {
        return Vec::new();
    };
    workspace
        .groups
        .iter()
        .flat_map(|group| group.windows.iter())
        .chain(workspace.floating_windows.iter())
        .filter_map(|window| snapshot.windows.iter().find(|item| item.id == *window))
        .map(|window| window_data(snapshot, window))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::num::NonZeroU32;

    use super::*;
    use crate::core::bsp::Axis;
    use crate::core::geometry::Rect;
    use crate::core::ids::{
        ApplicationId, DisplayId, GroupId, SpaceId, WorkspaceId, WorkspaceNumber,
    };
    use crate::core::snapshot::{DisplaySnapshot, GroupSnapshot};

    #[test]
    fn workspace_views_follow_active_context_and_use_preview_frames() {
        let window_id =
            crate::core::ids::WindowId::new(ApplicationId(42), NonZeroU32::new(1).unwrap());
        let workspace_id = WorkspaceId(7);
        let display_id = DisplayId("main".into());
        let preview = Rect::new(50.0, 60.0, 400.0, 300.0).unwrap();
        let snapshot = CoreSnapshot {
            displays: vec![DisplaySnapshot {
                id: display_id.clone(),
                frame: Rect::new(0.0, 0.0, 1000.0, 800.0).unwrap(),
                space: Some(SpaceId(9)),
                is_active_context: true,
                active_workspace: None,
                last_workspace: None,
            }],
            workspaces: vec![WorkspaceSnapshot {
                id: workspace_id,
                number: Some(WorkspaceNumber::try_from(1).unwrap()),
                name: "Main".into(),
                display: display_id,
                groups: vec![GroupSnapshot {
                    id: GroupId(3),
                    axis: Axis::Horizontal,
                    windows: vec![window_id],
                    selected: 0,
                }],
                floating_windows: Vec::new(),
                last_tiled_window: None,
                last_floating_window: None,
                layout_frames: BTreeMap::from([(window_id, preview)]),
            }],
            windows: vec![WindowSnapshot {
                id: window_id,
                workspace: Some(workspace_id),
                frame: Rect::new(-10_000.0, 0.0, 400.0, 300.0).unwrap(),
                title: "Terminal".into(),
                application_name: Some("Terminal".into()),
                platform_id: Some(99),
                floating: false,
                minimized: false,
                fullscreen: false,
            }],
            focused_window: Some(window_id),
            ..CoreSnapshot::default()
        };

        let workspaces = workspace_data(&snapshot);

        assert_eq!(workspaces.len(), 1);
        assert!(!workspaces[0].is_active);
        assert_eq!(workspaces[0].windows[0].info.frame.origin.x, preview.origin.x);
        assert_eq!(workspaces[0].windows[0].info.frame.origin.y, preview.origin.y);
    }

    #[test]
    fn workspace_views_follow_digit_row_order() {
        let display_id = DisplayId("main".into());
        let workspace = |id, number| WorkspaceSnapshot {
            id: WorkspaceId(id),
            number: Some(WorkspaceNumber::try_from(number).unwrap()),
            name: format!("Workspace {number}"),
            display: display_id.clone(),
            groups: Vec::new(),
            floating_windows: Vec::new(),
            last_tiled_window: None,
            last_floating_window: None,
            layout_frames: BTreeMap::new(),
        };
        let snapshot = CoreSnapshot {
            displays: vec![DisplaySnapshot {
                id: display_id.clone(),
                frame: Rect::new(0.0, 0.0, 1000.0, 800.0).unwrap(),
                space: Some(SpaceId(9)),
                is_active_context: true,
                active_workspace: Some(WorkspaceId(1)),
                last_workspace: None,
            }],
            workspaces: vec![workspace(0, 0), workspace(9, 9), workspace(1, 1)],
            ..CoreSnapshot::default()
        };

        let workspaces = workspace_data(&snapshot);

        assert_eq!(
            workspaces.iter().map(|workspace| workspace.number).collect::<Vec<_>>(),
            vec![1, 9, 0]
        );
    }

    #[test]
    fn hidden_workspace_is_not_projected_into_the_menu_bar() {
        let display_id = DisplayId("main".into());
        let hidden_id = WorkspaceId(99);
        let snapshot = CoreSnapshot {
            displays: vec![DisplaySnapshot {
                id: display_id.clone(),
                frame: Rect::new(0.0, 0.0, 1000.0, 800.0).unwrap(),
                space: Some(SpaceId(9)),
                is_active_context: true,
                active_workspace: Some(hidden_id),
                last_workspace: Some(WorkspaceId(1)),
            }],
            workspaces: vec![
                WorkspaceSnapshot {
                    id: WorkspaceId(1),
                    number: Some(WorkspaceNumber::try_from(1).unwrap()),
                    name: "Workspace 1".into(),
                    display: display_id.clone(),
                    groups: Vec::new(),
                    floating_windows: Vec::new(),
                    last_tiled_window: None,
                    last_floating_window: None,
                    layout_frames: BTreeMap::new(),
                },
                WorkspaceSnapshot {
                    id: hidden_id,
                    number: None,
                    name: "Hidden Workspace".into(),
                    display: display_id.clone(),
                    groups: Vec::new(),
                    floating_windows: Vec::new(),
                    last_tiled_window: None,
                    last_floating_window: None,
                    layout_frames: BTreeMap::new(),
                },
            ],
            ..CoreSnapshot::default()
        };

        let workspaces = workspace_data_for_display(&snapshot, &snapshot.displays[0]);
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].number, 1);
        assert!(!workspaces[0].is_active);
    }

    #[test]
    fn grouped_workspace_views_mark_display_boundaries_and_keep_each_active_workspace() {
        let left = DisplayId("left".into());
        let right = DisplayId("right".into());
        let workspace = |id, number, display: &DisplayId| WorkspaceSnapshot {
            id: WorkspaceId(id),
            number: Some(WorkspaceNumber::try_from(number).unwrap()),
            name: format!("Workspace {number}"),
            display: display.clone(),
            groups: Vec::new(),
            floating_windows: Vec::new(),
            last_tiled_window: None,
            last_floating_window: None,
            layout_frames: BTreeMap::new(),
        };
        let display = |id: &DisplayId, active_workspace, x| DisplaySnapshot {
            id: id.clone(),
            frame: Rect::new(x, 0.0, 1000.0, 800.0).unwrap(),
            space: Some(SpaceId(active_workspace)),
            is_active_context: id == &left,
            active_workspace: Some(WorkspaceId(active_workspace)),
            last_workspace: None,
        };
        let snapshot = CoreSnapshot {
            displays: vec![display(&right, 2, 0.0), display(&left, 1, -1000.0)],
            workspaces: vec![
                workspace(1, 1, &left),
                workspace(3, 3, &left),
                workspace(2, 2, &right),
            ],
            ..CoreSnapshot::default()
        };

        let (workspaces, display_starts) = grouped_workspace_data(&snapshot, &[]);

        assert_eq!(display_starts, vec![2]);
        assert_eq!(
            workspaces.iter().map(|workspace| workspace.number).collect::<Vec<_>>(),
            vec![1, 3, 2]
        );
        assert_eq!(
            workspaces
                .iter()
                .filter(|workspace| workspace.is_active)
                .map(|workspace| workspace.number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let configured_order = vec!["right".to_string(), "left".to_string()];
        let (workspaces, display_starts) = grouped_workspace_data(&snapshot, &configured_order);
        assert_eq!(display_starts, vec![1]);
        assert_eq!(
            workspaces.iter().map(|workspace| workspace.number).collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
    }

    #[test]
    fn menu_bar_display_data_keeps_each_displays_workspaces() {
        let left = DisplayId("left".into());
        let right = DisplayId("right".into());
        let workspace = |id, number, display: &DisplayId| WorkspaceSnapshot {
            id: WorkspaceId(id),
            number: Some(WorkspaceNumber::try_from(number).unwrap()),
            name: String::new(),
            display: display.clone(),
            groups: Vec::new(),
            floating_windows: Vec::new(),
            last_tiled_window: None,
            last_floating_window: None,
            layout_frames: BTreeMap::new(),
        };
        let snapshot = CoreSnapshot {
            displays: vec![
                DisplaySnapshot {
                    id: left.clone(),
                    frame: Rect::new(0.0, 0.0, 1000.0, 800.0).unwrap(),
                    space: Some(SpaceId(1)),
                    is_active_context: true,
                    active_workspace: Some(WorkspaceId(1)),
                    last_workspace: None,
                },
                DisplaySnapshot {
                    id: right.clone(),
                    frame: Rect::new(1000.0, 0.0, 1000.0, 800.0).unwrap(),
                    space: Some(SpaceId(2)),
                    is_active_context: false,
                    active_workspace: Some(WorkspaceId(2)),
                    last_workspace: None,
                },
            ],
            workspaces: vec![workspace(1, 1, &left), workspace(2, 2, &right)],
            ..CoreSnapshot::default()
        };

        let displays = menu_bar_display_data(&snapshot, &[]);
        assert_eq!(displays.len(), 2);
        assert_eq!(
            displays[0]
                .workspaces
                .iter()
                .map(|workspace| workspace.number)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            displays[1]
                .workspaces
                .iter()
                .map(|workspace| workspace.number)
                .collect::<Vec<_>>(),
            vec![2]
        );

        let (global, starts) = grouped_workspace_data(&snapshot, &[]);
        assert_eq!(
            global.iter().map(|workspace| workspace.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(starts, vec![1]);
    }
}
