use tracing::{debug, trace};

use super::window::WindowEventHandler;
use crate::actor::app::{AppInfo, WindowId, WindowInfo, pid_t};
use crate::actor::reactor::{Event, LayoutEvent, Reactor, WindowFilter, WindowState, utils};
use crate::common::collections::HashSet;
use crate::sys::window_server::{self, WindowServerId};

/// Handler for window discovery events, responsible for processing newly discovered windows
/// and managing the lifecycle of window state in the reactor.
pub struct WindowDiscoveryHandler;

impl WindowDiscoveryHandler {
    fn sync_existing_window_state(
        reactor: &mut Reactor,
        wid: WindowId,
        info: &WindowInfo,
        old_sys_id: Option<WindowServerId>,
    ) {
        Self::sync_window_server_id_mapping(reactor, wid, old_sys_id, info.sys_id);

        let was_minimized = reactor
            .window_manager
            .window(wid)
            .is_some_and(|window| window.info.is_minimized);

        if let Some(existing) = reactor.window_manager.window_mut(wid) {
            existing.info.title = info.title.clone();
            if info.frame.size.width != 0.0 || info.frame.size.height != 0.0 {
                existing.frame_monotonic = info.frame;
            }
            existing.info.is_standard = info.is_standard;
            existing.info.is_root = info.is_root;
            existing.info.is_resizable = info.is_resizable;
            existing.info.min_size = info.min_size;
            existing.info.max_size = info.max_size;
            existing.info.sys_id = info.sys_id;
            existing.info.bundle_id = info.bundle_id.clone();
            existing.info.path = info.path.clone();
            existing.info.ax_role = info.ax_role.clone();
            existing.info.ax_subrole = info.ax_subrole.clone();
        } else {
            return;
        }

        match (was_minimized, info.is_minimized) {
            (false, true) => WindowEventHandler::handle_window_minimized(reactor, wid),
            (true, false) => WindowEventHandler::handle_window_deminiaturized(reactor, wid),
            _ => {
                let manageable = utils::compute_window_manageability(
                    info.sys_id,
                    info.is_minimized,
                    info.is_standard,
                    info.is_root,
                    |wsid| reactor.window_manager.get_window_server_info(wsid),
                );
                if let Some(existing) = reactor.window_manager.window_mut(wid) {
                    existing.info.is_minimized = info.is_minimized;
                    existing.is_manageable = manageable;
                }
            }
        }

        if was_minimized != info.is_minimized {
            debug!(
                ?wid,
                was_minimized,
                is_minimized = info.is_minimized,
                "Window minimize state reconciled from discovery"
            );
        }
    }

    /// Handle a windows discovered event with app info.
    pub fn handle_discovery(
        reactor: &mut Reactor,
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
        // pending_refresh: bool,
        app_info: Option<AppInfo>,
    ) {
        // If app_info wasn't provided, try to look it up from our running app state so
        // we can apply workspace rules immediately on first discovery.
        let app_info =
            app_info.or_else(|| reactor.app_manager.apps.get(&pid).map(|app| app.info.clone()));

        let (stale_windows, pending_refresh) =
            Self::identify_stale_windows(reactor, pid, &known_visible);
        Self::cleanup_stale_windows(reactor, pid, stale_windows, pending_refresh);
        let new_windows = Self::process_window_list(reactor, new, &app_info);
        let discovered_new_windows: Vec<WindowId> =
            new_windows.iter().map(|(wid, _)| *wid).collect();
        Self::update_window_states(reactor, new_windows, &app_info);

        Self::emit_layout_events(reactor, pid, &known_visible, &app_info);
        reactor.consume_focus_next_window_from(discovered_new_windows);
    }

    fn sync_window_server_id_mapping(
        reactor: &mut Reactor,
        wid: WindowId,
        old_sys_id: Option<WindowServerId>,
        new_sys_id: Option<WindowServerId>,
    ) {
        if old_sys_id != new_sys_id
            && let Some(old_wsid) = old_sys_id
            && reactor.window_manager.tracked_window_id(old_wsid) == Some(wid)
        {
            reactor.window_manager.remove_window_server_state(old_wsid);
        }

        if let Some(new_wsid) = new_sys_id {
            reactor.window_manager.track_window_server_id(new_wsid, wid);
        }
    }

