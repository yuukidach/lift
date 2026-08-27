use std::collections::{BTreeMap, BTreeSet};

use super::{ChangeSet, CoreState, Transition};
use crate::core::command::{
    Command, Direction, DisplayCommand, MissionControlCommand, WindowCommand, WorkspaceCommand,
};
use crate::core::effect::{DomainEvent, Effect, EffectOutcome, LayoutRequest, WindowFrame};
use crate::core::error::CoreError;
use crate::core::ids::{DisplayId, Generation, WindowId, WorkspaceId};
use crate::core::input::{
    DisplayObservation, DisplayTopologyObservation, Input, Observation, PlatformSnapshotObservation,
};
use crate::core::interaction::{DragObservation, MissionControlPhase};
use crate::core::rules::{RuleDecision, RuleSet, WindowIdentity, WorkspaceTarget};
use crate::core::snapshot::CoreSnapshot;

impl CoreState {
    pub fn transition(&mut self, input: Input) -> Result<Transition, CoreError> {
        let mut next = self.clone();
        let old_snapshot = self.snapshot();
        let mut effects = Vec::new();
        let mut events = Vec::new();
        let mut config_changed = false;

        match input {
            Input::Observation(Observation::PlatformSnapshot(observation)) => {
                next.apply_platform_snapshot(observation)?;
            }
            Input::Observation(Observation::DisplayTopology(observation)) => {
                next.apply_display_topology(observation)?;
            }
            Input::Observation(Observation::FocusChanged { window }) => {
                next.apply_focus_observation(window)?;
            }
            Input::Observation(Observation::Drag(observation)) => {
                next.apply_drag_observation(observation)?;
            }
            Input::Observation(Observation::MissionControl { active }) => {
                next.interactions.mission_control = if active {
                    MissionControlPhase::Active
                } else {
                    MissionControlPhase::Inactive
                };
            }
            Input::Command(command) => next.apply_command(command, &mut effects, &mut events)?,
            Input::EffectCompleted(completion) => {
                if !next.completion_is_current(&completion) {
                    return Err(CoreError::StaleGeneration {
                        expected: next.platform.generation,
                        received: completion.generation,
                    });
                }
                if let EffectOutcome::Failed { message, .. } = completion.outcome {
                    return Err(CoreError::PlatformEffectFailed {
                        effect: completion.effect_id,
                        message,
                    });
                }
            }
            Input::Timer(_) => {}
            Input::ConfigReloaded(config) => {
                if next.config != config {
                    next.rules = RuleSet::compile(config.window_rules.clone())?;
                    next.config = config;
                    next.reconcile_window_assignments()?;
                    config_changed = true;
                }
            }
        }

        next.validate()?;
        next.revision = next.revision.saturating_add(1);
        next.interactions.current_transaction.0 =
            next.interactions.current_transaction.0.saturating_add(1);
        let transaction = next.interactions.current_transaction;
        let snapshot = next.snapshot();
        let changes = ChangeSet::between(&old_snapshot, &snapshot, config_changed);
        effects.extend(layout_effects(&snapshot, &changes));
        events.push(DomainEvent::SnapshotPublished { revision: snapshot.revision });
        *self = next;
        Ok(Transition {
            transaction,
            changes,
            effects,
            events,
            snapshot,
        })
    }

    fn apply_focus_observation(&mut self, window: Option<WindowId>) -> Result<(), CoreError> {
        if let Some(window) = window {
            if !self.platform.managed.contains(&window) {
                return Err(CoreError::MissingWindow(window));
            }
            self.focus.focused_window = Some(window);
            self.note_focus(window)?;
        } else {
            self.focus.focused_window = None;
        }
        Ok(())
    }

