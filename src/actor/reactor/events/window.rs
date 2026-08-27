use objc2_core_foundation::CGRect;
use serde_json::json;
use tracing::{debug, trace, warn};

use crate::actor::app::WindowId;
use crate::actor::reactor::events::drag::DragEventHandler;
use crate::actor::reactor::{
    DragState, LayoutEvent, Quiet, Reactor, Requested, TransactionId, WindowFilter, WindowState,
    utils,
};
use crate::sys::app::WindowInfo as Window;
use crate::sys::event::{MouseState, get_mouse_state};
use crate::sys::geometry::SameAs;
use crate::sys::screen::SpaceId;
use crate::sys::window_server::{WindowServerId, WindowServerInfo};

pub struct WindowEventHandler;

impl WindowEventHandler {
    pub fn handle_window_created(
        reactor: &mut Reactor,
        wid: WindowId,
        window: Window,
        ws_info: Option<WindowServerInfo>,
        _mouse_state: Option<MouseState>,
    ) {
        if let Some(wsid) = window.sys_id {
            reactor.window_manager.track_window_server_id(wsid, wid);
            reactor.window_manager.clear_window_server_observed(wsid);
        }
        if let Some(info) = ws_info {
            reactor.window_manager.clear_window_server_observed(info.id);
            reactor.window_manager.track_window_server_info(info);
        }

        let frame = window.frame;
        let mut window_state: WindowState = window.into();
        let is_manageable = utils::compute_window_manageability(
            window_state.info.sys_id,
            window_state.info.is_minimized,
            window_state.info.is_standard,
            window_state.info.is_root,
            |wsid| reactor.window_manager.get_window_server_info(wsid),
        );
        window_state.is_manageable = is_manageable;
        if let Some(wsid) = window_state.info.sys_id {
            reactor.transaction_manager.store_txid(
                wsid,
                reactor.transaction_manager.get_last_sent_txid(wsid),
                window_state.frame_monotonic,
            );
        }

        let server_id = window_state.info.sys_id;
        reactor.window_manager.insert_window(wid, window_state);

        if is_manageable {
            reactor.prepare_new_window_for_auxiliary_click_workspace(wid);
            let active_space = active_space_for_window(reactor, &frame, server_id);
            if let Some(space) = active_space {
                if let Some(app_info) =
                    reactor.app_manager.apps.get(&wid.pid).map(|app| app.info.clone())
                {
                    if let Some(wsid) = server_id {
                        reactor.window_manager.mark_wsids_recent(std::iter::once(wsid));
                    }
                    reactor.process_windows_for_app_rules(wid.pid, vec![wid], app_info);
                }
                maybe_dispatch_window_added_in_space(reactor, wid, space);
                reactor.consume_focus_next_window_for(wid);
            }
        }
    }

    pub fn handle_window_destroyed(reactor: &mut Reactor, wid: WindowId) -> bool {
        let window_server_id = match reactor.window_manager.window(wid) {
            Some(window) => window.info.sys_id,
            None => return false,
        };

        // Suppress false-positive destructions when on a fullscreen space or during MC.
        // kAXMainWindowChangedNotification triggers remove_stale_windows in app.rs, which
        // calls kAXWindowsAttribute (space-filtered), omitting Desktop windows and emitting
        // WindowDestroyed for them. get_window() uses CGWindowListCopyWindowInfo
        // (not space-filtered), so Some here means the window still exists.
        if !crate::sys::window_server::active_space_is_user() || reactor.is_mission_control_active()
        {
            if let Some(ws_id) = window_server_id {
                if crate::sys::window_server::get_window(ws_id)
                    .is_some_and(|ws_info| ws_info.pid == wid.pid)
                {
                    return false;
                }
            }
        }

        if !Self::remove_window_state(reactor, wid, window_server_id) {
            return false;
        }
        reactor.send_layout_event(LayoutEvent::Changed);
        true
    }

    pub fn remove_windows_for_terminated_app(reactor: &mut Reactor, pid: i32) -> bool {
        let windows = reactor.window_manager.window_ids_for_pid(pid).collect::<Vec<_>>();
        let mut removed = false;
        for wid in windows {
            let window_server_id =
                reactor.window_manager.window(wid).and_then(|window| window.info.sys_id);
            removed |= Self::remove_window_state(reactor, wid, window_server_id);
        }
        removed
    }

