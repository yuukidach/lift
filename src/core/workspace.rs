use std::collections::{BTreeMap, BTreeSet};

use super::bsp::{BspError, BspTree};
use super::config::LayoutConfig;
use super::constraints::WindowConstraints;
use super::error::CoreError;
use super::geometry::Rect;
use super::ids::{DisplayId, WindowId, WorkspaceId, WorkspaceNumber};
use super::snapshot::{PersistedWorkspace, WorkspaceSnapshot};

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceCatalog {
    workspaces: BTreeMap<WorkspaceId, Workspace>,
    by_number: BTreeMap<WorkspaceNumber, WorkspaceId>,
    displays: BTreeMap<DisplayId, DisplayWorkspaces>,
    window_assignment: BTreeMap<WindowId, WorkspaceId>,
    next_workspace_id: u64,
}

#[derive(Clone, Debug)]
struct Workspace {
    id: WorkspaceId,
    number: WorkspaceNumber,
    name: String,
    display: DisplayId,
    tiled: BspTree,
    floating: BTreeSet<WindowId>,
    floating_positions: BTreeMap<WindowId, Rect>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DisplayWorkspaces {
    active: Option<WorkspaceId>,
    last: Option<WorkspaceId>,
}

// The catalog is the mutable workspace aggregate used by the core reducer.
#[allow(dead_code)]
impl WorkspaceCatalog {
    pub fn from_persisted(workspaces: &[PersistedWorkspace]) -> Result<Self, CoreError> {
        let mut catalog = Self::default();
        for persisted in workspaces {
            if persisted.id.0 == 0 {
                return Err(CoreError::InvariantViolation(
                    "persisted workspace ids must be non-zero".into(),
                ));
            }
            if catalog.workspaces.contains_key(&persisted.id) {
                return Err(CoreError::WorkspaceConflict(persisted.id));
            }
            if let Some(existing) = catalog.by_number.get(&persisted.number) {
                return Err(CoreError::WorkspaceConflict(*existing));
            }

            catalog.next_workspace_id = catalog.next_workspace_id.max(persisted.id.0);
            catalog.by_number.insert(persisted.number, persisted.id);
            catalog.workspaces.insert(
                persisted.id,
                Workspace {
                    id: persisted.id,
                    number: persisted.number,
                    name: format!("Workspace {}", persisted.number.get()),
                    display: persisted.display.clone(),
                    tiled: BspTree::default(),
                    floating: BTreeSet::new(),
                    floating_positions: BTreeMap::new(),
                },
            );

            let display = catalog.displays.entry(persisted.display.clone()).or_default();
            let should_activate = display.active.is_none_or(|active| {
                catalog
                    .workspaces
                    .get(&active)
                    .is_none_or(|workspace| persisted.number < workspace.number)
            });
            if should_activate {
                display.active = Some(persisted.id);
            }
        }
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn reconcile_displays(
        &mut self,
        online: &[DisplayId],
        migration_receiver: Option<&DisplayId>,
    ) -> Result<(), CoreError> {
        let accepted_order = online.to_vec();
        let online = online.iter().cloned().collect::<BTreeSet<_>>();
        if online.is_empty() {
            if self.workspaces.is_empty() {
                self.displays.clear();
                return Ok(());
            }
            return Err(CoreError::IncompleteObservation(
                "cannot migrate live workspaces without an online display".into(),
            ));
        }

        let receiver = migration_receiver
            .filter(|receiver| online.contains(*receiver))
            .cloned()
            .ok_or_else(|| {
                CoreError::IncompleteObservation(
                    "online displays have no valid migration receiver".into(),
                )
            })?;
        for workspace in self.workspaces.values_mut() {
            if !online.contains(&workspace.display) {
                workspace.display = receiver.clone();
            }
        }
        self.displays.retain(|display, _| online.contains(display));
        for display in &online {
            self.displays.entry(display.clone()).or_default();
        }

        // macOS reports screens front-to-back while Lift's global workspace
        // numbering historically assigns the first slot from the back.
        for display in accepted_order.iter().rev() {
            if !self.workspaces.values().any(|workspace| &workspace.display == display) {
                let number = self.next_available_number().ok_or_else(|| {
                    CoreError::InvalidCommand("all ten workspace numbers are in use".into())
                })?;
                self.create(number, display.clone())?;
            }
        }

        for display in &online {
            let fallback = self
                .workspaces
                .values()
                .filter(|workspace| &workspace.display == display)
                .min_by_key(|workspace| workspace.number)
                .map(|workspace| workspace.id)
                .expect("each online display was assigned a workspace");
            let state = self.displays.get_mut(display).expect("display state was inserted");
            let valid = |workspace: WorkspaceId| {
                self.workspaces
                    .get(&workspace)
                    .is_some_and(|workspace| &workspace.display == display)
            };
            if !state.active.is_some_and(valid) {
                state.active = Some(fallback);
            }
            if !state.last.is_some_and(valid) {
                state.last = None;
            }
        }
        self.validate()
    }

    pub fn workspace_by_number(&self, number: WorkspaceNumber) -> Option<WorkspaceId> {
        self.by_number.get(&number).copied()
    }

    pub fn workspace_by_name(&self, display: &DisplayId, name: &str) -> Option<WorkspaceId> {
        self.workspaces
            .values()
            .find(|workspace| &workspace.display == display && workspace.name == name)
            .map(|workspace| workspace.id)
    }

    pub fn display_for_workspace(&self, workspace: WorkspaceId) -> Option<&DisplayId> {
        self.workspaces.get(&workspace).map(|workspace| &workspace.display)
    }

    pub fn create_numbered(
        &mut self,
        number: WorkspaceNumber,
        display: DisplayId,
    ) -> Result<WorkspaceId, CoreError> {
        self.create(number, display)
    }

    pub fn create_next(&mut self, display: DisplayId) -> Result<WorkspaceId, CoreError> {
        let number = self.next_available_number().ok_or_else(|| {
            CoreError::InvalidCommand("all ten workspace numbers are in use".into())
        })?;
        self.create(number, display)
    }

    fn next_available_number(&self) -> Option<WorkspaceNumber> {
        WorkspaceNumber::ORDERED
            .into_iter()
            .find(|number| !self.by_number.contains_key(number))
    }

    pub fn create(
        &mut self,
        number: WorkspaceNumber,
        display: DisplayId,
    ) -> Result<WorkspaceId, CoreError> {
        if let Some(existing) = self.by_number.get(&number) {
            return Err(CoreError::WorkspaceConflict(*existing));
        }
        self.next_workspace_id += 1;
        let id = WorkspaceId(self.next_workspace_id);
        self.workspaces.insert(
            id,
            Workspace {
                id,
                number,
                name: format!("Workspace {}", number.get()),
                display: display.clone(),
                tiled: BspTree::default(),
                floating: BTreeSet::new(),
                floating_positions: BTreeMap::new(),
            },
        );
        self.by_number.insert(number, id);
        let display_state = self.displays.entry(display).or_default();
        if display_state.active.is_none() {
            display_state.active = Some(id);
        }
        Ok(id)
    }

    pub fn activate(
        &mut self,
        display: &DisplayId,
        workspace: WorkspaceId,
    ) -> Result<(), CoreError> {
        let Some(state) = self.workspaces.get(&workspace) else {
            return Err(CoreError::WorkspaceConflict(workspace));
        };
        if &state.display != display {
            return Err(CoreError::InvalidCommand(format!(
                "workspace {workspace:?} is bound to {:?}, not {display:?}",
                state.display
            )));
        }
        let display_state = self.displays.entry(display.clone()).or_default();
        if display_state.active != Some(workspace) {
            display_state.last = display_state.active;
            display_state.active = Some(workspace);
        }
        Ok(())
    }

    pub fn active_workspace(&self, display: &DisplayId) -> Option<WorkspaceId> {
        self.displays.get(display).and_then(|state| state.active)
    }

    pub fn last_workspace(&self, display: &DisplayId) -> Option<WorkspaceId> {
        self.displays.get(display).and_then(|state| state.last)
    }

    pub fn step_workspace(
        &self,
        display: &DisplayId,
        current: WorkspaceId,
        forward: bool,
        skip_empty: bool,
    ) -> Option<WorkspaceId> {
        let mut candidates = self
            .workspaces
            .values()
            .filter(|workspace| &workspace.display == display)
            .map(|workspace| {
                (
                    workspace.number,
                    workspace.id,
                    workspace.tiled.is_empty() && workspace.floating.is_empty(),
                )
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(number, _, _)| *number);
        if candidates.is_empty() {
            return None;
        }
        let position = candidates
            .iter()
            .position(|(_, workspace, _)| *workspace == current)
            .unwrap_or(0);
        (1..=candidates.len()).find_map(|offset| {
            let next = if forward {
                (position + offset) % candidates.len()
            } else {
                (position + candidates.len() - (offset % candidates.len())) % candidates.len()
            };
            let (_, workspace, empty) = candidates[next];
            (!skip_empty || !empty).then_some(workspace)
        })
    }

    pub fn move_window(
        &mut self,
        window: WindowId,
        workspace: WorkspaceId,
    ) -> Result<(), CoreError> {
        if self.is_floating(window) {
            let position = self
                .workspace_for_window(window)
                .and_then(|source| self.workspaces.get(&source))
                .and_then(|source| source.floating_positions.get(&window))
                .copied();
            self.assign_floating(workspace, window)?;
            if let Some(position) = position {
                self.workspaces
                    .get_mut(&workspace)
                    .expect("target workspace was checked")
                    .floating_positions
                    .insert(window, position);
            }
            Ok(())
        } else {
            self.assign_tiled(workspace, window, None)
        }
    }

    pub fn record_active_floating_position(
        &mut self,
        window: WindowId,
        frame: Rect,
    ) -> Result<bool, CoreError> {
        let workspace = self.workspace_for_window(window).ok_or(CoreError::MissingWindow(window))?;
        let state = self
            .workspaces
            .get(&workspace)
            .ok_or(CoreError::WorkspaceConflict(workspace))?;
        if !state.floating.contains(&window)
            || self.active_workspace(&state.display) != Some(workspace)
        {
            return Ok(false);
        }
        Ok(self
            .workspaces
            .get_mut(&workspace)
            .expect("workspace was checked")
            .floating_positions
            .insert(window, frame)
            != Some(frame))
    }

    pub fn destroy_if_ephemeral(&mut self, workspace: WorkspaceId) -> Result<bool, CoreError> {
        let Some(candidate) = self.workspaces.get(&workspace) else {
            return Ok(false);
        };
        let display = candidate.display.clone();
        let empty = candidate.tiled.is_empty() && candidate.floating.is_empty();
        let workspace_count = self
            .workspaces
            .values()
            .filter(|candidate| candidate.display == display)
            .count();
        let active = self.active_workspace(&display) == Some(workspace);
        if !empty || active || workspace_count <= 1 {
            return Ok(false);
        }

        let removed = self
            .workspaces
            .remove(&workspace)
            .expect("ephemeral workspace was checked above");
        self.by_number.remove(&removed.number);
        if let Some(display_state) = self.displays.get_mut(&display)
            && display_state.last == Some(workspace)
        {
            display_state.last = None;
        }
        Ok(true)
    }

    pub fn workspace_for_window(&self, window: WindowId) -> Option<WorkspaceId> {
        self.window_assignment.get(&window).copied()
    }

    pub fn is_floating(&self, window: WindowId) -> bool {
        self.workspace_for_window(window)
            .and_then(|workspace| self.workspaces.get(&workspace))
            .is_some_and(|workspace| workspace.floating.contains(&window))
    }

    pub fn floating_windows(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<WindowId>, CoreError> {
        Ok(self
            .workspaces
            .get(&workspace)
            .ok_or(CoreError::WorkspaceConflict(workspace))?
            .floating
            .iter()
            .copied()
            .collect())
    }

    pub fn assign_tiled(
        &mut self,
        workspace: WorkspaceId,
        window: WindowId,
        after: Option<WindowId>,
    ) -> Result<(), CoreError> {
        if !self.workspaces.contains_key(&workspace) {
            return Err(CoreError::WorkspaceConflict(workspace));
        }
        if let Some(after) = after {
            let target_is_tiled =
                self.workspaces.get(&workspace).is_some_and(|state| state.tiled.contains(after));
            if !target_is_tiled {
                return Err(CoreError::InvalidCommand(format!(
                    "insertion target {after:?} is not tiled in workspace {workspace:?}"
                )));
            }
        }
        if self.workspace_for_window(window) == Some(workspace) && !self.is_floating(window) {
            return Ok(());
        }

        self.remove_membership(window)?;
        self.workspaces
            .get_mut(&workspace)
            .expect("workspace was checked above")
            .tiled
            .insert_after(after, window)
            .map_err(map_bsp_error)?;
        self.window_assignment.insert(window, workspace);
        Ok(())
    }

    pub fn assign_floating(
        &mut self,
        workspace: WorkspaceId,
        window: WindowId,
    ) -> Result<(), CoreError> {
        if !self.workspaces.contains_key(&workspace) {
            return Err(CoreError::WorkspaceConflict(workspace));
        }
        if self.workspace_for_window(window) == Some(workspace) && self.is_floating(window) {
            return Ok(());
        }
        self.remove_membership(window)?;
        self.workspaces
            .get_mut(&workspace)
            .expect("workspace was checked above")
            .floating
            .insert(window);
        self.window_assignment.insert(window, workspace);
        Ok(())
    }

    pub fn join(&mut self, window: WindowId, target: WindowId) -> Result<bool, CoreError> {
        let workspace =
            self.workspace_for_window(window).ok_or(CoreError::MissingWindow(window))?;
        if self.workspace_for_window(target) != Some(workspace) {
            return Err(CoreError::InvalidCommand(
                "joined windows must belong to the same workspace".into(),
            ));
        }
        let state = self
            .workspaces
            .get_mut(&workspace)
            .expect("assignments only reference existing workspaces");
        if state.floating.contains(&window) || state.floating.contains(&target) {
            return Err(CoreError::InvalidCommand(
                "floating windows cannot join a BSP group".into(),
            ));
        }
        state.tiled.join(window, target).map_err(map_bsp_error)
    }

    pub fn unjoin(&mut self, window: WindowId) -> Result<bool, CoreError> {
        let workspace =
            self.workspace_for_window(window).ok_or(CoreError::MissingWindow(window))?;
        let state = self
            .workspaces
            .get_mut(&workspace)
            .expect("assignments only reference existing workspaces");
        if state.floating.contains(&window) {
            return Err(CoreError::InvalidCommand(
                "floating windows do not belong to BSP groups".into(),
            ));
        }
        state.tiled.unjoin(window).map_err(map_bsp_error)
    }

    pub fn selected_tiled_windows(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<WindowId>, CoreError> {
        self.workspaces
            .get(&workspace)
            .ok_or(CoreError::WorkspaceConflict(workspace))?
            .tiled
            .selected_windows()
            .map_err(map_bsp_error)
    }

    pub fn select_tiled_window(&mut self, window: WindowId) -> Result<(), CoreError> {
        let workspace = self.workspace_for_window(window).ok_or(CoreError::MissingWindow(window))?;
        self.workspaces
            .get_mut(&workspace)
            .expect("window assignments reference live workspaces")
            .tiled
            .select(window)
            .map_err(map_bsp_error)
    }

    pub fn swap(&mut self, first: WindowId, second: WindowId) -> Result<bool, CoreError> {
        let workspace = self.workspace_for_window(first).ok_or(CoreError::MissingWindow(first))?;
        if self.workspace_for_window(second) != Some(workspace) {
            return Err(CoreError::InvalidCommand(
                "swapped windows must belong to the same workspace".into(),
            ));
        }
        self.workspaces
            .get_mut(&workspace)
            .expect("window assignments reference live workspaces")
            .tiled
            .swap(first, second)
            .map_err(map_bsp_error)
    }

    pub fn resize(&mut self, window: WindowId, amount: f64) -> Result<bool, CoreError> {
        let workspace = self.workspace_for_window(window).ok_or(CoreError::MissingWindow(window))?;
        self.workspaces
            .get_mut(&workspace)
            .expect("window assignments reference live workspaces")
            .tiled
            .resize(window, amount)
            .map_err(map_bsp_error)
    }

    pub fn toggle_orientation(&mut self, window: WindowId) -> Result<bool, CoreError> {
        let workspace = self.workspace_for_window(window).ok_or(CoreError::MissingWindow(window))?;
        self.workspaces
            .get_mut(&workspace)
            .expect("window assignments reference live workspaces")
            .tiled
            .toggle_orientation(window)
            .map_err(map_bsp_error)
    }

    pub fn toggle_fullscreen(
        &mut self,
        window: WindowId,
        within_gaps: bool,
    ) -> Result<bool, CoreError> {
        let workspace = self.workspace_for_window(window).ok_or(CoreError::MissingWindow(window))?;
        self.workspaces
            .get_mut(&workspace)
            .expect("window assignments reference live workspaces")
            .tiled
            .toggle_fullscreen(window, within_gaps)
            .map_err(map_bsp_error)
    }

    pub fn remove_window(&mut self, window: WindowId) -> Result<bool, CoreError> {
        if self.workspace_for_window(window).is_none() {
            return Ok(false);
        }
        self.remove_membership(window)?;
        Ok(true)
    }

    pub fn snapshots(&self) -> Result<Vec<WorkspaceSnapshot>, CoreError> {
        self.snapshots_with_layout(
            &BTreeMap::new(),
            &LayoutConfig::default(),
            &BTreeMap::new(),
        )
    }

    pub fn snapshots_with_layout(
        &self,
        displays: &BTreeMap<DisplayId, Rect>,
        layout: &LayoutConfig,
        constraints: &BTreeMap<WindowId, WindowConstraints>,
    ) -> Result<Vec<WorkspaceSnapshot>, CoreError> {
        self.workspaces
            .values()
            .map(|workspace| {
                let mut layout_frames = displays
                    .get(&workspace.display)
                    .map(|frame| {
                        workspace
                            .tiled
                            .layout(*frame, layout.gaps_for(&workspace.display), constraints)
                            .map_err(map_bsp_error)
                    })
                    .transpose()?
                    .unwrap_or_default();
                layout_frames.extend(
                    workspace
                        .floating_positions
                        .iter()
                        .map(|(window, frame)| (*window, *frame)),
                );
                Ok(WorkspaceSnapshot {
                    id: workspace.id,
                    number: workspace.number,
                    name: workspace.name.clone(),
                    display: workspace.display.clone(),
                    groups: workspace.tiled.groups().map_err(map_bsp_error)?,
                    floating_windows: workspace.floating.iter().copied().collect(),
                    last_tiled_window: None,
                    last_floating_window: None,
                    layout_frames,
                })
            })
            .collect()
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        if self.workspaces.len() != self.by_number.len() {
            return Err(CoreError::InvariantViolation(
                "workspace number index has the wrong size".into(),
            ));
        }

        let mut membership = BTreeMap::new();
        for (id, workspace) in &self.workspaces {
            if workspace.id != *id || self.by_number.get(&workspace.number) != Some(id) {
                return Err(CoreError::InvariantViolation(
                    "workspace number index disagrees with workspace state".into(),
                ));
            }
            workspace.tiled.validate().map_err(map_bsp_error)?;
            for window in workspace.tiled.windows() {
                if workspace.floating.contains(&window) {
                    return Err(CoreError::InvariantViolation(
                        "window is both tiled and floating".into(),
                    ));
                }
                if membership.insert(window, *id).is_some() {
                    return Err(CoreError::InvariantViolation(
                        "tiled window occurs in multiple workspaces".into(),
                    ));
                }
            }
            for window in &workspace.floating {
                if membership.insert(*window, *id).is_some() {
                    return Err(CoreError::InvariantViolation(
                        "floating window occurs in multiple workspaces".into(),
                    ));
                }
            }
        }
        if membership != self.window_assignment {
            return Err(CoreError::InvariantViolation(
                "window assignment index disagrees with workspace membership".into(),
            ));
        }

        for (display, state) in &self.displays {
            let valid = |workspace: Option<WorkspaceId>| {
                workspace.is_none_or(|id| {
                    self.workspaces.get(&id).is_some_and(|workspace| &workspace.display == display)
                })
            };
            if !valid(state.active) || !valid(state.last) {
                return Err(CoreError::InvariantViolation(
                    "display active/last state references another display".into(),
                ));
            }
            if self.workspaces.values().any(|workspace| &workspace.display == display)
                && state.active.is_none()
            {
                return Err(CoreError::InvariantViolation(
                    "display with workspaces has no active workspace".into(),
                ));
            }
        }
        Ok(())
    }

    fn remove_membership(&mut self, window: WindowId) -> Result<(), CoreError> {
        let Some(workspace_id) = self.window_assignment.remove(&window) else {
            return Ok(());
        };
        let workspace = self.workspaces.get_mut(&workspace_id).ok_or_else(|| {
            CoreError::InvariantViolation("window assignment references missing workspace".into())
        })?;
        if workspace.floating.remove(&window) {
            workspace.floating_positions.remove(&window);
            return Ok(());
        }
        workspace.tiled.remove(window).map_err(map_bsp_error)
    }

}

fn map_bsp_error(error: BspError) -> CoreError {
    match error {
        BspError::MissingWindow(window) | BspError::MissingTarget(window) => {
            CoreError::MissingWindow(window)
        }
        BspError::DuplicateWindow(window) => {
            CoreError::InvalidCommand(format!("window {window:?} already belongs to the BSP tree"))
        }
        BspError::InvalidRatio(ratio) => {
            CoreError::InvalidCommand(format!("invalid BSP ratio {ratio}"))
        }
        BspError::InvariantViolation(message) => CoreError::InvariantViolation(message),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::core::ids::ApplicationId;

    fn number(value: u8) -> WorkspaceNumber {
        WorkspaceNumber::try_from(value).unwrap()
    }

    fn window(index: u32) -> WindowId {
        WindowId::new(ApplicationId(7), NonZeroU32::new(index).unwrap())
    }

    #[test]
    fn assignment_has_one_authoritative_workspace_and_moves_atomically() {
        let mut catalog = WorkspaceCatalog::default();
        let first = catalog.create(number(1), DisplayId("left".into())).unwrap();
        let second = catalog.create(number(2), DisplayId("right".into())).unwrap();
        catalog.assign_tiled(first, window(1), None).unwrap();
        catalog.assign_tiled(second, window(1), None).unwrap();

        catalog.validate().unwrap();
        assert_eq!(catalog.workspace_for_window(window(1)), Some(second));
        assert!(catalog.snapshots().unwrap()[0].groups.is_empty());
    }

    #[test]
    fn floating_and_tiled_membership_are_mutually_exclusive() {
        let mut catalog = WorkspaceCatalog::default();
        let workspace = catalog.create(number(1), DisplayId("main".into())).unwrap();
        catalog.assign_tiled(workspace, window(1), None).unwrap();
        catalog.assign_floating(workspace, window(1)).unwrap();

        catalog.validate().unwrap();
        assert!(catalog.is_floating(window(1)));
        assert!(catalog.snapshots().unwrap()[0].groups.is_empty());
    }

    #[test]
    fn active_and_last_workspace_are_scoped_to_the_display() {
        let mut catalog = WorkspaceCatalog::default();
        let display = DisplayId("main".into());
        let first = catalog.create(number(1), display.clone()).unwrap();
        let second = catalog.create(number(2), display.clone()).unwrap();
        assert_eq!(catalog.active_workspace(&display), Some(first));
        catalog.activate(&display, second).unwrap();
        catalog.validate().unwrap();
        assert_eq!(catalog.active_workspace(&display), Some(second));
        assert_eq!(catalog.last_workspace(&display), Some(first));
    }

    #[test]
    fn automatic_workspace_numbers_follow_digit_row_order() {
        let mut catalog = WorkspaceCatalog::default();
        let display = DisplayId("main".into());
        for _ in 0..WorkspaceNumber::COUNT {
            catalog.create_next(display.clone()).unwrap();
        }
        let numbers = catalog
            .snapshots()
            .unwrap()
            .into_iter()
            .map(|workspace| workspace.number)
            .collect::<Vec<_>>();

        assert_eq!(numbers, WorkspaceNumber::ORDERED);
        assert!(catalog.create_next(display).is_err());
    }

    #[test]
    fn mixed_operation_sequence_preserves_catalog_invariants() {
        let mut catalog = WorkspaceCatalog::default();
        let first = catalog.create(number(1), DisplayId("left".into())).unwrap();
        let second = catalog.create(number(2), DisplayId("right".into())).unwrap();
        for index in 1..=20 {
            let target = if index % 2 == 0 { first } else { second };
            catalog.assign_tiled(target, window(index), None).unwrap();
            catalog.validate().unwrap();
        }
        catalog.join(window(4), window(2)).unwrap();
        catalog.validate().unwrap();
        catalog.unjoin(window(4)).unwrap();
        catalog.validate().unwrap();
        catalog.assign_floating(second, window(4)).unwrap();
        catalog.validate().unwrap();
        catalog.remove_window(window(3)).unwrap();
        catalog.validate().unwrap();
    }

}