    fn apply_platform_snapshot(
        &mut self,
        observation: PlatformSnapshotObservation,
    ) -> Result<(), CoreError> {
        let generation = observation.generation;
        let active_display = observation.active_display;
        let (displays, display_order) =
            self.validate_display_topology(generation, observation.displays, &active_display)?;
        let window_count = observation.windows.len();
        let windows = observation
            .windows
            .into_iter()
            .map(|window| (window.id, window))
            .collect::<BTreeMap<_, _>>();
        if windows.len() != window_count {
            return Err(CoreError::IncompleteObservation(
                "platform snapshot contains duplicate window identities".into(),
            ));
        }
        if displays.is_empty() && !windows.is_empty() {
            return Err(CoreError::IncompleteObservation(
                "window snapshot arrived without any displays".into(),
            ));
        }
        if let Some(window) = windows.values().find(|window| {
            window.display.as_ref().is_some_and(|display| !displays.contains_key(display))
        }) {
            return Err(CoreError::IncompleteObservation(format!(
                "window {:?} references an offline display",
                window.id
            )));
        }
        if let Some(focused) = observation.focused_window
            && !windows.contains_key(&focused)
        {
            return Err(CoreError::IncompleteObservation(format!(
                "focused window {focused:?} is absent from the platform snapshot"
            )));
        }

        self.commit_display_topology(generation, displays, display_order, active_display)?;
        for removed in self
            .platform
            .windows
            .keys()
            .filter(|window| !windows.contains_key(window))
            .copied()
            .collect::<Vec<_>>()
        {
            self.workspaces.remove_window(removed)?;
            self.platform.managed.remove(&removed);
            if self.focus.focused_window == Some(removed) {
                self.focus.focused_window = None;
            }
            self.focus.last_tiled_by_workspace.retain(|_, window| *window != removed);
            self.focus.last_floating_by_workspace.retain(|_, window| *window != removed);
        }
        self.platform.windows = windows;
        self.focus.focused_window = observation.focused_window;
        self.reconcile_window_assignments()?;
        for window in self.platform.windows.values() {
            if !self.workspaces.is_floating(window.id) {
                continue;
            }
            let Some(workspace) = self.workspaces.workspace_for_window(window.id) else {
                continue;
            };
            let Some(display) = self.workspaces.display_for_workspace(workspace) else {
                continue;
            };
            let Some(display_frame) =
                self.platform.displays.get(display).map(|display| display.frame)
            else {
                continue;
            };
            let overlap_width = (window.frame.origin.x + window.frame.size.width)
                .min(display_frame.origin.x + display_frame.size.width)
                - window.frame.origin.x.max(display_frame.origin.x);
            let overlap_height = (window.frame.origin.y + window.frame.size.height)
                .min(display_frame.origin.y + display_frame.size.height)
                - window.frame.origin.y.max(display_frame.origin.y);
            if overlap_width.max(0.0) * overlap_height.max(0.0) > 9.0 {
                self.workspaces.record_active_floating_position(window.id, window.frame)?;
            }
        }
        if let Some(window) = observation.focused_window {
            self.note_focus(window)?;
        }
        let drag = self.interactions.drag.snapshot();
        if drag
            .window
            .into_iter()
            .chain(drag.target)
            .any(|window| !self.platform.managed.contains(&window))
        {
            self.interactions.drag.reset();
        }
        Ok(())
    }

    fn apply_drag_observation(&mut self, observation: DragObservation) -> Result<(), CoreError> {
        match observation {
            DragObservation::Resized { window, old_frame, new_frame } => {
                if self.workspaces.is_floating(window) {
                    return Err(CoreError::InvalidCommand(
                        "floating windows cannot resize the BSP tree".into(),
                    ));
                }
                self.workspaces.resize_from_frames(window, old_frame, new_frame)?;
            }
            DragObservation::Updated { window, frame, candidates } => {
                let workspace = self
                    .workspaces
                    .workspace_for_window(window)
                    .ok_or(CoreError::MissingWindow(window))?;
                if self.workspaces.is_floating(window) {
                    return Err(CoreError::InvalidCommand(
                        "floating windows cannot start a BSP drag swap".into(),
                    ));
                }
                for candidate in &candidates {
                    if self.workspaces.workspace_for_window(candidate.window) != Some(workspace)
                        || self.workspaces.is_floating(candidate.window)
                    {
                        return Err(CoreError::InvalidCommand(format!(
                            "drag candidate {:?} is not tiled in the dragged workspace",
                            candidate.window
                        )));
                    }
                }
                self.interactions.drag.update(
                    window,
                    frame,
                    &candidates,
                    self.config.drag_swap_fraction,
                );
            }
            DragObservation::Committed { window } => {
                if let Some((first, second)) = self.interactions.drag.commit(window) {
                    self.workspaces.swap(first, second)?;
                    self.workspaces.select_tiled_window(first)?;
                }
            }
            DragObservation::Cancelled => self.interactions.drag.reset(),
        }
        Ok(())
    }

    fn apply_display_topology(
        &mut self,
        observation: DisplayTopologyObservation,
    ) -> Result<(), CoreError> {
        let (displays, display_order) = self.validate_display_topology(
            observation.generation,
            observation.displays,
            &observation.active_display,
        )?;
        self.commit_display_topology(
            observation.generation,
            displays,
            display_order,
            observation.active_display,
        )
    }

