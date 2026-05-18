use tracing::{error, info, warn};

use super::super::ScreenInfo;
use crate::actor::app::{AppThreadHandle, Quiet, WindowId};
use crate::actor::reactor::transaction_manager::TransactionId;
use crate::actor::reactor::{
    Command, DisplaySelector, Reactor, ReactorCommand, WorkspaceSwitchOrigin,
};
use crate::actor::stack_line::Event as StackLineEvent;
use crate::actor::wm_controller::WmEvent;
use crate::actor::{menu_bar, raise_manager};
use crate::common::collections::HashMap;
use crate::common::config::{self as config, Config};
use crate::common::log::{MetricsCommand, handle_command};
use crate::layout_engine::{EventResponse, LayoutCommand, LayoutEvent};
use crate::sys::app::pid_t;
use crate::sys::window_server::{self as window_server, WindowServerId};

pub struct CommandEventHandler;

impl CommandEventHandler {
    fn assigned_space_for_window(
        reactor: &Reactor,
        window_id: WindowId,
    ) -> Option<crate::sys::screen::SpaceId> {
        let vwm = reactor.layout_manager.layout_engine.virtual_workspace_manager();
        vwm.workspace_for_window(window_id)
            .and_then(|ws_id| vwm.workspace_space(ws_id))
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
                | LayoutCommand::SetWorkspaceLayout { .. }
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
        if is_workspace_switch {
            reactor
                .workspace_switch_manager
                .start_workspace_switch(WorkspaceSwitchOrigin::Manual);
        } else {
            reactor.workspace_switch_manager.mark_workspace_switch_inactive();
        }

        let response = match &cmd {
            LayoutCommand::NextWorkspace(_)
            | LayoutCommand::PrevWorkspace(_)
            | LayoutCommand::SwitchToWorkspace(_)
            | LayoutCommand::SetWorkspaceLayout { .. }
            | LayoutCommand::CreateWorkspace
            | LayoutCommand::SwitchToLastWorkspace => {
                if let Some(space) = workspace_space {
                    reactor
                        .layout_manager
                        .layout_engine
                        .handle_virtual_workspace_command(space, &cmd)
                } else {
                    EventResponse::default()
                }
            }
            LayoutCommand::MoveWindowToWorkspace { .. } => {
                if let Some(space) = command_space {
                    reactor
                        .layout_manager
                        .layout_engine
                        .handle_virtual_workspace_command(space, &cmd)
                } else {
                    EventResponse::default()
                }
            }
            _ => {
                let (visible_spaces, visible_space_centers) =
                    reactor.visible_spaces_for_layout(false);
                if visible_spaces.is_empty() {
                    warn!("Layout command ignored: no active spaces");
                    return;
                }
                reactor.layout_manager.layout_engine.handle_command(
                    command_space,
                    &visible_spaces,
                    &visible_space_centers,
                    cmd,
                )
            }
        };

        reactor.handle_layout_response(response, workspace_space);
        if requires_workspace_space {
            reactor.update_event_tap_layout_mode();
        }
    }

    /// Switch to whichever workspace owns global slot `slot`. The target may
    /// live on a different display from the source; switch its owning space
    /// first, then let the resulting target workspace drive focus.
    ///
    /// If no workspace owns slot `slot` yet, one is created on the focused
    /// display (cursor display / first online as fallbacks) and the layout
    /// tree is wired up before the switch proceeds.
    ///
    /// Fast path: if the target workspace is already active on its space,
    /// just move focus to that display — skipping the SwitchToWorkspace flow
    /// avoids the hide/show cycle that briefly empties the visible workspace.
    fn handle_command_switch_to_global_slot(reactor: &mut Reactor, slot: usize) {
        let source_uuid =
            reactor.workspace_command_space().and_then(|space| reactor.display_uuid_for_space(space));

        let target = match reactor
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .resolve_workspace(slot)
        {
            Some(t) => t,
            None => {
                // Determine the display to create on: focused → cursor → first online.
                let create_uuid = source_uuid
                    .clone()
                    .or_else(|| {
                        reactor
                            .space_for_cursor_screen()
                            .and_then(|sp| reactor.display_uuid_for_space(sp))
                    })
                    .or_else(|| {
                        reactor.space_manager.screens.first().map(|s| s.display_uuid.clone())
                    });
                let Some(uuid) = create_uuid else {
                    warn!(slot, "SwitchToGlobalSlot: no display available to create on");
                    return;
                };
                let space = reactor
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager()
                    .space_for_display(&uuid);
                let Some(space) = space else {
                    warn!(slot, %uuid, "SwitchToGlobalSlot: no space mapped to display");
                    return;
                };
                // Layout wiring needs the screen size — pull it from SpaceManager.
                let size = match reactor.space_manager.screen_by_space(space) {
                    Some(s) => s.frame.size,
                    None => {
                        warn!(
                            slot,
                            ?space,
                            "SwitchToGlobalSlot: no screen for space; cannot wire layout"
                        );
                        return;
                    }
                };
                reactor
                    .layout_manager
                    .layout_engine
                    .create_workspace_on_display(slot, &uuid, space, size);
                // Re-resolve — must succeed now.
                reactor
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager()
                    .resolve_workspace(slot)
                    .expect("just created workspace must resolve")
            }
        };

        if !reactor.is_space_active(target.space) {
            warn!(slot, ?target.space, "SwitchToGlobalSlot: target space inactive");
            return;
        }

        let already_active = reactor.layout_manager.layout_engine.active_workspace(target.space)
            == Some(target.workspace_id);
        if already_active {
            if let Some(screen) = reactor.space_manager.screen_by_space(target.space).cloned() {
                if !Self::focus_first_window_on_screen(reactor, &screen) {
                    reactor.warp_mouse_to_space_center(target.space);
                }
            }
            return;
        }

        reactor.store_current_floating_positions(target.space);
        reactor.workspace_switch_manager.start_workspace_switch(WorkspaceSwitchOrigin::Manual);
        let response = reactor.layout_manager.layout_engine.handle_virtual_workspace_command(
            target.space,
            &LayoutCommand::SwitchToWorkspace(target.per_space_index),
        );
        reactor.handle_layout_response(response, Some(target.space));
        reactor.update_event_tap_layout_mode();
    }

