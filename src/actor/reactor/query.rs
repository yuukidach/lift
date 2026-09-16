use crate::actor::menu_bar;
use crate::actor::reactor::Reactor;
use crate::runtime::SnapshotStore;
use crate::sys::screen::SpaceId;

#[derive(Clone)]
pub struct ReactorSnapshotHandle {
    snapshots: SnapshotStore,
}

impl ReactorSnapshotHandle {
    pub(super) fn new(snapshots: SnapshotStore) -> Self { Self { snapshots } }

    pub fn snapshot(&self) -> std::sync::Arc<crate::core::snapshot::CoreSnapshot> {
        self.snapshots.load()
    }
}

impl Reactor {
    pub(super) fn maybe_send_menu_update(&mut self) {
        let menu_tx = match self.menu_manager.menu_tx.as_ref() {
            Some(tx) => tx.clone(),
            None => return,
        };

        let snapshot = match self.build_core_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::debug!(?error, "menu snapshot deferred until state stabilizes");
                return;
            }
        };
        let Some(display) = crate::interfaces::ui::active_context_display(&snapshot) else {
            return;
        };
        let Some(active_space) = display.space.map(|space| SpaceId::new(space.0)) else {
            return;
        };

        let mut displays = crate::interfaces::ui::menu_bar_display_data(
            &snapshot,
            &self.config.virtual_workspaces.display_order,
        );
        // Core display frames exclude the menu bar and Dock. Status windows must
        // instead be matched against the full display bounds in Quartz coordinates.
        for display in &mut displays {
            let Some(screen) = self
                .space_manager
                .screens
                .iter()
                .find(|screen| screen.display_uuid == display.display_uuid)
            else {
                continue;
            };
            let bounds = objc2_core_graphics::CGDisplayBounds(screen.id.as_u32());
            if bounds.size.width > 0.0 && bounds.size.height > 0.0 {
                if let Ok(frame) = crate::core::geometry::Rect::new(
                    bounds.origin.x,
                    bounds.origin.y,
                    bounds.size.width,
                    bounds.size.height,
                ) {
                    display.frame = frame;
                }
            }
        }
        let mut workspaces = Vec::new();
        let mut display_starts = Vec::new();
        for display in &displays {
            if display.workspaces.is_empty() {
                continue;
            }
            if !workspaces.is_empty() {
                display_starts.push(workspaces.len());
            }
            workspaces.extend(display.workspaces.iter().cloned());
        }
        let active_space_is_activated = self.is_space_active(active_space);
        let active_workspace = display.active_workspace;
        let active_workspace_idx = workspaces
            .iter()
            .position(|workspace| {
                active_workspace.is_some_and(|active| workspace.id == active.0.to_string())
            })
            .map(|index| index as u64);
        let windows = crate::interfaces::ui::active_workspace_windows(&snapshot);

        menu_tx.send(menu_bar::Event::Update(menu_bar::Update {
            active_space,
            active_space_is_activated,
            workspaces,
            display_starts,
            displays,
            active_workspace_idx,
            active_workspace,
            windows,
        }));
    }

    pub(crate) fn serialize_state(&self) -> Result<String, serde_json::Error> {
        let snapshot = self.core_snapshot();
        let state = serde_json::json!({
            "core": snapshot.as_ref(),
            "runtime": {
                "apps": self.app_manager.apps.len(),
                "tracked_windows": self.window_manager.tracked_window_count(),
                "window_server_info": self.window_manager.window_server_info_count(),
                "visible_window_server_ids": self.window_manager.visible_window_server_count(),
                "screens": self.space_manager.screens.len(),
            }
        });
        serde_json::to_string_pretty(&state)
    }
}