    fn validate_display_topology(
        &self,
        generation: Generation,
        observations: Vec<DisplayObservation>,
        active_display: &Option<DisplayId>,
    ) -> Result<(BTreeMap<DisplayId, DisplayObservation>, Vec<DisplayId>), CoreError> {
        if generation.0 < self.platform.generation.0 {
            return Err(CoreError::StaleGeneration {
                expected: self.platform.generation,
                received: generation,
            });
        }
        let display_count = observations.len();
        let display_order =
            observations.iter().map(|display| display.id.clone()).collect::<Vec<_>>();
        let displays = observations
            .into_iter()
            .map(|display| (display.id.clone(), display))
            .collect::<BTreeMap<_, _>>();
        if displays.len() != display_count {
            return Err(CoreError::IncompleteObservation(
                "display topology contains duplicate display identities".into(),
            ));
        }
        if let Some(active_display) = active_display
            && !displays.contains_key(active_display)
        {
            return Err(CoreError::IncompleteObservation(format!(
                "active display {active_display:?} is absent from the topology snapshot"
            )));
        }
        let mut spaces = BTreeSet::new();
        if displays
            .values()
            .filter_map(|display| display.space)
            .any(|space| !spaces.insert(space))
        {
            return Err(CoreError::IncompleteObservation(
                "multiple displays report the same native SpaceId".into(),
            ));
        }
        Ok((displays, display_order))
    }

    fn commit_display_topology(
        &mut self,
        generation: Generation,
        displays: BTreeMap<DisplayId, DisplayObservation>,
        display_order: Vec<DisplayId>,
        active_display: Option<DisplayId>,
    ) -> Result<(), CoreError> {
        let migration_receiver = self
            .config
            .display_migration_priority
            .iter()
            .find(|display| displays.contains_key(*display))
            .or_else(|| display_order.first());
        self.workspaces.reconcile_displays(&display_order, migration_receiver)?;
        self.platform.generation = generation;
        self.platform.display_order = display_order;
        self.platform.active_display = active_display;
        self.platform.displays = displays;
        Ok(())
    }

    fn reconcile_window_assignments(&mut self) -> Result<(), CoreError> {
        let windows = self.platform.windows.values().cloned().collect::<Vec<_>>();
        for window in windows {
            let decision = self.rules.decide(WindowIdentity {
                app_id: window.app_id.as_deref(),
                app_name: window.app_name.as_deref(),
                title: Some(&window.title),
                ax_role: window.ax_role.as_deref(),
                ax_subrole: window.ax_subrole.as_deref(),
            });
            match decision {
                RuleDecision::Unmanaged { .. } => {
                    self.platform.managed.remove(&window.id);
                    self.workspaces.remove_window(window.id)?;
                }
                RuleDecision::Managed {
                    workspace,
                    floating,
                    rule_index,
                } => {
                    let display = window
                        .display
                        .clone()
                        .or_else(|| {
                            self.workspaces.workspace_for_window(window.id).and_then(|workspace| {
                                self.workspaces.display_for_workspace(workspace).cloned()
                            })
                        })
                        .ok_or_else(|| {
                            CoreError::IncompleteObservation(format!(
                                "managed window {:?} has no display",
                                window.id
                            ))
                        })?;
                    let existing = self.workspaces.workspace_for_window(window.id);
                    let floating = if rule_index.is_none() {
                        existing.is_some_and(|_| self.workspaces.is_floating(window.id))
                    } else {
                        floating
                    };
                    // Workspace rules choose a window's initial workspace. An
                    // existing assignment is authoritative so a later platform
                    // observation cannot undo an explicit move by the user.
                    let target = if let Some(existing) = existing {
                        existing
                    } else {
                        match workspace {
                            WorkspaceTarget::Number(number) => {
                                if let Some(workspace) = self.workspaces.workspace_by_number(number)
                                {
                                    workspace
                                } else {
                                    self.workspaces.create_numbered(number, display.clone())?
                                }
                            }
                            WorkspaceTarget::Name(name) => self
                                .workspaces
                                .workspace_by_name(&display, &name)
                                .or_else(|| self.workspaces.active_workspace(&display))
                                .ok_or_else(|| {
                                    CoreError::InvalidCommand(format!(
                                        "display {display:?} has no active workspace"
                                    ))
                                })?,
                            WorkspaceTarget::Current => {
                                self.workspaces.active_workspace(&display).ok_or_else(|| {
                                    CoreError::InvalidCommand(format!(
                                        "display {display:?} has no active workspace"
                                    ))
                                })?
                            }
                        }
                    };
                    if floating {
                        self.workspaces.assign_floating(target, window.id)?;
                    } else {
                        let after = self.focus.focused_window.filter(|focused| {
                            self.workspaces.workspace_for_window(*focused) == Some(target)
                                && !self.workspaces.is_floating(*focused)
                        });
                        self.workspaces.assign_tiled(target, window.id, after)?;
                    }
                    self.platform.managed.insert(window.id);
                }
            }
        }
        Ok(())
    }