    pub fn handle_command_metrics(_reactor: &mut Reactor, cmd: MetricsCommand) {
        handle_command(cmd);
    }

    pub fn handle_config_updated(reactor: &mut Reactor, new_cfg: Config) {
        let old_keys = reactor.config.keys.clone();

        reactor.config = new_cfg;
        reactor
            .layout_manager
            .layout_engine
            .set_layout_settings(&reactor.config.settings.layout);

        reactor
            .layout_manager
            .layout_engine
            .update_virtual_workspace_settings(&reactor.config.virtual_workspaces);

        reactor.drag_manager.update_config(reactor.config.settings.window_snapping);

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
        for screen in &reactor.space_manager.screens {
            if let Some(space) = screen.space {
                reactor.layout_manager.layout_engine.debug_tree_desc(space, "", true);
            }
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
                send_wm_cmd(
                    reactor,
                    crate::actor::wm_controller::WmCmd::ShowMissionControlAll,
                );
            }
            ReactorCommand::ShowMissionControlCurrent => {
                send_wm_cmd(
                    reactor,
                    crate::actor::wm_controller::WmCmd::ShowMissionControlCurrent,
                );
            }
            ReactorCommand::DismissMissionControl => {
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
        match reactor.layout_manager.layout_engine.save(config::restore_file()) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                error!("Could not save layout: {e}");
                std::process::exit(3);
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
            reactor.send_layout_event(LayoutEvent::WindowFocused(space, window_id));

            let mut app_handles: HashMap<i32, AppThreadHandle> = HashMap::default();
            if let Some(app) = reactor.app_manager.apps.get(&window_id.pid) {
                app_handles.insert(window_id.pid, app.handle.clone());
            }
            let request = raise_manager::Event::RaiseRequest(raise_manager::RaiseRequest {
                raise_windows: Vec::new(),
                focus_window: Some((window_id, None)),
                app_handles,
                focus_quiet: Quiet::No,
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
                reactor
                    .layout_manager
                    .layout_engine
                    .windows_in_active_workspace(space)
                    .into_iter()
                    .next()
            });
            if let Some(window_id) = focus_target {
                reactor.send_layout_event(LayoutEvent::WindowFocused(space, window_id));

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
                });
                if let Err(e) = reactor.communication_manager.raise_manager_tx.try_send(request) {
                    warn!("Failed to send raise request from focus_first_window_on_screen: {}", e);
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

        let resolved_window = {
            let vwm = reactor.layout_manager.layout_engine.virtual_workspace_manager();
            match window_idx {
                Some(idx) => {
                    if let Some(space) = reactor.workspace_command_space() {
                        vwm.find_window_by_idx(space, idx).or_else(|| {
                            reactor
                                .iter_active_spaces()
                                .find_map(|sp| vwm.find_window_by_idx(sp, idx))
                        })
                    } else {
                        reactor.iter_active_spaces().find_map(|sp| vwm.find_window_by_idx(sp, idx))
                    }
                }
                None => reactor.main_window().or_else(|| reactor.window_id_under_cursor()).or_else(
                    || {
                        reactor
                            .workspace_command_space()
                            .and_then(|space| vwm.find_window_by_idx(space, 0))
                    },
                ),
            }
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

        let response = reactor.layout_manager.layout_engine.move_window_to_space(
            source_space,
            target_space,
            target_screen.frame.size,
            window_id,
        );

        reactor.handle_layout_response(response, None);

        let _ = reactor.update_layout_or_warn(false, false);
    }

    pub fn handle_command_reactor_close_window(
        reactor: &mut Reactor,
        window_server_id: Option<WindowServerId>,
    ) {
        let target = window_server_id
            .and_then(|wsid| reactor.window_manager.tracked_window_id(wsid))
            .or_else(|| reactor.main_window());
        if let Some(wid) = target {
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
