use tracing::{error, info, warn};

use super::super::ScreenInfo;
use crate::actor::app::{AppThreadHandle, Quiet, WindowId};
use crate::actor::reactor::transaction_manager::TransactionId;
use crate::actor::reactor::{
    Command, DisplaySelector, LayoutEvent, Reactor, ReactorCommand, WorkspaceSwitchOrigin,
};
use crate::actor::stack_line::Event as StackLineEvent;
use crate::actor::wm_controller::WmEvent;
use crate::actor::{menu_bar, raise_manager};
use crate::common::collections::HashMap;
use crate::common::config::Config;
use crate::common::log::{handle_command, MetricsCommand};
use crate::core::command::{
    Command as CoreCommand, Direction as CoreDirection, DisplayCommand as CoreDisplayCommand,
    MissionControlCommand as CoreMissionControlCommand,
    WindowCommand as CoreWindowCommand, WorkspaceCommand as CoreWorkspaceCommand,
};
use crate::core::error::CoreError;
use crate::core::effect::Effect as CoreEffect;
use crate::core::ids::{DisplayId as CoreDisplayId, WorkspaceNumber as CoreWorkspaceNumber};
use crate::model::layout::{EventResponse, LayoutCommand};
use crate::sys::app::pid_t;
use crate::sys::window_server::{self as window_server, WindowServerId};

pub struct CommandEventHandler;

impl CommandEventHandler {
    fn core_window(window: WindowId) -> crate::core::ids::WindowId {
        crate::core::ids::WindowId::new(
            crate::core::ids::ApplicationId(window.pid),
            window.idx,
        )
    }

    fn core_direction(direction: crate::model::layout::Direction) -> CoreDirection {
        match direction {
            crate::model::layout::Direction::Left => CoreDirection::Left,
            crate::model::layout::Direction::Right => CoreDirection::Right,
            crate::model::layout::Direction::Up => CoreDirection::Up,
            crate::model::layout::Direction::Down => CoreDirection::Down,
        }
    }

    fn actor_window(window: crate::core::ids::WindowId) -> WindowId {
        WindowId::new(window.application.0, window.index.get())
    }

    fn core_workspace_number(number: usize) -> Result<CoreWorkspaceNumber, CoreError> {
        let one_based = number
            .checked_add(1)
            .and_then(|number| u8::try_from(number).ok())
            .ok_or_else(|| {
                CoreError::InvalidCommand(format!("workspace slot {number} is out of range"))
            })?;
        CoreWorkspaceNumber::try_from(one_based)
            .map_err(|error| CoreError::InvalidCommand(error.to_string()))
    }

    fn transition_core_workspace_command(
        reactor: &mut Reactor,
        command_space: Option<crate::sys::screen::SpaceId>,
        command: &LayoutCommand,
    ) -> Result<EventResponse, CoreError> {
        let display = || {
            let space = command_space.ok_or_else(|| {
                CoreError::IncompleteObservation(
                    "workspace command has no active native Space".into(),
                )
            })?;
            reactor
                .display_uuid_for_space(space)
                .map(CoreDisplayId)
                .ok_or_else(|| {
                    CoreError::IncompleteObservation(format!(
                        "workspace command Space {space:?} has no display identity"
                    ))
                })
        };

        let command = match command {
            LayoutCommand::NextWorkspace(skip_empty) => {
                CoreWorkspaceCommand::Next {
                    display: display()?,
                    skip_empty: skip_empty.unwrap_or(false),
                }
            }
            LayoutCommand::PrevWorkspace(skip_empty) => {
                CoreWorkspaceCommand::Previous {
                    display: display()?,
                    skip_empty: skip_empty.unwrap_or(false),
                }
            }
            LayoutCommand::SwitchToWorkspace(index) => {
                let display = display()?;
                let snapshot = reactor
                    .core_state
                    .as_ref()
                    .map(|core| core.snapshot())
                    .unwrap_or_else(|| reactor.snapshot_store.load());
                let mut workspaces = snapshot
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.display == display)
                    .map(|workspace| workspace.number)
                    .collect::<Vec<_>>();
                workspaces.sort_unstable();
                let number = workspaces.get(*index).copied().ok_or_else(|| {
                    CoreError::InvalidCommand(format!(
                        "display {display:?} has no workspace at index {index}"
                    ))
                })?;
                CoreWorkspaceCommand::Activate(number)
            }
            LayoutCommand::CreateWorkspace => CoreWorkspaceCommand::Create {
                display: display()?,
            },
            LayoutCommand::SwitchToLastWorkspace => CoreWorkspaceCommand::Last {
                display: display()?,
            },
            unsupported => {
                return Err(CoreError::UnsupportedCommand(format!(
                    "{unsupported:?} is not a workspace reducer command"
                )));
            }
        };