    fn apply_command(
        &mut self,
        command: Command,
        effects: &mut Vec<Effect>,
        events: &mut Vec<DomainEvent>,
    ) -> Result<(), CoreError> {
        match command {
            Command::Window(WindowCommand::Activate { window }) => {
                self.command_window(Some(window))?;
                self.focus_window(window, effects, events)?;
            }
            Command::Workspace(WorkspaceCommand::Activate(number)) => {
                let workspace = self.workspaces.workspace_by_number(number).ok_or_else(|| {
                    CoreError::InvalidCommand(format!(
                        "workspace number {} does not exist",
                        number.get()
                    ))
                })?;
                let display = self
                    .workspaces
                    .display_for_workspace(workspace)
                    .cloned()
                    .ok_or(CoreError::WorkspaceConflict(workspace))?;
                self.activate_workspace(display, workspace, events)?;
            }
            Command::Workspace(WorkspaceCommand::ActivateOrCreate {
                workspace: number,
                display,
            }) => {
                self.require_online_display(&display)?;
                let workspace = match self.workspaces.workspace_by_number(number) {
                    Some(workspace) => workspace,
                    None => self.workspaces.create_numbered(number, display.clone())?,
                };
                let owner = self
                    .workspaces
                    .display_for_workspace(workspace)
                    .cloned()
                    .ok_or(CoreError::WorkspaceConflict(workspace))?;
                self.activate_workspace(owner, workspace, events)?;
            }
            Command::Workspace(WorkspaceCommand::Create { display }) => {
                self.require_online_display(&display)?;
                self.workspaces.create_next(display)?;
            }
            Command::Workspace(WorkspaceCommand::MoveWindow { workspace, window }) => {
                let window = window.or(self.focus.focused_window).ok_or_else(|| {
                    CoreError::InvalidCommand("no window was selected to move".into())
                })?;
                let source = self
                    .workspaces
                    .workspace_for_window(window)
                    .ok_or(CoreError::MissingWindow(window))?;
                let display = self
                    .workspaces
                    .display_for_workspace(source)
                    .cloned()
                    .ok_or(CoreError::WorkspaceConflict(source))?;
                let target = match self.workspaces.workspace_by_number(workspace) {
                    Some(workspace) => workspace,
                    None => self.workspaces.create_numbered(workspace, display)?,
                };
                self.workspaces.move_window(window, target)?;
                self.workspaces.destroy_if_ephemeral(source)?;
            }
            Command::Workspace(WorkspaceCommand::MoveWindowToHidden { display, window }) => {
                self.require_online_display(&display)?;
                let window = window.or(self.focus.focused_window).ok_or_else(|| {
                    CoreError::InvalidCommand("no window was selected to move".into())
                })?;
                let (source, _) = self.workspaces.move_window_to_hidden(&display, window)?;
                self.workspaces.destroy_if_ephemeral(source)?;
            }
            Command::Workspace(WorkspaceCommand::Next { display, skip_empty }) => {
                self.step_workspace(display, true, skip_empty, events)?;
            }
            Command::Workspace(WorkspaceCommand::Previous { display, skip_empty }) => {
                self.step_workspace(display, false, skip_empty, events)?;
            }
            Command::Workspace(WorkspaceCommand::Last { display }) => {
                self.require_online_display(&display)?;
                let workspace = self.workspaces.last_workspace(&display).ok_or_else(|| {
                    CoreError::InvalidCommand("display has no previous workspace".into())
                })?;
                self.activate_workspace(display, workspace, events)?;
            }
            Command::Workspace(WorkspaceCommand::ToggleHidden { display }) => {
                self.require_online_display(&display)?;
                let previous = self.workspaces.active_workspace(&display);
                let workspace = self.workspaces.toggle_hidden(&display)?;
                if previous != Some(workspace) {
                    events.push(DomainEvent::WorkspaceChanged { display, workspace });
                }
            }
            Command::Display(DisplayCommand::MoveWindowTo { display, window }) => {
                self.require_online_display(&display)?;
                let window = self.command_window(window)?;
                let source = self
                    .workspaces
                    .workspace_for_window(window)
                    .ok_or(CoreError::MissingWindow(window))?;
                let target = self.workspaces.active_workspace(&display).ok_or_else(|| {
                    CoreError::IncompleteObservation(format!(
                        "display {display:?} has no active workspace"
                    ))
                })?;
                self.workspaces.move_window(window, target)?;
                self.workspaces.destroy_if_ephemeral(source)?;
            }
            Command::Window(WindowCommand::Focus { direction, window }) => {
                let window = self.command_window(window)?;
                let target = match self.directional_neighbor(window, direction)? {
                    Some(target) => Some(target),
                    None => self.cross_display_focus_target(window, direction)?,
                }
                .ok_or_else(|| CoreError::InvalidCommand("focus reached a boundary".into()))?;
                self.focus_window(target, effects, events)?;
            }
            Command::Window(WindowCommand::Move { direction, window }) => {
                let window = self.command_window(window)?;
                if self.workspaces.is_floating(window) {
                    return Err(CoreError::InvalidCommand(
                        "floating windows cannot move inside the BSP tree".into(),
                    ));
                }
                if let Some(target) = self.directional_neighbor(window, direction)? {
                    self.workspaces.swap(window, target)?;
                    self.workspaces.select_tiled_window(window)?;
                } else {
                    let source = self
                        .workspaces
                        .workspace_for_window(window)
                        .ok_or(CoreError::MissingWindow(window))?;
                    let source_display = self
                        .workspaces
                        .display_for_workspace(source)
                        .cloned()
                        .ok_or(CoreError::WorkspaceConflict(source))?;
                    let target_display =
                        self.adjacent_display(&source_display, direction).ok_or_else(|| {
                            CoreError::InvalidCommand("move reached a boundary".into())
                        })?;
                    let target =
                        self.workspaces.active_workspace(&target_display).ok_or_else(|| {
                            CoreError::IncompleteObservation(format!(
                                "display {target_display:?} has no active workspace"
                            ))
                        })?;
                    self.workspaces.move_window(window, target)?;
                    self.workspaces.destroy_if_ephemeral(source)?;
                }
            }
            Command::Window(WindowCommand::Resize { amount, window }) => {
                let window = self.command_window(window)?;
                if !self.workspaces.is_floating(window) {
                    self.workspaces.resize(window, amount)?;
                }
            }
            Command::Window(WindowCommand::ResizeDirectional { direction, window }) => {
                let window = self.command_window(window)?;
                if !self.workspaces.is_floating(window) {
                    self.workspaces.resize_directional(window, direction)?;
                }
            }
            Command::Window(WindowCommand::ToggleFocusLayer { window }) => {
                let window = self.command_window(window)?;
                let workspace = self
                    .workspaces
                    .workspace_for_window(window)
                    .ok_or(CoreError::MissingWindow(window))?;
                let target = if self.workspaces.is_floating(window) {
                    self.focus
                        .last_tiled_by_workspace
                        .get(&workspace)
                        .copied()
                        .filter(|candidate| {
                            self.workspaces.workspace_for_window(*candidate) == Some(workspace)
                                && !self.workspaces.is_floating(*candidate)
                        })
                        .or_else(|| {
                            self.workspaces
                                .selected_tiled_windows(workspace)
                                .ok()?
                                .into_iter()
                                .next()
                        })
                } else {
                    self.focus
                        .last_floating_by_workspace
                        .get(&workspace)
                        .copied()
                        .filter(|candidate| {
                            self.workspaces.workspace_for_window(*candidate) == Some(workspace)
                                && self.workspaces.is_floating(*candidate)
                        })
                        .or_else(|| {
                            self.workspaces.floating_windows(workspace).ok()?.into_iter().next()
                        })
                }
                .ok_or_else(|| {
                    CoreError::InvalidCommand("the opposite focus layer is empty".into())
                })?;
                self.focus_window(target, effects, events)?;
            }
            Command::Window(WindowCommand::ToggleFloating { window }) => {
                let window = self.command_window(window)?;
                let workspace = self
                    .workspaces
                    .workspace_for_window(window)
                    .ok_or(CoreError::MissingWindow(window))?;
                if self.workspaces.is_floating(window) {
                    self.workspaces.assign_tiled(workspace, window, None)?;
                } else {
                    self.workspaces.assign_floating(workspace, window)?;
                }
            }
            Command::Window(WindowCommand::ToggleFullscreen { window, within_gaps }) => {
                let window = self.command_window(window)?;
                self.workspaces.toggle_fullscreen(window, within_gaps)?;
            }
            Command::Window(WindowCommand::Join { direction, window }) => {
                let window = self.command_window(window)?;
                let target = self
                    .directional_neighbor(window, direction)?
                    .ok_or_else(|| CoreError::InvalidCommand("join reached a boundary".into()))?;
                self.workspaces.join(window, target)?;
            }
            Command::Window(WindowCommand::Unjoin { window }) => {
                let window = self.command_window(window)?;
                self.workspaces.unjoin(window)?;
            }
            Command::Window(WindowCommand::ToggleOrientation { window }) => {
                let window = self.command_window(window)?;
                self.workspaces.toggle_orientation(window)?;
            }
            Command::Window(WindowCommand::Swap { first, second }) => {
                self.workspaces.swap(first, second)?;
            }
            Command::Window(WindowCommand::Next { window }) => {
                let window = self.command_window(window)?;
                let target = self.cycle_window(window, true)?;
                self.focus_window(target, effects, events)?;
            }
            Command::Window(WindowCommand::Previous { window }) => {
                let window = self.command_window(window)?;
                let target = self.cycle_window(window, false)?;
                self.focus_window(target, effects, events)?;
            }
            Command::Window(WindowCommand::Close(window)) => {
                let window = self.command_window(window)?;
                effects.push(Effect::CloseWindow(window));
            }
            Command::MissionControl(command) => {
                self.interactions.mission_control = match command {
                    MissionControlCommand::ShowAll => MissionControlPhase::ShowAllRequested,
                    MissionControlCommand::ShowCurrent => MissionControlPhase::ShowCurrentRequested,
                    MissionControlCommand::Dismiss => MissionControlPhase::DismissRequested,
                };
            }
            Command::SaveAndExit => {
                effects.push(Effect::Save(self.persisted_state()));
                effects.push(Effect::Shutdown(crate::core::effect::ShutdownReason::Requested));
            }
            unsupported => {
                return Err(CoreError::UnsupportedCommand(format!("{unsupported:?}")));
            }
        }
        Ok(())
    }