    fn remove_window_state(
        reactor: &mut Reactor,
        wid: WindowId,
        window_server_id: Option<WindowServerId>,
    ) -> bool {
        if !reactor.window_manager.contains_window(wid) {
            return false;
        }

        if let Some(ws_id) = window_server_id {
            reactor.transaction_manager.remove_for_window(ws_id);
            reactor.window_manager.remove_window_server_state(ws_id);
        }
        reactor.window_manager.remove_window(wid);

        let drag = reactor.core_drag_snapshot();
        let to_actor = |window: crate::core::ids::WindowId| {
            WindowId::new(window.application.0, window.index.get())
        };
        if drag.window.map(to_actor) == Some(wid) || drag.target.map(to_actor) == Some(wid) {
            trace!(
                ?wid,
                "Clearing drag swap because a participant window was destroyed"
            );
            let _ = reactor.transition_core_input(crate::core::input::Input::Observation(
                crate::core::input::Observation::Drag(
                    crate::core::interaction::DragObservation::Cancelled,
                ),
            ));
            reactor.drag_manager.drag_state = DragState::Inactive;
        }

        if reactor.drag_manager.skip_layout_for_window == Some(wid) {
            reactor.drag_manager.skip_layout_for_window = None;
        }
        true
    }

    pub fn handle_window_minimized(reactor: &mut Reactor, wid: WindowId) {
        if let Some(window) = reactor.window_manager.window_mut(wid) {
            if window.info.is_minimized {
                return;
            }
            window.info.is_minimized = true;
            window.is_manageable = false;
            if let Some(ws_id) = window.info.sys_id {
                reactor.window_manager.mark_window_hidden(ws_id);
            }
            reactor.send_layout_event(LayoutEvent::Changed);
        } else {
            debug!(?wid, "Received WindowMinimized for unknown window - ignoring");
        }
    }

    pub fn handle_window_deminiaturized(reactor: &mut Reactor, wid: WindowId) {
        let (frame, server_id, is_ax_standard, is_ax_root) =
            match reactor.window_manager.window_mut(wid) {
                Some(window) => {
                    if !window.info.is_minimized {
                        return;
                    }
                    window.info.is_minimized = false;
                    (
                        window.frame_monotonic,
                        window.info.sys_id,
                        window.info.is_standard,
                        window.info.is_root,
                    )
                }
                None => {
                    debug!(
                        ?wid,
                        "Received WindowDeminiaturized for unknown window - ignoring"
                    );
                    return;
                }
            };
        let is_manageable = utils::compute_window_manageability(
            server_id,
            false,
            is_ax_standard,
            is_ax_root,
            |wsid| reactor.window_manager.get_window_server_info(wsid),
        );
        if let Some(window) = reactor.window_manager.window_mut(wid) {
            window.is_manageable = is_manageable;
        }

        if is_manageable {
            let active_space = active_space_for_window(reactor, &frame, server_id);
            if let Some(space) = active_space {
                maybe_dispatch_window_added_in_space(reactor, wid, space);
            }
        }
    }