    /// Identify windows that should be removed as stale.
    fn identify_stale_windows(
        reactor: &Reactor,
        pid: pid_t,
        known_visible: &[WindowId],
    ) -> (Vec<WindowId>, bool) {
        const MIN_REAL_WINDOW_DIMENSION: f64 = 2.0;

        let known_visible_set: HashSet<WindowId> = known_visible.iter().cloned().collect();
        // TODO: maybe remove pid here, and use pending_refresh bool
        let pending_refresh =
            reactor.mission_control_manager.pending_mission_control_refresh.contains(&pid);

        let has_window_server_visibles_without_ax = {
            let known_visible_set = &known_visible_set;
            reactor
                .window_manager
                .iter_visible_window_server_ids()
                .filter_map(|wsid| reactor.window_manager.tracked_window_id(wsid))
                .any(|wid| wid.pid == pid && !known_visible_set.contains(&wid))
        };
        // TODO: Rewrite it
        let skip_stale_cleanup = matches!(
            reactor.refocus_manager.stale_cleanup_state,
            crate::actor::reactor::StaleCleanupState::Suppressed
        ) || pending_refresh
            || reactor.is_mission_control_active()
            || reactor.is_in_drag()
            || (known_visible_set.is_empty()
                && !reactor.has_visible_window_server_ids_for_pid(pid))
            || has_window_server_visibles_without_ax;

        if skip_stale_cleanup {
            return (Vec::new(), false);
        }

        let active_space_windows: Option<HashSet<WindowServerId>> = {
            let active_space_ids = reactor.active_space_ids();

            if active_space_ids.is_empty() {
                None
            } else {
                let window_ids = crate::sys::window_server::space_window_list_for_connection(
                    &active_space_ids,
                    0,
                    true,
                );
                let mut set = HashSet::default();
                set.extend(window_ids.into_iter().map(WindowServerId::new));
                Some(set)
            }
        };

        let stale_windows = reactor
            .window_manager
            .iter_windows()
            .filter_map(|(wid, state)| {
                if wid.pid != pid || known_visible_set.contains(&wid) {
                    return None;
                }

                if state.info.is_minimized {
                    return None;
                }

                let Some(ws_id) = state.info.sys_id else {
                    trace!(
                        ?wid,
                        "Skipping stale cleanup for window without window server id"
                    );
                    return None;
                };

                let is_on_active_space =
                    active_space_windows.as_ref().map_or(false, |set| set.contains(&ws_id));
                if active_space_windows.is_some() && !is_on_active_space {
                    trace!(
                        ?wid,
                        ws_id = ?ws_id,
                        "Skipping stale cleanup; window is not on an active space"
                    );
                    return None;
                }

                let server_info = reactor
                    .window_manager
                    .get_window_server_info(ws_id)
                    .or_else(|| window_server::get_window(ws_id));

                let info = match server_info {
                    Some(info) => info,
                    None => {
                        trace!(
                            ?wid,
                            ws_id = ?ws_id,
                            "Skipping stale cleanup for window without server info"
                        );
                        return None;
                    }
                };

                let width = info.frame.size.width.abs();
                let height = info.frame.size.height.abs();

                let unsuitable = !window_server::app_window_suitable(ws_id);
                let invalid_layer = info.layer != 0;
                let too_small =
                    width < MIN_REAL_WINDOW_DIMENSION || height < MIN_REAL_WINDOW_DIMENSION;
                let ordered_in = window_server::window_is_ordered_in(ws_id);
                let visible_in_snapshot = reactor.window_manager.is_window_visible(ws_id);

                if unsuitable
                    || invalid_layer
                    || too_small
                    || (is_on_active_space && !ordered_in && !visible_in_snapshot)
                {
                    Some(wid)
                } else {
                    None
                }
            })
            .collect();

        (stale_windows, pending_refresh)
    }