    fn step_workspace(
        &mut self,
        display: DisplayId,
        forward: bool,
        skip_empty: bool,
        events: &mut Vec<DomainEvent>,
    ) -> Result<(), CoreError> {
        self.require_online_display(&display)?;
        let current = self.workspaces.active_workspace(&display).ok_or_else(|| {
            CoreError::IncompleteObservation(format!("display {display:?} has no active workspace"))
        })?;
        let workspace = self
            .workspaces
            .step_workspace(&display, current, forward, skip_empty)
            .ok_or_else(|| CoreError::InvalidCommand("no matching workspace exists".into()))?;
        self.activate_workspace(display, workspace, events)?;
        Ok(())
    }

    fn command_window(&self, window: Option<WindowId>) -> Result<WindowId, CoreError> {
        let window = window
            .or(self.focus.focused_window)
            .ok_or_else(|| CoreError::InvalidCommand("no window was selected".into()))?;
        if self.platform.managed.contains(&window) {
            Ok(window)
        } else {
            Err(CoreError::MissingWindow(window))
        }
    }

    fn focus_window(
        &mut self,
        window: WindowId,
        effects: &mut Vec<Effect>,
        events: &mut Vec<DomainEvent>,
    ) -> Result<(), CoreError> {
        if !self.workspaces.is_floating(window) {
            self.workspaces.select_tiled_window(window)?;
        }
        if self.focus.focused_window != Some(window) {
            self.focus.focused_window = Some(window);
            events.push(DomainEvent::FocusChanged { window: Some(window) });
        }
        self.note_focus(window)?;
        effects.push(Effect::FocusWindow(window));
        effects.push(Effect::RaiseWindow(window));
        Ok(())
    }

