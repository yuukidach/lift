use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::actor::app::WindowId;
use crate::core::snapshot::{CoreSnapshot, DisplaySnapshot, WindowSnapshot, WorkspaceSnapshot};
use crate::model::server::{WindowData, WorkspaceData};
use crate::sys::app::WindowInfo;
use crate::sys::window_server::WindowServerId;

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
            let display = &snapshot
                .workspaces
                .iter()
                .find(|item| item.id == workspace)?
                .display;
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
        id: WindowId { pid: window.id.application.0, idx: window.id.index },
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

pub fn workspace_data_for_display(
    snapshot: &CoreSnapshot,
    display: &DisplaySnapshot,
) -> Vec<WorkspaceData> {
    let mut workspaces = snapshot
        .workspaces
        .iter()
        .filter(|workspace| workspace.display == display.id)
        .collect::<Vec<_>>();
    workspaces.sort_by_key(|workspace| workspace.number.global_slot());
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
                    if !is_active
                        && let Some(frame) = workspace.layout_frames.get(&window.id)
                    {
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
                number: workspace.number.get() as usize,
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
    use crate::core::geometry::Rect;
    use crate::core::bsp::Axis;
    use crate::core::ids::{
        ApplicationId, DisplayId, GroupId, SpaceId, WorkspaceId, WorkspaceNumber,
    };
    use crate::core::snapshot::{DisplaySnapshot, GroupSnapshot};

    #[test]
    fn workspace_views_follow_active_context_and_use_preview_frames() {
        let window_id = crate::core::ids::WindowId::new(
            ApplicationId(42),
            NonZeroU32::new(1).unwrap(),
        );
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
                number: WorkspaceNumber::try_from(1).unwrap(),
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
            number: WorkspaceNumber::try_from(number).unwrap(),
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
}