        let transition = reactor.transition_core_command(CoreCommand::Workspace(command))?;
        Ok(Self::response_from_core_transition(reactor, &transition))
    }

    fn transition_core_window_move(
        reactor: &mut Reactor,
        workspace: usize,
        window: WindowId,
    ) -> Result<(), CoreError> {
        let workspace = Self::core_workspace_number(workspace)?;
        let window = Self::core_window(window);
        reactor.transition_core_command(CoreCommand::Workspace(
            CoreWorkspaceCommand::MoveWindow {
                workspace,
                window: Some(window),
            },
        ))?;
        Ok(())
    }

    fn transition_core_layout_command(
        reactor: &mut Reactor,
        command: &LayoutCommand,
    ) -> Result<Option<EventResponse>, CoreError> {
        let focused = reactor.focused_window_for_command().map(Self::core_window);
        let boundary_hit = match command {
            LayoutCommand::MoveFocus(direction) | LayoutCommand::MoveNode(direction) => {
                Some(*direction)
            }
            _ => None,
        };
        let core_command = match command {
            LayoutCommand::NextWindow => CoreWindowCommand::Next { window: focused },
            LayoutCommand::PrevWindow => CoreWindowCommand::Previous { window: focused },
            LayoutCommand::MoveFocus(direction) => CoreWindowCommand::Focus {
                direction: Self::core_direction(*direction),
                window: focused,
            },
            LayoutCommand::MoveNode(direction) => CoreWindowCommand::Move {
                direction: Self::core_direction(*direction),
                window: focused,
            },
            LayoutCommand::ResizeWindowGrow => {
                CoreWindowCommand::Resize { amount: 0.05, window: focused }
            }
            LayoutCommand::ResizeWindowShrink => {
                CoreWindowCommand::Resize { amount: -0.05, window: focused }
            }
            LayoutCommand::ResizeWindowBy { amount } => {
                CoreWindowCommand::Resize { amount: *amount, window: focused }
            }
            LayoutCommand::ToggleFocusFloating => {
                CoreWindowCommand::ToggleFocusLayer { window: focused }
            }
            LayoutCommand::ToggleFullscreen => CoreWindowCommand::ToggleFullscreen {
                window: focused,
                within_gaps: false,
            },
            LayoutCommand::ToggleFullscreenWithinGaps => {
                CoreWindowCommand::ToggleFullscreen {
                    window: focused,
                    within_gaps: true,
                }
            }
            LayoutCommand::JoinWindow(direction) => CoreWindowCommand::Join {
                direction: Self::core_direction(*direction),
                window: focused,
            },
            LayoutCommand::UnjoinWindows => CoreWindowCommand::Unjoin { window: focused },
            LayoutCommand::ToggleOrientation => {
                CoreWindowCommand::ToggleOrientation { window: focused }
            }
            LayoutCommand::SwapWindows(first, second) => CoreWindowCommand::Swap {
                first: Self::core_window(*first),
                second: Self::core_window(*second),
            },
            _ => return Ok(None),
        };
        match reactor.transition_core_command(CoreCommand::Window(core_command)) {
            Ok(transition) => {
                let mut response = EventResponse::default();
                for effect in transition.effects {
                    match effect {
                        CoreEffect::FocusWindow(window) => {
                            response.focus_window = Some(Self::actor_window(window));
                        }
                        CoreEffect::RaiseWindow(window) => {
                            let window = Self::actor_window(window);
                            if !response.raise_windows.contains(&window) {
                                response.raise_windows.push(window);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Some(response))
            }
            Err(CoreError::InvalidCommand(message)) if message.ends_with("reached a boundary") => {
                Ok(Some(EventResponse {
                    boundary_hit,
                    ..EventResponse::default()
                }))
            }
            Err(error) => Err(error),
        }
    }

    fn response_from_core_transition(
        reactor: &Reactor,
        transition: &crate::core::state::Transition,
    ) -> EventResponse {
        reactor.layout_response_for_transition(transition)
    }

    fn assigned_space_for_window(
        reactor: &Reactor,
        window_id: WindowId,
    ) -> Option<crate::sys::screen::SpaceId> {
        reactor
            .workspace_for_window(window_id)
            .and_then(|workspace| reactor.space_for_workspace(workspace))
    }

    fn resolve_current_window_for_command(
        reactor: &Reactor,
        command_space: Option<crate::sys::screen::SpaceId>,
        prefer_window_under_cursor: bool,
        layout_focus: Option<WindowId>,
    ) -> Option<WindowId> {
        let main_window = reactor.main_window();

        // AXMainWindow can lag behind Lift's own focus selection (in
        // particular immediately after cycling between two windows owned by
        // the same application).  When both observations refer to the same
        // app, prefer the layout focus once both windows are known to the
        // workspace model.  An unassigned main window is different: it is
        // commonly a newly-created window racing discovery, and must retain
        // priority so the pending workspace target can be recorded for it.
        let app_focus = match (main_window, layout_focus) {
            (Some(main), Some(layout)) if main.pid == layout.pid => {
                let main_is_assigned = reactor.workspace_for_window(main).is_some();
                let layout_is_assigned = reactor.workspace_for_window(layout).is_some();
                if main_is_assigned && layout_is_assigned {
                    Some(layout)
                } else {
                    Some(main)
                }
            }
            (Some(main), _) => Some(main),
            (None, Some(layout)) => Some(layout),
            (None, None) => None,
        };

        let preferred_window = if prefer_window_under_cursor {
            reactor.window_id_under_cursor().or(app_focus)
        } else {
            app_focus.or_else(|| reactor.window_id_under_cursor())
        };

        preferred_window
            .or_else(|| {
                command_space.and_then(|space| {
                    reactor.windows_in_active_workspace(space).into_iter().next()
                })
            })
            .or_else(|| {
                reactor.iter_active_spaces().find_map(|space| {
                    reactor.windows_in_active_workspace(space).into_iter().next()
                })
            })
    }

    fn current_instance_pid_hint(reactor: &Reactor) -> Option<pid_t> {
        let layout_focus = reactor.focused_window_for_command();
        Self::resolve_current_window_for_command(
            reactor,
            reactor.workspace_command_space(),
            reactor.config.settings.focus_follows_mouse,
            layout_focus,
        )
        .map(|wid| wid.pid)
    }

    fn resolve_window_index(
        reactor: &Reactor,
        index: u32,
        command_space: Option<crate::sys::screen::SpaceId>,
    ) -> Option<WindowId> {
        let pid_hint = Self::current_instance_pid_hint(reactor);
        let snapshot = reactor.core_snapshot();
        let active_workspace = command_space
            .and_then(|space| reactor.active_workspace_for_space(space));
        let mut candidates = snapshot
            .windows
            .iter()
            .filter(|window| window.id.index.get() == index)
            .collect::<Vec<_>>();
        candidates.sort_by_key(|window| {
            (
                window.workspace != active_workspace,
                pid_hint.is_none_or(|pid| window.id.application.0 != pid),
                window.id,
            )
        });
        candidates.first().map(|window| Self::actor_window(window.id))
    }

    pub fn handle_command(reactor: &mut Reactor, cmd: Command) {
        match cmd {
            Command::Layout(cmd) => Self::handle_command_layout(reactor, cmd),
            Command::Metrics(cmd) => Self::handle_command_metrics(reactor, cmd),
            Command::Reactor(cmd) => Self::handle_command_reactor(reactor, cmd),
        }
    }

    pub fn handle_command_layout(reactor: &mut Reactor, cmd: LayoutCommand) {
        info!(?cmd);
        if let LayoutCommand::SwitchToGlobalSlot(slot) = cmd {
            Self::handle_command_switch_to_global_slot(reactor, slot);
            return;
        }
        let is_workspace_switch = matches!(
            cmd,
            LayoutCommand::NextWorkspace(_)
                | LayoutCommand::PrevWorkspace(_)
                | LayoutCommand::SwitchToWorkspace(_)
                | LayoutCommand::SwitchToLastWorkspace
        );
        let requires_workspace_space = matches!(
            cmd,
            LayoutCommand::NextWorkspace(_)
                | LayoutCommand::PrevWorkspace(_)
                | LayoutCommand::SwitchToWorkspace(_)
                | LayoutCommand::CreateWorkspace
                | LayoutCommand::SwitchToLastWorkspace
        );
        let command_space = reactor.workspace_command_space();

        let workspace_space = if requires_workspace_space {
            if let Some(space) = command_space {
                reactor.store_current_floating_positions(space);
            }
            command_space
        } else {
            None
        };
        let core_workspace_response = if matches!(
            cmd,
            LayoutCommand::NextWorkspace(_)
                | LayoutCommand::PrevWorkspace(_)
                | LayoutCommand::SwitchToWorkspace(_)
                | LayoutCommand::CreateWorkspace
                | LayoutCommand::SwitchToLastWorkspace
        ) {
            match Self::transition_core_workspace_command(reactor, workspace_space, &cmd) {
                Ok(response) => Some(response),
                Err(error) => {
                    warn!(?error, ?cmd, "Core rejected workspace command");
                    reactor.handle_layout_response(EventResponse::default(), workspace_space);
                    return;
                }
            }
        } else {
            None
        };
        if is_workspace_switch {
            reactor
                .workspace_switch_manager
                .start_workspace_switch(WorkspaceSwitchOrigin::Manual);
        } else {
            reactor.workspace_switch_manager.mark_workspace_switch_inactive();
        }

        if let LayoutCommand::MoveWindowToWorkspace { workspace, window_id: None } = &cmd {
            let layout_focus = reactor.focused_window_for_command();
            let current_window = Self::resolve_current_window_for_command(
                reactor,
                command_space,
                reactor.config.settings.focus_follows_mouse,
                layout_focus,
            );
            let target_number = u8::try_from(workspace.saturating_add(1)).ok();
            let snapshot = reactor.core_snapshot();
            let target_workspace = target_number.and_then(|number| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|candidate| candidate.number.get() == number)
                    .map(|candidate| {
                        serde_json::json!({
                            "id": candidate.id,
                            "number": candidate.number.get(),
                            "display": candidate.display,
                        })
                    })
            });
            reactor.recording_manager.diagnostics.record_decision(
                "move_window_resolution",
                serde_json::json!({
                    "requested_slot_zero_based": workspace,
                    "requested_workspace_number": target_number,
                    "command_space": command_space,
                    "focus_follows_mouse": reactor.config.settings.focus_follows_mouse,
                    "main_window": reactor.main_window(),
                    "window_under_cursor": reactor.window_id_under_cursor(),
                    "layout_focus": layout_focus,
                    "resolved_window": current_window,
                    "layout_focus_workspace": layout_focus.and_then(|window| reactor.workspace_for_window(window)),
                    "resolved_window_workspace": current_window.and_then(|window| reactor.workspace_for_window(window)),
                    "target_workspace": target_workspace,
                }),
            );
            let current_window_without_workspace =
                current_window.filter(|wid| reactor.workspace_for_window(*wid).is_none());
            if let Some(window_id) = current_window_without_workspace {
                reactor.recording_manager.diagnostics.record_decision(
                    "move_window_result",
                    serde_json::json!({
                        "status": "deferred_until_window_assignment",
                        "window": window_id,
                    }),
                );
                reactor.remember_recent_workspace_target_for_slot(window_id, *workspace);
                reactor.handle_layout_response(EventResponse::default(), workspace_space);
                return;
            }

            if let Some(window_id) = current_window {
                if let Err(error) =
                    Self::transition_core_window_move(reactor, *workspace, window_id)
                {
                    reactor.recording_manager.diagnostics.record_decision(
                        "move_window_result",
                        serde_json::json!({"status": "rejected", "error": error.to_string()}),
                    );
                    warn!(?error, ?cmd, "Core rejected window move");
                    reactor.handle_layout_response(EventResponse::default(), workspace_space);
                    return;
                }
                if layout_focus != Some(window_id)
                    && Self::assigned_space_for_window(reactor, window_id)
                        .or_else(|| reactor.intended_space_for_window_id(window_id))
                        .is_some()
                {
                    reactor.send_layout_event(LayoutEvent::WindowFocused(window_id));
                }

                reactor.remember_recent_workspace_target(window_id);
                reactor.recording_manager.diagnostics.record_decision(
                    "move_window_result",
                    serde_json::json!({
                        "status": "accepted",
                        "window": window_id,
                        "actual_workspace": reactor.workspace_for_window(window_id),
                    }),
                );
                let response = EventResponse::default();
                reactor.handle_layout_response(response, workspace_space);
                return;
            }

            if let Some(window_id) = layout_focus {
                reactor.recording_manager.diagnostics.record_decision(
                    "move_window_result",
                    serde_json::json!({
                        "status": "deferred_without_resolved_current_window",
                        "layout_focus": window_id,
                    }),
                );
                reactor.remember_recent_workspace_target_for_slot(window_id, *workspace);
            } else {
                reactor.recording_manager.diagnostics.record_decision(
                    "move_window_result",
                    serde_json::json!({"status": "ignored_without_current_window"}),
                );
            }
        }

        if matches!(cmd, LayoutCommand::ToggleWindowFloating) {
            let Some(window) = reactor.focused_window_for_command()
            else {
                reactor.handle_layout_response(EventResponse::default(), workspace_space);
                return;
            };
            let window = Self::core_window(window);
            if let Err(error) = reactor.transition_core_command(CoreCommand::Window(
                CoreWindowCommand::ToggleFloating {
                    window: Some(window),
                },
            )) {
                warn!(?error, ?cmd, "Core rejected floating-state command");
                reactor.handle_layout_response(EventResponse::default(), workspace_space);
                return;
            }
        }

        let response = match &cmd {
            LayoutCommand::NextWorkspace(_)
            | LayoutCommand::PrevWorkspace(_)
            | LayoutCommand::SwitchToWorkspace(_)
            | LayoutCommand::CreateWorkspace
            | LayoutCommand::SwitchToLastWorkspace => {
                core_workspace_response.unwrap_or_default()
            }
            LayoutCommand::MoveWindowToWorkspace { .. } => {
                let LayoutCommand::MoveWindowToWorkspace {
                    workspace,
                    window_id: Some(window_idx),
                } = &cmd
                else {
                    return;
                };
                let Some(window) = Self::resolve_window_index(reactor, *window_idx, command_space)
                else {
                    return;
                };
                if let Err(error) = Self::transition_core_window_move(reactor, *workspace, window) {
                    warn!(?error, ?cmd, "Core rejected window move");
                    return;
                }
                reactor.remember_recent_workspace_target(window);
                EventResponse::default()
            }
            _ => {
                match Self::transition_core_layout_command(reactor, &cmd) {
                    Ok(response) => response,
                    Err(error) => {
                        warn!(?error, ?cmd, "Core rejected BSP command");
                        return;
                    }
                }
                .unwrap_or_default()
            }
        };

        reactor.handle_layout_response(response, workspace_space);
    }

    fn handle_command_switch_to_global_slot(reactor: &mut Reactor, slot: usize) {
        let number = match Self::core_workspace_number(slot) {
            Ok(number) => number,
            Err(error) => {
                warn!(?error, slot, "Core rejected global workspace slot");
                return;
            }
        };
        let source_display = reactor
            .workspace_command_space()
            .and_then(|space| reactor.display_uuid_for_space(space));
        let before = reactor.core_snapshot();
        let existing = before
            .workspaces
            .iter()
            .find(|workspace| workspace.number == number)
            .cloned();
        let target_display = existing
            .as_ref()
            .map(|workspace| workspace.display.0.clone())
            .or(source_display.clone())
            .or_else(|| {
                reactor
                    .space_for_cursor_screen()
                    .and_then(|space| reactor.display_uuid_for_space(space))
            })
            .or_else(|| {
                reactor.space_manager.screens.first().map(|screen| screen.display_uuid.clone())
            });
        let Some(target_display) = target_display else {
            warn!(slot, "SwitchToGlobalSlot: no display available");
            return;
        };
        let target_space = reactor
            .space_manager
            .screens
            .iter()
            .find(|screen| screen.display_uuid == target_display)
            .and_then(|screen| screen.space);
        let Some(target_space) = target_space.filter(|space| reactor.is_space_active(*space)) else {
            warn!(slot, %target_display, "SwitchToGlobalSlot: target display is inactive");
            return;
        };

        let already_active = reactor
            .active_workspace_for_space(target_space)
            .and_then(|workspace| reactor.workspace_number(workspace))
            == Some(number);
        let back_and_forth_enabled = reactor.config.virtual_workspaces.workspace_auto_back_and_forth;
        if already_active
            && source_display.as_deref() == Some(target_display.as_str())
            && back_and_forth_enabled
        {
            reactor.store_current_floating_positions(target_space);
            reactor
                .workspace_switch_manager
                .start_workspace_switch(WorkspaceSwitchOrigin::Manual);
            let transition = reactor.transition_core_command(CoreCommand::Workspace(
                CoreWorkspaceCommand::Last {
                    display: CoreDisplayId(target_display.clone()),
                },
            ));
            let response = match transition {
                Ok(transition) => Self::response_from_core_transition(reactor, &transition),
                Err(error) => {
                    warn!(?error, slot, "Core rejected workspace back-and-forth");
                    return;
                }
            };
            reactor.handle_layout_response(response, Some(target_space));
            return;
        }

        if already_active {
            if let Some(screen) = reactor.space_manager.screen_by_space(target_space).cloned()
                && !Self::focus_first_window_on_screen(reactor, &screen)
            {
                reactor.warp_mouse_to_space_center(target_space);
            }
            return;
        }

        reactor.store_current_floating_positions(target_space);
        reactor
            .workspace_switch_manager
            .start_workspace_switch(WorkspaceSwitchOrigin::Manual);
        let command = if existing.is_some() {
            CoreWorkspaceCommand::Activate(number)
        } else {
            CoreWorkspaceCommand::ActivateOrCreate {
                workspace: number,
                display: CoreDisplayId(target_display),
            }
        };
        let response = match reactor.transition_core_command(CoreCommand::Workspace(command)) {
            Ok(transition) => Self::response_from_core_transition(reactor, &transition),
            Err(error) => {
                warn!(?error, slot, "Core rejected global workspace switch");
                return;
            }
        };
        reactor.handle_layout_response(response, Some(target_space));
    }

    pub fn handle_command_metrics(_reactor: &mut Reactor, cmd: MetricsCommand) {
        handle_command(cmd);
    }

    pub fn handle_config_updated(reactor: &mut Reactor, new_cfg: Config) {
        let core_config = match crate::interfaces::config::core_config(&new_cfg) {
            Ok(config) => config,
            Err(error) => {
                warn!(?error, "Ignoring invalid Lift configuration update");
                return;
            }
        };
        let window_rules = match crate::core::rules::RuleSet::compile(core_config.window_rules) {
            Ok(rules) => rules,
            Err(error) => {
                warn!(?error, "Ignoring invalid Lift window rules");
                return;
            }
        };
        let old_keys = reactor.config.keys.clone();

        reactor
            .recording_manager
            .diagnostics
            .reconfigure(new_cfg.settings.diagnostics.clone());
        reactor.config = new_cfg;
        reactor.window_rules = window_rules;
        if let Some(tx) = &reactor.communication_manager.stack_line_tx {
            if let Err(e) = tx.try_send(StackLineEvent::ConfigUpdated(reactor.config.clone())) {
                warn!("Failed to send config update to stack line: {}", e);
            }
        }

        if let Some(tx) = &reactor.menu_manager.menu_tx {
            if let Err(e) = tx.try_send(menu_bar::Event::ConfigUpdated(reactor.config.clone())) {
                warn!("Failed to send config update to menu bar: {}", e);
            }
        }

        let _ = reactor.update_layout_or_warn(false, true);

        if old_keys != reactor.config.keys {
            if let Some(wm) = &reactor.communication_manager.wm_sender {
                wm.send(WmEvent::ConfigUpdated(reactor.config.clone()));
            }
        }
    }

    pub fn handle_command_reactor_debug(reactor: &mut Reactor) {
        if let Ok(state) = reactor.serialize_state() {
            println!("{state}");
        }
    }

    pub fn handle_command_reactor(reactor: &mut Reactor, cmd: ReactorCommand) {
        match cmd {
            ReactorCommand::Debug => Self::handle_command_reactor_debug(reactor),
            ReactorCommand::Serialize => Self::handle_command_reactor_serialize(reactor),
            ReactorCommand::SaveAndExit => Self::handle_command_reactor_save_and_exit(reactor),
            ReactorCommand::SwitchSpace(dir) => unsafe { window_server::switch_space(dir) },
            ReactorCommand::ToggleSpaceActivated => {
                Self::handle_command_reactor_toggle_space_activated(reactor);
            }
            ReactorCommand::FocusWindow { window_id, window_server_id } => {
                Self::handle_command_reactor_focus_window(reactor, window_id, window_server_id)
            }
            ReactorCommand::FocusNextWindow => reactor.request_focus_next_window(),
            ReactorCommand::CancelFocusNextWindow => reactor.cancel_focus_next_window(),
            ReactorCommand::ShowMissionControlAll => {
                if let Err(error) = reactor.transition_core_command(CoreCommand::MissionControl(
                    CoreMissionControlCommand::ShowAll,
                )) {
                    warn!(?error, "Core rejected Mission Control request");
                    return;
                }
                send_wm_cmd(
                    reactor,
                    crate::actor::wm_controller::WmCmd::ShowMissionControlAll,
                );
            }
            ReactorCommand::ShowMissionControlCurrent => {
                if let Err(error) = reactor.transition_core_command(CoreCommand::MissionControl(
                    CoreMissionControlCommand::ShowCurrent,
                )) {
                    warn!(?error, "Core rejected Mission Control request");
                    return;
                }
                send_wm_cmd(
                    reactor,
                    crate::actor::wm_controller::WmCmd::ShowMissionControlCurrent,
                );
            }
            ReactorCommand::DismissMissionControl => {
                if let Err(error) = reactor.transition_core_command(CoreCommand::MissionControl(
                    CoreMissionControlCommand::Dismiss,
                )) {
                    warn!(?error, "Core rejected Mission Control dismiss request");
                    return;
                }
                if !send_wm_cmd(
                    reactor,
                    crate::actor::wm_controller::WmCmd::DismissMissionControl,
                ) {
                    reactor.set_mission_control_active(false);
                }
            }
            ReactorCommand::MoveMouseToDisplay(selector) => {
                Self::handle_command_reactor_move_mouse_to_display(reactor, &selector);
            }
            ReactorCommand::FocusDisplay(selector) => {
                Self::handle_command_reactor_focus_display(reactor, &selector);
            }
            ReactorCommand::CloseWindow { window_server_id } => {
                Self::handle_command_reactor_close_window(reactor, window_server_id);
            }
            ReactorCommand::MoveWindowToDisplay { selector, window_id } => {
                Self::handle_command_reactor_move_window_to_display(reactor, &selector, window_id);
            }
        }
    }

    pub fn handle_command_reactor_serialize(reactor: &mut Reactor) {
        if let Ok(state) = reactor.serialize_state() {
            println!("{}", state);
        }
    }

    pub fn handle_command_reactor_save_and_exit(reactor: &mut Reactor) {
        let transition = match reactor.transition_core_command(CoreCommand::SaveAndExit) {
            Ok(transition) => transition,
            Err(error) => {
                error!(?error, "Core rejected save-and-exit");
                return;
            }
        };
        for effect in transition.effects {
            match effect {
                CoreEffect::Save(state) => {
                    if let Err(error) = crate::runtime::persistence::save(
                        &crate::common::config::restore_file(),
                        &state,
                    ) {
                        error!(?error, "Could not save Lift state");
                        std::process::exit(3);
                    }
                }
                CoreEffect::Shutdown(_) => std::process::exit(0),
                _ => {}
            }
        }
    }

    pub fn handle_command_reactor_toggle_space_activated(reactor: &mut Reactor) {
        let cfg = reactor.activation_cfg();

        let focused_space = reactor
            .space_for_cursor_screen()
            .or_else(|| reactor.space_manager.first_known_space());

        let Some(space) = focused_space else {
            return;
        };

        let display_uuid = reactor
            .space_manager
            .screen_by_space(space)
            .and_then(|screen| screen.display_uuid_owned());

        reactor.space_activation_policy.toggle_space_activated(
            cfg,
            crate::model::space_activation::ToggleSpaceContext { space, display_uuid },
        );

        reactor.recompute_and_set_active_spaces_from_current_screens();
    }

    pub fn handle_command_reactor_focus_window(
        reactor: &mut Reactor,
        window_id: WindowId,
        window_server_id: Option<WindowServerId>,
    ) {
        if let Some(window) = reactor.window_manager.window(window_id) {
            let Some(space) =
                reactor.best_space_for_window(&window.frame_monotonic, window.info.sys_id)
            else {
                warn!(?window_id, "Focus window ignored: space unknown");
                return;
            };
            if !reactor.is_space_active(space) {
                warn!(?window_id, ?space, "Focus window ignored: space is inactive");
                return;
            }
            if let Err(error) = reactor.transition_core_command(CoreCommand::Window(
                CoreWindowCommand::Activate {
                    window: Self::core_window(window_id),
                },
            )) {
                warn!(?error, ?window_id, "Core rejected focus request");
                return;
            }
            reactor.send_layout_event(LayoutEvent::WindowFocused(window_id));

            let mut app_handles: HashMap<i32, AppThreadHandle> = HashMap::default();
            if let Some(app) = reactor.app_manager.apps.get(&window_id.pid) {
                app_handles.insert(window_id.pid, app.handle.clone());
            }
            let request = raise_manager::Event::RaiseRequest(raise_manager::RaiseRequest {
                raise_windows: Vec::new(),
                focus_window: Some((window_id, None)),
                app_handles,
                focus_quiet: Quiet::No,
                workspace_switch_generation: None,
            });
            if let Err(e) = reactor.communication_manager.raise_manager_tx.try_send(request) {
                warn!("Failed to send raise request: {}", e);
            }
        } else if let Some(wsid) = window_server_id {
            if let Err(e) = window_server::make_key_window(window_id.pid, wsid) {
                warn!("Failed to make key window: {:?}", e);
            }
        }
    }

    fn focus_first_window_on_screen(reactor: &mut Reactor, screen: &ScreenInfo) -> bool {
        if let Some(space) = screen.space {
            let focus_target = reactor.last_focused_window_in_space(space).or_else(|| {
                reactor.windows_in_active_workspace(space).into_iter().next()
            });
            if let Some(window_id) = focus_target {
                reactor.send_layout_event(LayoutEvent::WindowFocused(window_id));

                // Update layout state, then issue an OS-level raise — without
                // it, system focus stays on the previously-focused display
                // (so mouse_follows_focus and keyboard input target the wrong
                // screen). Mirrors handle_command_reactor_focus_window.
                let mut app_handles: HashMap<pid_t, AppThreadHandle> = HashMap::default();
                reactor.insert_app_handle_for_window(&mut app_handles, window_id);
                let warp = reactor
                    .config
                    .settings
                    .mouse_follows_focus
                    .then(|| reactor.window_center_on_known_screen(window_id))
                    .flatten();
                let request = raise_manager::Event::RaiseRequest(raise_manager::RaiseRequest {
                    raise_windows: Vec::new(),
                    focus_window: Some((window_id, warp)),
                    app_handles,
                    focus_quiet: Quiet::No,
                    workspace_switch_generation: None,
                });
                if let Err(e) = reactor.communication_manager.raise_manager_tx.try_send(request) {
                    warn!(
                        "Failed to send raise request from focus_first_window_on_screen: {}",
                        e
                    );
                }
                return true;
            }
        }
        false
    }

    pub fn handle_command_reactor_move_mouse_to_display(
        reactor: &mut Reactor,
        selector: &DisplaySelector,
    ) {
        let target_screen = reactor.screen_for_selector(selector, None).cloned();

        if let Some(screen) = target_screen {
            if screen.space.is_some_and(|space| !reactor.is_space_active(space)) {
                warn!(
                    ?selector,
                    ?screen.space,
                    "Move mouse ignored: target display space is inactive"
                );
                return;
            }
            let center = screen.frame.mid();
            reactor.warp_mouse(center);
            let _ = Self::focus_first_window_on_screen(reactor, &screen);
        }
    }

    pub fn handle_command_reactor_focus_display(reactor: &mut Reactor, selector: &DisplaySelector) {
        let screen = match reactor.screen_for_selector(selector, None).cloned() {
            Some(s) => s,
            None => return,
        };
        if screen.space.is_some_and(|space| !reactor.is_space_active(space)) {
            warn!(
                ?selector,
                ?screen.space,
                "Focus display ignored: target display space is inactive"
            );
            return;
        }

        if Self::focus_first_window_on_screen(reactor, &screen) {
            return;
        }

        reactor.warp_mouse(screen.frame.mid());
    }

    pub fn handle_command_reactor_move_window_to_display(
        reactor: &mut Reactor,
        selector: &DisplaySelector,
        window_idx: Option<u32>,
    ) {
        if reactor.is_in_drag() {
            warn!("Ignoring move-window-to-display while a drag is active");
            return;
        }

        let resolved_window = match window_idx {
            Some(index) => {
                Self::resolve_window_index(reactor, index, reactor.workspace_command_space())
            }
            None => Self::resolve_current_window_for_command(
                reactor,
                reactor.workspace_command_space(),
                reactor.config.settings.focus_follows_mouse,
                reactor.focused_window_for_command(),
            ),
        };

        let Some(window_id) = resolved_window else {
            warn!("Move window to display ignored because no target window was resolved");
            return;
        };

        let (window_server_id, window_frame) = match reactor.window_manager.window(window_id) {
            Some(state) => (state.info.sys_id, state.frame_monotonic),
            None => {
                warn!(?window_id, "Move window to display ignored: unknown window");
                return;
            }
        };

        let Some(source_space) = Self::assigned_space_for_window(reactor, window_id)
            .or_else(|| reactor.best_space_for_window(&window_frame, window_server_id))
        else {
            warn!(
                ?window_id,
                "Move window to display ignored: source space unknown"
            );
            return;
        };
        if !reactor.is_space_active(source_space) {
            warn!(
                ?window_id,
                ?source_space,
                "Move window to display ignored: source space is inactive"
            );
            return;
        }

        let origin_screen = reactor.space_manager.screen_by_space(source_space);

        let origin_point =
            origin_screen.map(|s| s.frame.mid()).or_else(|| reactor.current_screen_center());
        let target_screen = reactor.screen_for_selector(selector, origin_point).cloned();

        let Some(target_screen) = target_screen else {
            warn!(
                ?selector,
                "Move window to display ignored: target display not found"
            );
            return;
        };
        let Some(target_space) = target_screen.space else {
            warn!(
                uuid = ?target_screen.display_uuid,
                "Move window to display ignored: display has no active space"
            );
            return;
        };
        if !reactor.is_space_active(target_space) {
            warn!(
                ?selector,
                ?target_space,
                "Move window to display ignored: target display space is inactive"
            );
            return;
        }

        if target_space == source_space {
            return;
        }

        let mut target_frame = window_frame;
        let size = window_frame.size;
        let dest_rect = target_screen.frame;
        let mut origin = dest_rect.mid();
        origin.x -= size.width / 2.0;
        origin.y -= size.height / 2.0;
        let min = dest_rect.min();
        let max = dest_rect.max();
        origin.x = origin.x.max(min.x).min(max.x - size.width);
        origin.y = origin.y.max(min.y).min(max.y - size.height);
        target_frame.origin = origin;

        if let Some(app) = reactor.app_manager.apps.get(&window_id.pid) {
            if let Some(wsid) = window_server_id {
                let txid = reactor.transaction_manager.generate_next_txid(wsid);
                reactor.transaction_manager.set_last_sent_txid(wsid, txid);
                let _ = app.handle.send(crate::actor::app::Request::SetWindowFrame(
                    window_id,
                    target_frame,
                    txid,
                    true,
                ));
            } else {
                let txid = TransactionId::default();
                let _ = app.handle.send(crate::actor::app::Request::SetWindowFrame(
                    window_id,
                    target_frame,
                    txid,
                    true,
                ));
            }
        }

        if let Some(state) = reactor.window_manager.window_mut(window_id) {
            state.frame_monotonic = target_frame;
        }

        let core_command = CoreDisplayCommand::MoveWindowTo {
            display: CoreDisplayId(target_screen.display_uuid.clone()),
            window: Some(Self::core_window(window_id)),
        };
        let response = match reactor.transition_core_command(CoreCommand::Display(core_command)) {
            Ok(transition) => Self::response_from_core_transition(reactor, &transition),
            Err(error) => {
                warn!(?error, ?window_id, "Core rejected cross-display window move");
                return;
            }
        };

        reactor.remember_recent_workspace_target(window_id);
        reactor.handle_layout_response(response, None);

        let _ = reactor.update_layout_or_warn(false, false);
    }

    pub fn handle_command_reactor_close_window(
        reactor: &mut Reactor,
        window_server_id: Option<WindowServerId>,
    ) {
        let command_space = reactor.workspace_command_space();
        let target = window_server_id
            .and_then(|wsid| reactor.window_manager.tracked_window_id(wsid))
            .or_else(|| Self::resolve_current_window_for_command(
                reactor,
                command_space,
                false,
                None,
            ));
        if let Some(wid) = target {
            let transition = match reactor.transition_core_command(CoreCommand::Window(
                CoreWindowCommand::Close(Some(Self::core_window(wid))),
            )) {
                Ok(transition) => transition,
                Err(error) => {
                    warn!(?error, ?wid, "Core rejected close request");
                    return;
                }
            };
            if !transition.effects.iter().any(
                |effect| matches!(effect, CoreEffect::CloseWindow(window) if *window == Self::core_window(wid)),
            ) {
                warn!(?wid, "Core close transaction did not emit a platform effect");
                return;
            }
            reactor.request_close_window(wid);
        } else {
            warn!("Close window command ignored because no window is tracked");
        }
    }
}

fn send_wm_cmd(reactor: &mut Reactor, cmd: crate::actor::wm_controller::WmCmd) -> bool {
    if let Some(wm) = reactor.communication_manager.wm_sender.as_ref() {
        let _ = wm.send(crate::actor::wm_controller::WmEvent::Command(
            crate::actor::wm_controller::WmCommand::Wm(cmd),
        ));
        true
    } else {
        false
    }
}