    fn note_focus(&mut self, window: WindowId) -> Result<(), CoreError> {
        let workspace = self
            .workspaces
            .workspace_for_window(window)
            .ok_or(CoreError::MissingWindow(window))?;
        if self.workspaces.is_floating(window) {
            self.focus.last_floating_by_workspace.insert(workspace, window);
        } else {
            self.focus.last_tiled_by_workspace.insert(workspace, window);
        }
        Ok(())
    }

    fn cycle_window(&self, window: WindowId, forward: bool) -> Result<WindowId, CoreError> {
        let workspace = self
            .workspaces
            .workspace_for_window(window)
            .ok_or(CoreError::MissingWindow(window))?;
        let snapshot = self.snapshot();
        let workspace_snapshot = snapshot
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .ok_or(CoreError::WorkspaceConflict(workspace))?;
        let windows = if self.workspaces.is_floating(window) {
            workspace_snapshot.floating_windows.clone()
        } else {
            self.workspaces.selected_tiled_windows(workspace)?
        };
        let position = windows
            .iter()
            .position(|candidate| *candidate == window)
            .ok_or(CoreError::MissingWindow(window))?;
        let next = if forward {
            (position + 1) % windows.len()
        } else {
            (position + windows.len() - 1) % windows.len()
        };
        Ok(windows[next])
    }