    pub fn handle_window_frame_changed(
        reactor: &mut Reactor,
        wid: WindowId,
        new_frame: CGRect,
        last_seen: Option<TransactionId>,
        requested: Requested,
        mouse_state: Option<MouseState>,
    ) -> bool {
        debug!(
            ?wid,
            ?new_frame,
            last_seen=?last_seen,
            requested=?requested,
            mouse_state=?mouse_state,
            window_known=reactor.window_manager.contains_window(wid),
            "WindowFrameChanged event"
        );

        let effective_mouse_state = mouse_state.or_else(|| get_mouse_state());
        let result = (|| -> bool {
            let (server_id, old_frame) = {
                let Some(window) = reactor.window_manager.window(wid) else {
                    return false;
                };

                if reactor.is_mission_control_active() {
                    return false;
                }

                (window.info.sys_id, window.frame_monotonic)
            };

            let pending_target = server_id.and_then(|wsid| {
                reactor.transaction_manager.get_target_frame(wsid).map(|target| (wsid, target))
            });

            let last_sent_txid = server_id
                .map(|wsid| reactor.transaction_manager.get_last_sent_txid(wsid))
                .unwrap_or_default();

            let mut has_pending_request = pending_target.is_some();
            let mut triggered_by_lift =
                has_pending_request && last_seen.is_some_and(|seen| seen == last_sent_txid);

            if effective_mouse_state == Some(MouseState::Down) && triggered_by_lift {
                if let Some((wsid, _)) = pending_target {
                    reactor.transaction_manager.clear_target_for_window(wsid);
                }
                triggered_by_lift = false;
                has_pending_request = false;
            }

            if !old_frame.size.same_as(new_frame.size) {
                let core_window = Reactor::core_window_id(wid);
                let snapshot = reactor.core_snapshot();
                let planned_frame = snapshot
                    .workspaces
                    .iter()
                    .find_map(|workspace| workspace.layout_frames.get(&core_window).copied());
                let workspace = reactor.workspace_for_window(wid);
                let focused_window = reactor.focused_window_for_command();
                let main_window = reactor.main_window();
                let floating = reactor.is_window_floating(wid);
                let in_drag = reactor.is_in_drag();
                let app = reactor.app_manager.apps.get(&wid.pid).map(|app| {
                    json!({
                        "bundle_id": app.info.bundle_id,
                        "name": app.info.localized_name,
                    })
                });
                let attribution = if triggered_by_lift {
                    "lift_transaction"
                } else if requested.0 {
                    "lift_requested_without_pending_transaction"
                } else if has_pending_request {
                    "stale_transaction"
                } else if effective_mouse_state == Some(MouseState::Down) || in_drag {
                    "user_drag"
                } else {
                    "external"
                };
                reactor.recording_manager.diagnostics.record_window_frame_change(json!({
                    "window": wid,
                    "window_server_id": server_id.map(|id| id.as_u32()),
                    "app": app,
                    "workspace": workspace,
                    "focused_window": focused_window,
                    "main_window": main_window,
                    "floating": floating,
                    "in_drag": in_drag,
                    "mouse_state": effective_mouse_state,
                    "requested": requested.0,
                    "attribution": attribution,
                    "last_seen_transaction": last_seen,
                    "last_sent_transaction": last_sent_txid,
                    "pending_target": pending_target.map(|(_, frame)| frame_json(frame)),
                    "planned_layout_frame": planned_frame,
                    "old_frame": frame_json(old_frame),
                    "new_frame": frame_json(new_frame),
                    "delta": {
                        "width": new_frame.size.width - old_frame.size.width,
                        "height": new_frame.size.height - old_frame.size.height,
                    },
                }));
            }

            if has_pending_request && last_seen.is_some_and(|seen| seen != last_sent_txid) {
                debug!(?last_seen, ?last_sent_txid, "Ignoring frame change");
                return false;
            }

            if triggered_by_lift {
                let Some(window) = reactor.window_manager.window_mut(wid) else {
                    return false;
                };

                if let Some((wsid, target)) = pending_target {
                    if new_frame.same_as(target) {
                        if !window.frame_monotonic.same_as(new_frame) {
                            debug!(?wid, ?new_frame, "Final frame matches Lift request");
                            window.frame_monotonic = new_frame;
                        }
                        reactor.transaction_manager.clear_target_for_window(wsid);
                    } else {
                        trace!(
                            ?wid,
                            ?new_frame,
                            ?target,
                            "Skipping intermediate frame from Lift request"
                        );
                    }
                } else if !window.frame_monotonic.same_as(new_frame) {
                    debug!(
                        ?wid,
                        ?new_frame,
                        "Lift frame event missing tx record; updating state"
                    );
                    window.frame_monotonic = new_frame;
                    if let Some(wsid) = window.info.sys_id {
                        reactor.transaction_manager.clear_target_for_window(wsid);
                    }
                }

                return false;
            }

            if requested.0 {
                if let Some(window) = reactor.window_manager.window_mut(wid) {
                    if !window.frame_monotonic.same_as(new_frame) {
                        debug!(
                            ?wid,
                            ?new_frame,
                            "Requested frame change without pending tx; syncing state"
                        );
                        window.frame_monotonic = new_frame;
                    }
                }
                if let Some(wsid) = server_id {
                    reactor.transaction_manager.clear_target_for_window(wsid);
                }
                return false;
            }

            let old_space = reactor.best_space_for_window(&old_frame, server_id);
            let new_space = reactor.best_space_for_window(&new_frame, server_id);
            let old_active = old_space.is_some_and(|space| reactor.is_space_active(space));
            let new_active = new_space.is_some_and(|space| reactor.is_space_active(space));

            if !old_active && !new_active {
                return false;
            }

            {
                let Some(window) = reactor.window_manager.window_mut(wid) else {
                    return false;
                };
                if window.frame_monotonic.same_as(new_frame) {
                    return false;
                }
                window.frame_monotonic = new_frame;
            }

            let dragging = effective_mouse_state == Some(MouseState::Down) || reactor.is_in_drag();

            if dragging {
                reactor.ensure_active_drag(wid, &old_frame);
                reactor.update_active_drag(wid, &new_frame);
                let is_resize = !old_frame.size.same_as(new_frame.size);
                if is_resize {
                    if active_space_for_window(reactor, &new_frame, server_id).is_some() {
                        let old_frame = crate::core::geometry::Rect::new(
                            old_frame.origin.x,
                            old_frame.origin.y,
                            old_frame.size.width,
                            old_frame.size.height,
                        );
                        let new_frame = crate::core::geometry::Rect::new(
                            new_frame.origin.x,
                            new_frame.origin.y,
                            new_frame.size.width,
                            new_frame.size.height,
                        );
                        if let (Ok(old_frame), Ok(new_frame)) = (old_frame, new_frame)
                            && let Err(error) = reactor.transition_core_input(
                                crate::core::input::Input::Observation(
                                    crate::core::input::Observation::Drag(
                                        crate::core::interaction::DragObservation::Resized {
                                            window: Reactor::core_window_id(wid),
                                            old_frame,
                                            new_frame,
                                        },
                                    ),
                                ),
                            )
                        {
                            warn!(?error, ?wid, "Core rejected dragged window resize");
                        }
                        reactor.send_layout_event(LayoutEvent::Changed);
                    }
                } else {
                    reactor.maybe_swap_on_drag(wid, new_frame);
                }
            } else {
                if old_space != new_space {
                    // Preserve a programmatic cross-display move long enough to
                    // transfer the window to the destination workspace. Same-space
                    // changes must not take this path: some apps resize themselves
                    // by a few pixels when focused, and skipping the tiled window
                    // would leave it overlapping its BSP neighbors.
                    reactor.drag_manager.skip_layout_for_window = Some(wid);
                    reactor.send_layout_event(LayoutEvent::Changed);
                    if let Some(space) = new_space {
                        if reactor.is_space_active(space) {
                            if let Some(active_ws) = reactor.active_workspace_for_space(space) {
                                let assigned =
                                    reactor.move_core_window_to_space(wid, space).is_ok();
                                if !assigned {
                                    warn!(
                                        "Failed to assign window {:?} to workspace {:?}",
                                        wid, active_ws
                                    );
                                } else {
                                    let _ = reactor.remember_recent_workspace_target_for(
                                        wid, space, active_ws,
                                    );
                                }
                            }
                            reactor.send_layout_event(LayoutEvent::WindowAdded(space, wid));
                        }
                    }
                    let _ = reactor.update_layout_or_warn(false, false);
                } else if !old_frame.size.same_as(new_frame.size) {
                    if let Some(space) = old_space {
                        if reactor.is_space_active(space) {
                            reactor.send_layout_event(LayoutEvent::Changed);
                            return true;
                        }
                    }
                    return false;
                }
            }
            false
        })();
        handle_mouse_up_if_needed(reactor, effective_mouse_state);
        result
    }