    /// Remove stale windows and send events.
    fn cleanup_stale_windows(
        reactor: &mut Reactor,
        pid: pid_t,
        stale_windows: Vec<WindowId>,
        pending_refresh: bool,
    ) {
        for wid in stale_windows {
            reactor.handle_event(Event::WindowDestroyed(wid));
        }
        if pending_refresh {
            reactor.mission_control_manager.pending_mission_control_refresh.remove(&pid);
        }
    }

    /// Process new and updated windows, returning lists of new and updated windows.
    fn process_window_list(
        reactor: &mut Reactor,
        new: Vec<(WindowId, WindowInfo)>,
        app_info: &Option<AppInfo>,
    ) -> Vec<(WindowId, WindowInfo)> {
        const APP_RULE_TTL_MS: u64 = 1000;

        let mut new_windows = Vec::new();

        reactor.window_manager.purge_expired(APP_RULE_TTL_MS);

        let any_recent = new.iter().any(|(_, info)| {
            info.sys_id.map_or(false, |wsid| {
                reactor.window_manager.is_wsid_recent(wsid, APP_RULE_TTL_MS)
            })
        });

        if any_recent && app_info.is_none() && !new.is_empty() {
            // Update state for any newly reported windows, but do not early-return;
            // proceed to emit WindowsOnScreenUpdated so existing mappings are respected
            // without reapplying app rules.
            for (wid, info) in &new {
                if reactor.window_manager.contains_window(*wid) {
                    let old_sys_id =
                        reactor.window_manager.window(*wid).and_then(|window| window.info.sys_id);
                    Self::sync_existing_window_state(reactor, *wid, info, old_sys_id);
                } else {
                    let mut state: WindowState = WindowState::from((*info).clone());
                    let manageable = utils::compute_window_manageability(
                        state.info.sys_id,
                        state.info.is_minimized,
                        state.info.is_standard,
                        state.info.is_root,
                        |wsid| reactor.window_manager.get_window_server_info(wsid),
                    );
                    state.is_manageable = manageable;
                    reactor.window_manager.insert_window(*wid, state);
                }
                Self::sync_window_server_id_mapping(reactor, *wid, None, info.sys_id);
            }
            // fall through
        }

        // Process all new windows
        for (wid, info) in new {
            if reactor.window_manager.contains_window(wid) {
                let old_sys_id =
                    reactor.window_manager.window(wid).and_then(|window| window.info.sys_id);
                Self::sync_existing_window_state(reactor, wid, &info, old_sys_id);
            } else {
                Self::sync_window_server_id_mapping(reactor, wid, None, info.sys_id);
                new_windows.push((wid, info));
            }
        }

        new_windows
    }

    /// Update window states in Reactor.
    fn update_window_states(
        reactor: &mut Reactor,
        new_windows: Vec<(WindowId, WindowInfo)>,
        _app_info: &Option<AppInfo>,
    ) {
        // Update or insert window states
        for (wid, info) in new_windows {
            let mut state: WindowState = info.into();
            let manageable = utils::compute_window_manageability(
                state.info.sys_id,
                state.info.is_minimized,
                state.info.is_standard,
                state.info.is_root,
                |wsid| reactor.window_manager.get_window_server_info(wsid),
            );
            state.is_manageable = manageable;
            reactor.window_manager.insert_window(wid, state);
        }
    }

    /// Send layout events for discovered windows.
    fn emit_layout_events(
        reactor: &mut Reactor,
        pid: pid_t,
        _known_visible: &[WindowId],
        app_info: &Option<AppInfo>,
    ) {
        let Some(app_info) = app_info
            .clone()
            .or_else(|| reactor.app_manager.apps.get(&pid).map(|app| app.info.clone()))
        else {
            return;
        };
        let windows = reactor
            .window_manager
            .iter_windows()
            .filter(|(window, state)| {
                window.pid == pid && state.matches_filter(WindowFilter::Manageable)
            })
            .map(|(window, _)| window)
            .collect::<Vec<_>>();
        reactor.process_windows_for_app_rules(pid, windows, app_info);

        if let Some(main_window) = reactor.main_window()
            && let Some(space) = reactor.main_window_space()
            && reactor.is_space_active(space)
        {
            reactor.send_layout_event(LayoutEvent::WindowFocused(main_window));
        }
    }

}