    fn directional_neighbor(
        &self,
        window: WindowId,
        direction: Direction,
    ) -> Result<Option<WindowId>, CoreError> {
        let workspace = self
            .workspaces
            .workspace_for_window(window)
            .ok_or(CoreError::MissingWindow(window))?;
        let snapshot = self.snapshot();
        let workspace_snapshot = snapshot
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .ok_or(CoreError::WorkspaceConflict(workspace))?;
        let candidates = if self.workspaces.is_floating(window) {
            workspace_snapshot.floating_windows.clone()
        } else {
            self.workspaces.selected_tiled_windows(workspace)?
        };
        let frame_for = |candidate: WindowId| {
            workspace_snapshot
                .layout_frames
                .get(&candidate)
                .copied()
                .or_else(|| self.platform.windows.get(&candidate).map(|window| window.frame))
        };
        let Some(current) = frame_for(window) else {
            return Ok(None);
        };
        let current_x = current.origin.x + current.size.width / 2.0;
        let current_y = current.origin.y + current.size.height / 2.0;
        Ok(candidates
            .into_iter()
            .filter(|candidate| *candidate != window)
            .filter_map(|candidate| {
                let frame = frame_for(candidate)?;
                let dx = frame.origin.x + frame.size.width / 2.0 - current_x;
                let dy = frame.origin.y + frame.size.height / 2.0 - current_y;
                let in_direction = match direction {
                    Direction::Left => dx < 0.0,
                    Direction::Right => dx > 0.0,
                    Direction::Up => dy < 0.0,
                    Direction::Down => dy > 0.0,
                };
                in_direction.then_some((candidate, dx * dx + dy * dy))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(candidate, _)| candidate))
    }

    fn adjacent_display(&self, display: &DisplayId, direction: Direction) -> Option<DisplayId> {
        let current = self.platform.displays.get(display)?.frame;
        let current_x = current.origin.x + current.size.width / 2.0;
        let current_y = current.origin.y + current.size.height / 2.0;
        self.platform
            .displays
            .values()
            .filter(|candidate| candidate.id != *display)
            .filter_map(|candidate| {
                let candidate_x = candidate.frame.origin.x + candidate.frame.size.width / 2.0;
                let candidate_y = candidate.frame.origin.y + candidate.frame.size.height / 2.0;
                let dx = candidate_x - current_x;
                let dy = candidate_y - current_y;
                let in_direction = match direction {
                    Direction::Left => dx < 0.0,
                    Direction::Right => dx > 0.0,
                    Direction::Up => dy < 0.0,
                    Direction::Down => dy > 0.0,
                };
                in_direction.then_some((candidate.id.clone(), dx * dx + dy * dy))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(display, _)| display)
    }

    fn cross_display_focus_target(
        &self,
        window: WindowId,
        direction: Direction,
    ) -> Result<Option<WindowId>, CoreError> {
        let workspace = self
            .workspaces
            .workspace_for_window(window)
            .ok_or(CoreError::MissingWindow(window))?;
        let display = self
            .workspaces
            .display_for_workspace(workspace)
            .ok_or(CoreError::WorkspaceConflict(workspace))?;
        let Some(display) = self.adjacent_display(display, direction) else {
            return Ok(None);
        };
        let target = self.workspaces.active_workspace(&display).ok_or_else(|| {
            CoreError::IncompleteObservation(format!("display {display:?} has no active workspace"))
        })?;
        if let Some(window) = self.workspaces.selected_tiled_windows(target)?.into_iter().next() {
            return Ok(Some(window));
        }
        Ok(self
            .snapshot()
            .workspaces
            .iter()
            .find(|workspace| workspace.id == target)
            .and_then(|workspace| workspace.floating_windows.first().copied()))
    }

    fn activate_workspace(
        &mut self,
        display: DisplayId,
        workspace: WorkspaceId,
        events: &mut Vec<DomainEvent>,
    ) -> Result<(), CoreError> {
        if self.workspaces.active_workspace(&display) == Some(workspace) {
            return Ok(());
        }
        let previous = self.workspaces.active_workspace(&display);
        self.workspaces.activate(&display, workspace)?;
        if let Some(previous) = previous {
            self.workspaces.destroy_if_ephemeral(previous)?;
        }
        events.push(DomainEvent::WorkspaceChanged { display, workspace });
        Ok(())
    }

    fn require_online_display(&self, display: &DisplayId) -> Result<(), CoreError> {
        if self.platform.displays.contains_key(display) {
            Ok(())
        } else {
            Err(CoreError::InvalidCommand(format!(
                "display {display:?} is not online"
            )))
        }
    }

    fn validate(&self) -> Result<(), CoreError> {
        self.workspaces.validate()?;
        if let Some(window) = self.platform.managed.iter().find(|window| {
            !self.platform.windows.contains_key(window)
                || self.workspaces.workspace_for_window(**window).is_none()
        }) {
            return Err(CoreError::InvariantViolation(format!(
                "managed window {window:?} is absent or unassigned"
            )));
        }
        for workspace in self.workspaces.snapshots()? {
            for window in workspace
                .groups
                .iter()
                .flat_map(|group| group.windows.iter())
                .chain(workspace.floating_windows.iter())
            {
                if !self.platform.managed.contains(window) {
                    return Err(CoreError::InvariantViolation(format!(
                        "workspace references unmanaged window {window:?}"
                    )));
                }
            }
        }
        Ok(())
    }
}

fn layout_effects(snapshot: &CoreSnapshot, changes: &ChangeSet) -> Vec<Effect> {
    let active = snapshot
        .displays
        .iter()
        .filter_map(|display| display.active_workspace)
        .collect::<BTreeSet<_>>();
    snapshot
        .workspaces
        .iter()
        .filter(|workspace| {
            active.contains(&workspace.id)
                && (changes.workspaces.contains(&workspace.id)
                    || changes.displays.contains(&workspace.display))
        })
        .map(|workspace| {
            Effect::ApplyLayout(LayoutRequest {
                workspace: workspace.id,
                frames: workspace
                    .layout_frames
                    .iter()
                    .map(|(window, frame)| WindowFrame { window: *window, frame: *frame })
                    .collect(),
            })
        })
        .collect()
}

impl ChangeSet {
    fn between(old: &CoreSnapshot, new: &CoreSnapshot, config_changed: bool) -> Self {
        let old_displays = old
            .displays
            .iter()
            .map(|display| (display.id.clone(), display))
            .collect::<BTreeMap<_, _>>();
        let new_displays = new
            .displays
            .iter()
            .map(|display| (display.id.clone(), display))
            .collect::<BTreeMap<_, _>>();
        let displays = changed_keys(&old_displays, &new_displays);

        let old_workspaces = old
            .workspaces
            .iter()
            .map(|workspace| (workspace.id, workspace))
            .collect::<BTreeMap<_, _>>();
        let new_workspaces = new
            .workspaces
            .iter()
            .map(|workspace| (workspace.id, workspace))
            .collect::<BTreeMap<_, _>>();
        let workspaces = changed_keys(&old_workspaces, &new_workspaces);

        let old_windows =
            old.windows.iter().map(|window| (window.id, window)).collect::<BTreeMap<_, _>>();
        let new_windows =
            new.windows.iter().map(|window| (window.id, window)).collect::<BTreeMap<_, _>>();
        let windows = changed_keys(&old_windows, &new_windows);

        let focus_changed = old.focused_window != new.focused_window;
        let ui_changed = config_changed
            || !displays.is_empty()
            || !workspaces.is_empty()
            || !windows.is_empty()
            || focus_changed;
        Self {
            displays,
            workspaces,
            windows,
            focus_changed,
            config_changed,
            ui_changed,
        }
    }
}

fn changed_keys<Key, Value>(
    old: &BTreeMap<Key, Value>,
    new: &BTreeMap<Key, Value>,
) -> BTreeSet<Key>
where
    Key: Clone + Ord,
    Value: PartialEq,
{
    old.keys()
        .chain(new.keys())
        .filter(|key| old.get(*key) != new.get(*key))
        .cloned()
        .collect()
}