    pub fn handle_window_title_changed(reactor: &mut Reactor, wid: WindowId, new_title: String) {
        if let Some(window) = reactor.window_manager.window_mut(wid) {
            let previous_title = window.info.title.clone();
            if previous_title == new_title {
                return;
            }
            window.info.title = new_title.clone();
            reactor.broadcast_window_title_changed(wid, previous_title, new_title);
            reactor.maybe_reapply_app_rules_for_window(wid);
        }
    }

    pub fn handle_mouse_moved_over_window(reactor: &mut Reactor, wsid: WindowServerId) {
        let Some(wid) = reactor.window_manager.tracked_window_id(wsid) else {
            return;
        };
        let should_sync = reactor.should_raise_on_mouse_over(wid);
        let is_main = reactor.main_window() == Some(wid);
        let needs_sync = reactor.focused_window_for_command() != Some(wid);

        if !should_sync || (is_main && !needs_sync) {
            return;
        }

        if !is_main {
            reactor.raise_window(wid, Quiet::No, None);
        }

        if let Some(window) = reactor.window_manager.window(wid) {
            if active_space_for_window(reactor, &window.frame_monotonic, window.info.sys_id)
                .is_some()
            {
                reactor.send_layout_event(LayoutEvent::WindowFocused(wid));
            }
        }
    }
}

fn frame_json(frame: CGRect) -> serde_json::Value {
    json!({
        "origin": {"x": frame.origin.x, "y": frame.origin.y},
        "size": {"width": frame.size.width, "height": frame.size.height},
    })
}

fn active_space_for_window(
    reactor: &Reactor,
    frame: &CGRect,
    server_id: Option<WindowServerId>,
) -> Option<SpaceId> {
    let best = reactor.best_space_for_window(frame, server_id);
    if let Some(space) = best.filter(|space| reactor.is_space_active(*space)) {
        return Some(space);
    }

    // Some apps publish AX windows before the window server id/space is ready.
    // Fall back to the active command context so new windows land on the intended display.
    if server_id.is_none() {
        return reactor.workspace_command_space();
    }

    None
}

fn maybe_dispatch_window_added_in_space(reactor: &mut Reactor, wid: WindowId, space: SpaceId) {
    let should_dispatch = reactor
        .window_manager
        .window(wid)
        .map(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
        .unwrap_or(false);
    if should_dispatch {
        reactor.send_layout_event(LayoutEvent::WindowAdded(space, wid));
    }
}

fn handle_mouse_up_if_needed(reactor: &mut Reactor, mouse_state: Option<MouseState>) {
    if mouse_state == Some(MouseState::Up)
        && (matches!(reactor.drag_manager.drag_state, DragState::Active { .. })
            || reactor.drag_manager.skip_layout_for_window.is_some())
    {
        DragEventHandler::handle_mouse_up(reactor);
    }
}
