//! The Reactor's job is to maintain coherence between the system and model state.
//!
//! It takes events from the rest of the system and builds a coherent picture of
//! what is going on. It shares this with the layout actor, and reacts to layout
//! changes by sending requests out to the other actors in the system.

mod animation;
pub(crate) use animation::AnimationCommandCompletion;
mod display_topology;
mod events;
mod main_window;
mod managers;
mod query;
mod replay;
pub mod transaction_manager;
mod utils;

#[cfg(test)]
mod testing;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use animation::Sender as AnimationSender;
use events::app::AppEventHandler;
use events::command::CommandEventHandler;
use events::drag::DragEventHandler;
use events::space::SpaceEventHandler;
use events::system::SystemEventHandler;
use events::window::WindowEventHandler;
use main_window::MainWindowTracker;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
pub use replay::{Record, replay};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use tracing::{debug, info, instrument, trace, warn};
use transaction_manager::TransactionId;

use super::{event_tap, gesture_tap};
use crate::actor::app::{AppInfo, AppThreadHandle, Quiet, Request, WindowId, WindowInfo, pid_t};
use crate::actor::broadcast::{BroadcastEvent, BroadcastSender};
use crate::actor::raise_manager::{self, RaiseManager, RaiseRequest};
use crate::actor::reactor::events::window_discovery::WindowDiscoveryHandler;
use crate::actor::{self, menu_bar, stack_line};
use crate::common::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use crate::common::config::Config;
use crate::core::rules::{RuleDecision, RuleSet, WindowIdentity};
use crate::model::layout::{self as layout, Direction};
use crate::model::space_activation::{SpaceActivationConfig, SpaceActivationPolicy};
use crate::model::tx_store::WindowTxStore;
use crate::runtime::SnapshotStore;
use crate::runtime::diagnostics::{DiagnosticLog, UserInputTrace};
use crate::sys::event::MouseState;
use crate::sys::executor::Executor;
use crate::sys::geometry::{CGRectDef, CGRectExt};
pub use crate::sys::screen::ScreenInfo;
use crate::sys::screen::{SpaceId, get_active_space_number};
use crate::sys::window_server::{
    self, WindowServerId, WindowServerInfo, current_cursor_location, space_is_fullscreen,
    wait_for_native_fullscreen_transition, window_level, window_sub_level,
};

pub type Sender = actor::Sender<Event>;
type Receiver = actor::Receiver<Event>;
pub use query::ReactorSnapshotHandle;

const FOCUS_NEXT_WINDOW_TIMEOUT: Duration = Duration::from_secs(8);
const AUXILIARY_WINDOW_EXPANSION_TIMEOUT: Duration = Duration::from_secs(3);
const RECENT_WORKSPACE_TARGET_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) use crate::model::reactor::{
    AppState, FullscreenSpaceTrack, FullscreenWindowTrack, PendingSpaceChange, WindowFilter,
    WindowState,
};
pub use crate::model::reactor::{
    Command, DisplaySelector, DragSession, DragState, MenuState, ReactorCommand, RefocusState,
    Requested, StaleCleanupState, WorkspaceSwitchOrigin, WorkspaceSwitchState,
};

#[derive(Clone)]
pub struct ReactorHandle {
    sender: Sender,
    snapshots: ReactorSnapshotHandle,
}

impl ReactorHandle {
    pub fn new(sender: Sender, snapshots: ReactorSnapshotHandle) -> Self {
        Self { sender, snapshots }
    }

    pub fn sender(&self) -> Sender { self.sender.clone() }

    pub fn send(&self, event: Event) { self.sender.send(event) }

    pub fn try_send(
        &self,
        event: Event,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<(tracing::Span, Event)>> {
        self.sender.try_send(event)
    }
}

impl std::ops::Deref for ReactorHandle {
    type Target = ReactorSnapshotHandle;

    fn deref(&self) -> &Self::Target { &self.snapshots }
}

use display_topology::{DisplaySnapshot, DisplayTopologyManager, WindowSnapshot};

#[derive(Clone, Debug)]
pub(crate) enum LayoutEvent {
    Changed,
    WindowAdded(SpaceId, WindowId),
    WindowFocused(WindowId),
}

#[serde_as]
#[derive(Serialize, Deserialize, Debug)]
pub enum Event {
    /// The screen layout, including resolution, changed. This is always the
    /// first event sent on startup.
    ///
    /// The first vec is the snapshot for each screen. The main screen is always
    /// first in the list.
    ScreenParametersChanged(Vec<ScreenInfo>),

    /// The current space changed.
    ///
    /// There is one SpaceId per screen in the last ScreenParametersChanged
    /// event. `None` in the SpaceId vec disables managing windows on that
    /// screen until the next space change.
    SpaceChanged(Vec<Option<SpaceId>>),

    /// An application was launched. This event is also sent for every running
    /// application on startup.
    ///
    /// Both WindowInfo (accessibility) and WindowServerInfo are collected for
    /// any already-open windows when the launch event is sent. Since this
    /// event isn't ordered with respect to the Space events, it is possible to
    /// receive this event for a space we just switched off of.. FIXME. The same
    /// is true of WindowCreated events.
    ApplicationLaunched {
        pid: pid_t,
        info: AppInfo,
        #[serde(skip, default = "replay::deserialize_app_thread_handle")]
        handle: AppThreadHandle,
        is_frontmost: bool,
        main_window: Option<WindowId>,
        visible_windows: Vec<(WindowId, WindowInfo)>,
        window_server_info: Vec<WindowServerInfo>,
    },
    ApplicationTerminated(pid_t),
    ApplicationThreadTerminated(pid_t),
    ApplicationActivated(pid_t, Quiet),
    ApplicationDeactivated(pid_t),
    ApplicationGloballyActivated(pid_t),
    ApplicationGloballyDeactivated(pid_t),
    ApplicationMainWindowChanged(pid_t, Option<WindowId>, Quiet),

    WindowsDiscovered {
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
    },
    WindowCreated(
        WindowId,
        WindowInfo,
        Option<WindowServerInfo>,
        Option<MouseState>,
    ),
    WindowDestroyed(WindowId),
    #[serde(skip)]
    WindowServerDestroyed(crate::sys::window_server::WindowServerId, SpaceId),
    #[serde(skip)]
    WindowServerAppeared(crate::sys::window_server::WindowServerId, SpaceId),
    #[serde(skip)]
    SpaceCreated(SpaceId),
    #[serde(skip)]
    SpaceDestroyed(SpaceId),
    WindowMinimized(WindowId),
    WindowDeminiaturized(WindowId),
    WindowFrameChanged(
        WindowId,
        #[serde(with = "CGRectDef")] CGRect,
        Option<TransactionId>,
        Requested,
        Option<MouseState>,
    ),
    WindowTitleChanged(WindowId, String),
    ResyncAppForWindow(WindowServerId),
    MenuOpened(pid_t),
    MenuClosed(pid_t),

    /// Left mouse button was released.
    ///
    /// Layout changes are suppressed while the button is down so that they
    /// don't interfere with drags. This event is used to update the layout in
    /// case updates were supressed while the button was down.
    ///
    /// FIXME: This can be interleaved incorrectly with the MouseState in app
    /// actor events.
    MouseUp,
    /// The left mouse button was pressed. The hit window and click location are
    /// captured before an auxiliary controller can disappear in response.
    MouseDown(
        Option<WindowServerInfo>,
        #[serde(with = "crate::sys::geometry::CGPointDef")] CGPoint,
    ),
    /// The mouse cursor moved over a new window. Only sent if focus-follows-
    /// mouse is enabled.
    MouseMoved(#[serde(with = "crate::sys::geometry::CGPointDef")] CGPoint),
    /// System woke from sleep; used to re-subscribe SLS notifications.
    SystemWoke,
    /// The login session became active after an unlock or user switch.
    SessionDidBecomeActive,

    #[serde(skip)]
    DisplayChurnBegin,
    #[serde(skip)]
    DisplayChurnEnd,

    #[serde(skip)]
    MissionControlNativeEntered,
    #[serde(skip)]
    MissionControlNativeExited,

    /// A raise request completed. Used by the raise manager to track when
    /// all raise requests in a sequence have finished.
    RaiseCompleted {
        window_id: WindowId,
        sequence_id: u64,
    },

    /// A raise sequence timed out. Used by the raise manager to clean up
    /// pending raises that took too long.
    RaiseTimeout {
        sequence_id: u64,
    },

    Command(Command),

    #[serde(skip)]
    RegisterWmSender(crate::actor::wm_controller::Sender),

    #[serde(skip)]
    ConfigUpdated(Config),
    UserInput(UserInputTrace),
}

fn untracked_window_is_focusable(info: &WindowServerInfo) -> bool { info.layer == 0 }

fn lifecycle_activation_suppression(event: &Event) -> Option<bool> {
    match event {
        Event::SystemWoke | Event::SessionDidBecomeActive => Some(true),
        Event::MouseDown(..)
        | Event::MouseUp
        | Event::MouseMoved(_)
        | Event::UserInput(_)
        | Event::Command(_) => Some(false),
        _ => None,
    }
}

pub struct Reactor {
    pub config: Config,
    pub one_space: bool,
    app_manager: managers::AppManager,
    window_manager: managers::WindowManager,
    space_manager: managers::SpaceManager,
    space_activation_policy: SpaceActivationPolicy,
    main_window_tracker: MainWindowTracker,
    drag_manager: managers::DragManager,
    workspace_switch_manager: managers::WorkspaceSwitchManager,
    recording_manager: managers::RecordingManager,
    communication_manager: managers::CommunicationManager,
    notification_manager: managers::NotificationManager,
    transaction_manager: transaction_manager::TransactionManager,
    menu_manager: managers::MenuManager,
    mission_control_manager: managers::MissionControlManager,
    refocus_manager: managers::RefocusManager,
    pending_space_change_manager: managers::PendingSpaceChangeManager,
    active_spaces: HashSet<SpaceId>,
    display_topology_manager: DisplayTopologyManager,
    pub above_window: Option<WindowServerId>,
    pub animation_tx: Option<AnimationSender>,
    snapshot_store: SnapshotStore,
    window_rules: RuleSet,
    core_state: Option<crate::core::state::CoreState>,
}

impl Reactor {
    pub fn spawn(
        config: Config,
        record: Record,
        event_tap_tx: event_tap::Sender,
        broadcast_tx: BroadcastSender,
        menu_tx: menu_bar::Sender,
        stack_line_tx: stack_line::Sender,
        window_notify: Option<(crate::actor::window_notify::Sender, WindowTxStore)>,
        gesture_tap_tx: Option<gesture_tap::Sender>,
        one_space: bool,
    ) -> ReactorHandle {
        let (events_tx, events) = actor::channel();
        let events_tx_clone = events_tx.clone();
        let restore_path = crate::common::config::restore_file();
        let mut reactor = Reactor::new(
            config,
            record,
            broadcast_tx,
            window_notify,
            one_space,
            Some(&restore_path),
        );
        reactor.communication_manager.event_tap_tx = Some(event_tap_tx);
        reactor.menu_manager.menu_tx = Some(menu_tx);
        reactor.communication_manager.stack_line_tx = Some(stack_line_tx);
        reactor.communication_manager.gesture_tap_tx = gesture_tap_tx;
        reactor.communication_manager.events_tx = Some(events_tx_clone.clone());
        let _ = reactor.publish_core_snapshot();
        let snapshot_handle = ReactorSnapshotHandle::new(reactor.snapshot_store.clone());
        thread::Builder::new()
            .name("reactor".to_string())
            .spawn(move || {
                Executor::run(Reactor::run(reactor, events, events_tx_clone));
            })
            .unwrap();
        ReactorHandle::new(events_tx, snapshot_handle)
    }

    pub fn new(
        config: Config,
        mut record: Record,
        broadcast_tx: BroadcastSender,
        window_notify: Option<(crate::actor::window_notify::Sender, WindowTxStore)>,
        one_space: bool,
        restore_path: Option<&Path>,
    ) -> Reactor {
        // FIXME: Remove apps that are no longer running from restored state.
        record.start(&config);
        let core_config = crate::interfaces::config::core_config(&config)
            .expect("validated Lift configuration must translate to core configuration");
        let window_rules = RuleSet::compile(core_config.window_rules.clone())
            .expect("validated Lift configuration must contain valid window rules");
        let core_state = restore_path
            .and_then(|path| match crate::runtime::persistence::load(path) {
                Ok(persisted) => match crate::core::state::CoreState::from_persisted(
                    core_config.clone(),
                    &persisted,
                ) {
                    Ok(state) => {
                        info!(
                            ?path,
                            workspaces = persisted.workspaces.len(),
                            "Restored Lift state"
                        );
                        Some(state)
                    }
                    Err(error) => {
                        warn!(?path, ?error, "Ignoring invalid persisted Lift state");
                        None
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    warn!(?path, ?error, "Could not load persisted Lift state");
                    None
                }
            })
            .unwrap_or_else(|| crate::core::state::CoreState::new(core_config));
        let (raise_manager_tx, _rx) = actor::channel();
        let (window_notify_tx, window_tx_store) = match window_notify {
            Some((tx, store)) => (Some(tx), store),
            None => (None, WindowTxStore::new()),
        };
        let diagnostic_settings = {
            let settings = config.settings.diagnostics.clone();
            #[cfg(test)]
            let settings =
                crate::common::config::DiagnosticsSettings { enabled: false, ..settings };
            settings
        };
        let reactor = Reactor {
            config: config.clone(),
            one_space,
            app_manager: managers::AppManager::new(),
            window_manager: Box::default(),
            space_manager: managers::SpaceManager {
                screens: vec![],
                fullscreen_by_space: HashMap::default(),
                has_seen_display_set: false,
            },
            space_activation_policy: SpaceActivationPolicy::new(),
            main_window_tracker: MainWindowTracker::default(),
            drag_manager: managers::DragManager {
                drag_state: DragState::Inactive,
                skip_layout_for_window: None,
            },
            workspace_switch_manager: managers::WorkspaceSwitchManager {
                workspace_switch_state: WorkspaceSwitchState::Inactive,
                workspace_switch_generation: 0,
                active_workspace_switch: None,
                pending_workspace_switch_origin: None,
                pending_workspace_mouse_warp: None,
                suppress_auto_workspace_switch_until_input: false,
                quiet_activation_deadlines: HashMap::default(),
            },
            recording_manager: managers::RecordingManager {
                record,
                diagnostics: DiagnosticLog::new(
                    diagnostic_settings,
                    crate::common::config::diagnostics_file(),
                ),
            },
            communication_manager: managers::CommunicationManager {
                event_tap_tx: None,
                gesture_tap_tx: None,
                stack_line_tx: None,
                raise_manager_tx,
                event_broadcaster: broadcast_tx,
                wm_sender: None,
                events_tx: None,
            },
            notification_manager: managers::NotificationManager {
                last_sls_notification_ids: Vec::new(),
                _window_notify_tx: window_notify_tx,
            },
            transaction_manager: transaction_manager::TransactionManager::new(window_tx_store),
            menu_manager: managers::MenuManager {
                menu_state: MenuState::Closed,
                menu_tx: None,
            },
            mission_control_manager: managers::MissionControlManager {
                pending_mission_control_refresh: HashSet::default(),
            },
            refocus_manager: managers::RefocusManager {
                stale_cleanup_state: StaleCleanupState::Enabled,
                refocus_state: RefocusState::None,
                focus_next_window_deadline: None,
                focus_next_window_target: None,
                auxiliary_window_workspace_target: None,
                recent_workspace_targets: HashMap::default(),
            },
            pending_space_change_manager: managers::PendingSpaceChangeManager {
                pending_space_change: None,
                topology_relayout_pending: false,
                pending_removed_display_uuids: HashSet::default(),
            },
            active_spaces: HashSet::default(),
            display_topology_manager: DisplayTopologyManager::default(),
            above_window: None,
            animation_tx: None,
            snapshot_store: SnapshotStore::default(),
            window_rules,
            core_state: Some(core_state),
        };
        reactor
    }

    fn window_rule_decision(&self, wid: WindowId, app_info: Option<&AppInfo>) -> RuleDecision {
        let window = self.window_manager.window(wid);
        self.window_rules.decide(WindowIdentity {
            app_id: app_info.and_then(|app| app.bundle_id.as_deref()),
            app_name: app_info.and_then(|app| app.localized_name.as_deref()),
            title: window.map(|window| window.info.title.as_str()),
            ax_role: window.and_then(|window| window.info.ax_role.as_deref()),
            ax_subrole: window.and_then(|window| window.info.ax_subrole.as_deref()),
        })
    }

    fn set_active_spaces(&mut self, spaces: &[Option<SpaceId>]) {
        self.active_spaces.clear();
        for space in spaces.iter().flatten().copied() {
            self.active_spaces.insert(space);
        }
    }

    fn is_space_active(&self, space: SpaceId) -> bool { self.active_spaces.contains(&space) }

    fn iter_active_spaces(&self) -> impl Iterator<Item = SpaceId> + '_ {
        self.active_spaces.iter().copied()
    }

    fn active_space_ids(&self) -> Vec<u64> {
        self.active_spaces.iter().map(|space| space.get()).collect()
    }

    fn is_window_on_active_space(&self, wid: WindowId) -> bool {
        self.intended_space_for_window_id(wid)
            .is_some_and(|space| self.is_space_active(space))
    }

    fn activation_cfg(&self) -> SpaceActivationConfig {
        SpaceActivationConfig {
            default_disable: self.config.settings.default_disable,
            one_space: self.one_space,
        }
    }

    fn screens_for_current_spaces(&self) -> Vec<ScreenInfo> { self.space_manager.screens.clone() }

    fn screens_for_spaces(&self, spaces: &[Option<SpaceId>]) -> Vec<ScreenInfo> {
        self.space_manager
            .screens
            .iter()
            .zip(spaces.iter().copied())
            .map(|(screen, space)| ScreenInfo { space, ..screen.clone() })
            .collect()
    }

    fn display_uuids_for_current_screens(&self) -> Vec<Option<String>> {
        self.space_manager
            .screens
            .iter()
            .map(|screen| screen.display_uuid_owned())
            .collect()
    }

    fn raw_spaces_for_current_screens(&self) -> Vec<Option<SpaceId>> {
        self.space_manager.screens.iter().map(|s| s.space).collect()
    }

    fn display_uuid_for_space(&self, space: SpaceId) -> Option<String> {
        self.space_manager
            .screen_by_space(space)
            .and_then(|screen| screen.display_uuid_owned())
    }

    fn expose_space_if_known(&mut self, space: SpaceId) {
        if self.space_manager.screen_by_space(space).is_none() {
            return;
        }
        self.send_layout_event(LayoutEvent::Changed);
    }

    fn recompute_and_set_active_spaces(&mut self, spaces: &[Option<SpaceId>]) {
        let cfg = self.activation_cfg();
        let display_uuids = self.display_uuids_for_current_screens();
        let active_spaces =
            self.space_activation_policy.compute_active_spaces(cfg, spaces, &display_uuids);
        let previous_active = self.active_spaces.clone();
        self.set_active_spaces(&active_spaces);
        self.handle_active_space_change(previous_active);
    }

    fn recompute_and_set_active_spaces_from_current_screens(&mut self) {
        let raw_spaces = self.raw_spaces_for_current_screens();
        self.recompute_and_set_active_spaces(&raw_spaces);
    }

    fn handle_active_space_change(&mut self, previous_active: HashSet<SpaceId>) {
        if previous_active == self.active_spaces {
            return;
        }

        let deactivated: Vec<SpaceId> =
            previous_active.difference(&self.active_spaces).copied().collect();
        let activated: Vec<SpaceId> =
            self.active_spaces.difference(&previous_active).copied().collect();

        // Do not remove windows when a space is merely deactivated (e.g. macOS Space
        // switches). Removing them clears workspace assignments and causes windows
        // without app rules to be re-assigned to the current workspace.

        if !activated.is_empty() {
            for space in &activated {
                self.expose_space_if_known(*space);
            }
        }

        if !activated.is_empty() || !deactivated.is_empty() {
            self.refresh_window_server_snapshot_for_active_spaces();
            if !self.pending_space_change_manager.topology_relayout_pending {
                self.check_for_new_windows();
            }
        }

        if !activated.is_empty() {
            self.apply_app_rules_for_activated_spaces(&activated);
        }
    }

    fn apply_app_rules_for_activated_spaces(&mut self, activated: &[SpaceId]) {
        let activated_set: HashSet<SpaceId> = activated.iter().copied().collect();
        let mut windows_by_pid: HashMap<pid_t, Vec<WindowId>> = HashMap::default();

        for (wid, state) in self.window_manager.iter_windows() {
            if !state.matches_filter(WindowFilter::Manageable) {
                continue;
            }
            let Some(space) = self.intended_space_for_window_state(wid, state) else {
                continue;
            };

            if !activated_set.contains(&space) {
                continue;
            }

            windows_by_pid.entry(wid.pid).or_default().push(wid);
        }

        for (pid, window_ids) in windows_by_pid {
            let Some(app_state) = self.app_manager.apps.get(&pid) else {
                continue;
            };

            self.process_windows_for_app_rules(pid, window_ids, app_state.info.clone());
        }
    }

    fn refresh_window_server_snapshot_for_active_spaces(&mut self) {
        let ws_info = self.authoritative_window_snapshot_for_active_spaces();
        self.update_complete_window_server_info(ws_info);
    }

    fn authoritative_window_snapshot_for_active_spaces(&self) -> Vec<WindowServerInfo> {
        let ws_info = window_server::get_visible_windows_with_layer(None);
        self.filter_ws_info_to_active_spaces(ws_info)
    }

    fn build_display_snapshot(&self, ws_info: Vec<WindowServerInfo>) -> DisplaySnapshot {
        let ordered_screens = self.space_manager.screens.clone();
        let active_spaces = self.active_spaces.clone();

        let mut inactive_spaces: HashSet<SpaceId> = HashSet::default();
        for space in ordered_screens.iter().filter_map(|s| s.space) {
            if !active_spaces.contains(&space) {
                inactive_spaces.insert(space);
            }
        }

        let windows = ws_info
            .into_iter()
            .map(|info| {
                let space = window_server::window_space(info.id);
                (info.id, WindowSnapshot { info, space })
            })
            .collect();

        DisplaySnapshot {
            ordered_screens,
            active_spaces,
            inactive_spaces,
            windows,
        }
    }

    fn maybe_commit_display_topology_snapshot(&mut self) -> bool {
        let Some((epoch, started_at, flags, pre_known_wsids)) =
            self.display_topology_manager.take_awaiting_commit()
        else {
            return false;
        };

        if self.space_manager.screens.is_empty()
            || self.space_manager.screens.iter().any(|screen| screen.space.is_none())
        {
            // Topology is not stable yet; keep waiting for the next complete snapshot.
            self.display_topology_manager.restore_awaiting_commit(
                epoch,
                started_at,
                flags,
                pre_known_wsids,
            );
            return false;
        }

        let mut unique_spaces = HashSet::default();
        if self
            .space_manager
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .any(|space| !unique_spaces.insert(space))
        {
            self.display_topology_manager.restore_awaiting_commit(
                epoch,
                started_at,
                flags,
                pre_known_wsids,
            );
            return false;
        }

        let ws_info = self.authoritative_window_snapshot_for_active_spaces();
        let snapshot = self.build_display_snapshot(ws_info);
        self.reconcile_windows_after_topology_commit(
            epoch,
            started_at,
            flags,
            pre_known_wsids,
            snapshot,
        );
        self.display_topology_manager.mark_stable();
        true
    }

    fn reconcile_windows_after_topology_commit(
        &mut self,
        epoch: u64,
        started_at: std::time::Instant,
        flags: crate::sys::skylight::DisplayReconfigFlags,
        pre_known_wsids: HashSet<WindowServerId>,
        snapshot: DisplaySnapshot,
    ) {
        let post_visible_wsids: HashSet<WindowServerId> =
            snapshot.windows.keys().copied().collect();
        let appeared: Vec<WindowServerId> =
            post_visible_wsids.difference(&pre_known_wsids).copied().collect();
        let disappeared: Vec<WindowServerId> =
            pre_known_wsids.difference(&post_visible_wsids).copied().collect();

        let mut synthetic_appeared = 0u64;
        let mut synthetic_destroyed = 0u64;

        for wsid in appeared {
            let Some(snapshot_window) = snapshot.windows.get(&wsid) else {
                continue;
            };
            if snapshot_window.info.layer != 0 {
                continue;
            }
            let Some(space) = snapshot_window.space else {
                continue;
            };
            if !self.is_space_active(space) && !window_server::space_is_user(space.get()) {
                continue;
            }
            SpaceEventHandler::handle_window_server_snapshot_appeared(
                self,
                snapshot_window.info,
                space,
            );
            synthetic_appeared += 1;
        }

        for wsid in disappeared {
            let still_exists = window_server::get_window(wsid).is_some();
            let spaces = window_server::window_spaces(wsid);
            let in_user_or_active = spaces.iter().any(|space| {
                window_server::space_is_user(space.get()) || self.is_space_active(*space)
            });
            if still_exists && in_user_or_active {
                continue;
            }
            let sid = window_server::window_space(wsid)
                .or_else(|| self.space_manager.first_known_space());
            let Some(sid) = sid else {
                continue;
            };
            SpaceEventHandler::handle_window_server_destroyed(self, wsid, sid);
            synthetic_destroyed += 1;
        }

        self.force_refresh_all_windows();
        let _ = self.update_layout_or_warn_with(
            false,
            false,
            "Layout update failed after display churn commit",
        );

        info!(
            epoch,
            flags = ?flags,
            duration_ms = started_at.elapsed().as_millis(),
            synthetic_appeared,
            synthetic_destroyed,
            active_spaces = snapshot.active_spaces.len(),
            inactive_spaces = snapshot.inactive_spaces.len(),
            screens = snapshot.ordered_screens.len(),
            "display topology commit reconciled"
        );
    }

    fn filter_ws_info_to_active_spaces(
        &self,
        ws_info: Vec<WindowServerInfo>,
    ) -> Vec<WindowServerInfo> {
        let active_space_ids = self.active_space_ids();
        if active_space_ids.is_empty() {
            return Vec::new();
        }

        let active_window_ids: std::collections::HashSet<u32> =
            crate::sys::window_server::space_window_list_for_connection(
                &active_space_ids,
                0,
                false,
            )
            .into_iter()
            .collect();

        ws_info
            .into_iter()
            .filter(|w| active_window_ids.contains(&w.id.as_u32()))
            .collect()
    }

    fn is_login_window_pid(&self, pid: pid_t) -> bool {
        self.app_manager.apps.get(&pid).and_then(|a| a.info.bundle_id.as_deref())
            == Some("com.apple.loginwindow")
    }

    // fn store_txid(&self, wsid: Option<WindowServerId>, txid: TransactionId, target: CGRect) {
    //     self.transaction_manager.store_txid(wsid, txid, target);
    // }
    //
    // fn update_txid_entries<I>(&self, entries: I)
    // where
    //     I: IntoIterator<Item = (WindowServerId, TransactionId, CGRect)>,
    // {
    //     self.transaction_manager.update_entries(entries);
    // }
    //
    // fn remove_txid_for_window(&self, wsid: Option<WindowServerId>) {
    //     self.transaction_manager.remove_for_window(wsid);
    // }

    fn is_in_drag(&self) -> bool {
        matches!(self.drag_manager.drag_state, DragState::Active { .. })
    }

    fn is_mission_control_active(&self) -> bool {
        self.core_state.as_ref().is_some_and(|core| {
            core.snapshot().mission_control == crate::core::interaction::MissionControlPhase::Active
        })
    }

    fn get_pending_drag_swap(&self) -> Option<(WindowId, WindowId)> {
        let drag = self.core_drag_snapshot();
        let window = drag.window?;
        let target = drag.target?;
        Some((
            WindowId::new(window.application.0, window.index.get()),
            WindowId::new(target.application.0, target.index.get()),
        ))
    }

    fn get_active_drag_session(&self) -> Option<&DragSession> {
        if let DragState::Active { session } = &self.drag_manager.drag_state {
            Some(session)
        } else {
            None
        }
    }

    fn get_active_drag_session_mut(&mut self) -> Option<&mut DragSession> {
        if let DragState::Active { session } = &mut self.drag_manager.drag_state {
            Some(session)
        } else {
            None
        }
    }

    fn take_active_drag_session(&mut self) -> Option<DragSession> {
        match std::mem::replace(&mut self.drag_manager.drag_state, DragState::Inactive) {
            DragState::Active { session } => Some(session),
            _ => None,
        }
    }

    async fn run(mut reactor: Reactor, events: Receiver, events_tx: Sender) {
        let (raise_manager_tx, raise_manager_rx) = actor::channel();
        let (animation_tx, animation_rx) = tokio::sync::mpsc::unbounded_channel();
        reactor.communication_manager.raise_manager_tx = raise_manager_tx.clone();
        reactor.animation_tx = Some(animation_tx);
        let event_tap_tx = reactor.communication_manager.event_tap_tx.clone();
        let reactor_task = Self::run_reactor_loop(reactor, events);
        let raise_manager_task = RaiseManager::run(raise_manager_rx, events_tx, event_tap_tx);
        let animation_task = animation::AnimationManager::run(animation_rx);
        let _ = tokio::join!(reactor_task, raise_manager_task, animation_task);
    }

    async fn run_reactor_loop(mut reactor: Reactor, mut events: Receiver) {
        const MAX_EVENT_BATCH: usize = 64;

        while let Some((span, event)) = events.recv().await {
            let _guard = span.enter();
            reactor.handle_loop_event(event);
            // Drain a bounded batch to reduce recv/select overhead.
            for _ in 1..MAX_EVENT_BATCH {
                let Ok((span, event)) = events.try_recv() else {
                    break;
                };
                let _guard = span.enter();
                reactor.handle_loop_event(event);
            }
        }
    }

    fn handle_loop_event(&mut self, event: Event) {
        if self.maybe_quarantine_during_churn(&event) {
            Self::note_windowserver_activity(&event);
            trace!(?event, "quarantined event during display churn");
            return;
        }
        Self::note_windowserver_activity(&event);
        self.handle_event(event);
        if let Err(error) = self.publish_core_snapshot() {
            debug!(
                ?error,
                "core snapshot publication deferred until runtime state stabilizes"
            );
        }
    }

    fn publish_core_snapshot(&mut self) -> Result<(), crate::core::error::CoreError> {
        let previous = self.snapshot_store.load();
        let mut snapshot = self.advance_core_state()?;
        snapshot.revision = previous.revision.saturating_add(1);
        self.snapshot_store.publish(snapshot);
        let current = self.snapshot_store.load();
        self.broadcast_windows_changed(&previous, &current);
        self.broadcast_layout_changed(&previous, &current);
        self.recording_manager.diagnostics.record_snapshot(&current);
        if let Some(stack_line_tx) = self.communication_manager.stack_line_tx.as_ref() {
            if let Err(error) =
                stack_line_tx.try_send(stack_line::Event::SnapshotUpdated(current.clone()))
            {
                debug!(?error, "stack line skipped a snapshot update");
            }
        }
        Ok(())
    }

    fn broadcast_windows_changed(
        &self,
        previous: &crate::core::snapshot::CoreSnapshot,
        current: &crate::core::snapshot::CoreSnapshot,
    ) {
        fn windows_by_display(
            snapshot: &crate::core::snapshot::CoreSnapshot,
        ) -> BTreeMap<
            crate::core::ids::DisplayId,
            BTreeMap<crate::core::ids::WorkspaceId, Vec<crate::core::ids::WindowId>>,
        > {
            let mut result = BTreeMap::new();
            for workspace in &snapshot.workspaces {
                let windows = workspace
                    .groups
                    .iter()
                    .flat_map(|group| group.windows.iter().copied())
                    .chain(workspace.floating_windows.iter().copied())
                    .collect();
                result
                    .entry(workspace.display.clone())
                    .or_insert_with(BTreeMap::new)
                    .insert(workspace.id, windows);
            }
            result
        }

        let previous_windows = windows_by_display(previous);
        let current_windows = windows_by_display(current);
        let display_ids = previous_windows
            .keys()
            .chain(current_windows.keys())
            .cloned()
            .collect::<BTreeSet<_>>();

        for display_id in display_ids {
            if previous_windows.get(&display_id) == current_windows.get(&display_id) {
                continue;
            }
            let Some(display) = current.displays.iter().find(|display| display.id == display_id)
            else {
                continue;
            };
            let (Some(space), Some(workspace_id)) = (display.space, display.active_workspace)
            else {
                continue;
            };
            let Some(workspace) =
                current.workspaces.iter().find(|workspace| workspace.id == workspace_id)
            else {
                continue;
            };
            let windows = current_windows
                .get(&display_id)
                .and_then(|workspaces| workspaces.get(&workspace_id))
                .into_iter()
                .flatten()
                .map(|window| format!("{:?}", Self::actor_window_id(*window)))
                .collect();
            let event = BroadcastEvent::WindowsChanged {
                workspace_id,
                workspace_name: workspace.name.clone(),
                windows,
                space_id: SpaceId::new(space.0),
                display_uuid: Some(display_id.0),
            };
            let _ = self.communication_manager.event_broadcaster.send(event);
        }
    }

    fn broadcast_layout_changed(
        &self,
        previous: &crate::core::snapshot::CoreSnapshot,
        current: &crate::core::snapshot::CoreSnapshot,
    ) {
        for display in &current.displays {
            let (Some(space), Some(workspace_id)) = (display.space, display.active_workspace)
            else {
                continue;
            };
            let Some(workspace) =
                current.workspaces.iter().find(|workspace| workspace.id == workspace_id)
            else {
                continue;
            };
            let current_layout = crate::interfaces::query::layout_state_for_workspace(
                current,
                workspace,
                crate::core::ids::SpaceId(space.0),
            );
            let previous_layout =
                previous.displays.iter().find(|candidate| candidate.id == display.id).and_then(
                    |candidate| {
                        let previous_workspace = candidate.active_workspace?;
                        let workspace = previous
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.id == previous_workspace)?;
                        Some((
                            previous_workspace,
                            crate::interfaces::query::layout_state_for_workspace(
                                previous,
                                workspace,
                                crate::core::ids::SpaceId(candidate.space?.0),
                            ),
                        ))
                    },
                );
            if previous_layout.as_ref() == Some(&(workspace_id, current_layout.clone())) {
                continue;
            }
            let event = BroadcastEvent::LayoutChanged {
                workspace_id,
                workspace_index: workspace.number.map(|number| number.get() as u64),
                workspace_name: workspace.name.clone(),
                layout: current_layout,
                space_id: SpaceId::new(space.0),
                display_uuid: Some(display.id.0.clone()),
            };
            let _ = self.communication_manager.event_broadcaster.send(event);
        }
    }

    fn transition_core_command(
        &mut self,
        command: crate::core::command::Command,
    ) -> Result<crate::core::state::Transition, crate::core::error::CoreError> {
        self.transition_core_input(crate::core::input::Input::Command(command))
    }

    fn layout_response_for_transition(
        &self,
        transition: &crate::core::state::Transition,
    ) -> layout::EventResponse {
        let mut response = layout::EventResponse::default();
        for effect in &transition.effects {
            match effect {
                crate::core::effect::Effect::FocusWindow(window) => {
                    response.focus_window = Some(Self::actor_window_id(*window));
                }
                crate::core::effect::Effect::RaiseWindow(window) => {
                    let window = Self::actor_window_id(*window);
                    if !response.raise_windows.contains(&window) {
                        response.raise_windows.push(window);
                    }
                }
                _ => {}
            }
        }
        for event in &transition.events {
            let crate::core::effect::DomainEvent::WorkspaceChanged { workspace, .. } = event else {
                continue;
            };
            let Some(snapshot) = transition
                .snapshot
                .workspaces
                .iter()
                .find(|candidate| candidate.id == *workspace)
            else {
                continue;
            };
            let mut windows = snapshot
                .groups
                .iter()
                .flat_map(|group| group.windows.iter().copied())
                .chain(snapshot.floating_windows.iter().copied())
                .map(Self::actor_window_id)
                .collect::<Vec<_>>();
            windows.dedup();
            response.raise_windows = windows;
            response.focus_window = snapshot
                .last_tiled_window
                .or(snapshot.last_floating_window)
                .or_else(|| {
                    snapshot
                        .groups
                        .iter()
                        .find_map(|group| group.windows.get(group.selected).copied())
                })
                .or_else(|| snapshot.floating_windows.first().copied())
                .map(Self::actor_window_id);
        }
        response
    }

    fn transition_core_input(
        &mut self,
        input: crate::core::input::Input,
    ) -> Result<crate::core::state::Transition, crate::core::error::CoreError> {
        self.core_state
            .as_mut()
            .ok_or_else(|| {
                crate::core::error::CoreError::IncompleteObservation(
                    "cannot execute a command before the first display snapshot".into(),
                )
            })?
            .transition(input)
    }

    fn core_drag_snapshot(&self) -> crate::core::interaction::DragSnapshot {
        self.core_state.as_ref().map(|core| core.snapshot().drag).unwrap_or_default()
    }

    fn core_snapshot(&self) -> std::sync::Arc<crate::core::snapshot::CoreSnapshot> {
        self.core_state
            .as_ref()
            .map(crate::core::state::CoreState::snapshot)
            .unwrap_or_default()
    }

    fn core_window_id(window: WindowId) -> crate::core::ids::WindowId {
        crate::core::ids::WindowId::new(crate::core::ids::ApplicationId(window.pid), window.idx)
    }

    fn actor_window_id(window: crate::core::ids::WindowId) -> WindowId {
        WindowId::new(window.application.0, window.index.get())
    }

    fn active_workspace_for_space(&self, space: SpaceId) -> Option<crate::core::ids::WorkspaceId> {
        let display = self.display_uuid_for_space(space)?;
        self.core_snapshot()
            .displays
            .iter()
            .find(|candidate| candidate.id.0 == display)
            .and_then(|candidate| candidate.active_workspace)
    }

    fn workspace_for_window(&self, window: WindowId) -> Option<crate::core::ids::WorkspaceId> {
        let window = Self::core_window_id(window);
        self.core_snapshot()
            .windows
            .iter()
            .find(|candidate| candidate.id == window)
            .and_then(|candidate| candidate.workspace)
    }

    fn space_for_workspace(&self, workspace: crate::core::ids::WorkspaceId) -> Option<SpaceId> {
        let snapshot = self.core_snapshot();
        let display = snapshot
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)?
            .display
            .clone();
        snapshot
            .displays
            .iter()
            .find(|candidate| candidate.id == display)
            .and_then(|candidate| candidate.space)
            .map(|space| SpaceId::new(space.0))
    }

    fn is_window_floating(&self, window: WindowId) -> bool {
        let window = Self::core_window_id(window);
        self.core_snapshot()
            .windows
            .iter()
            .find(|candidate| candidate.id == window)
            .is_some_and(|candidate| candidate.floating)
    }

    fn is_window_in_active_workspace(&self, space: SpaceId, window: WindowId) -> bool {
        self.active_workspace_for_space(space)
            .is_some_and(|workspace| self.workspace_for_window(window) == Some(workspace))
    }

    fn windows_in_active_workspace(&self, space: SpaceId) -> Vec<WindowId> {
        let Some(workspace) = self.active_workspace_for_space(space) else {
            return Vec::new();
        };
        let snapshot = self.core_snapshot();
        snapshot
            .windows
            .iter()
            .filter(|window| window.workspace == Some(workspace))
            .map(|window| Self::actor_window_id(window.id))
            .collect()
    }

    fn focused_window_for_command(&self) -> Option<WindowId> {
        self.core_snapshot().focused_window.map(Self::actor_window_id)
    }

    fn last_tiled_window_in_workspace(
        &self,
        workspace: crate::core::ids::WorkspaceId,
    ) -> Option<WindowId> {
        self.core_snapshot()
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|candidate| candidate.last_tiled_window)
            .map(Self::actor_window_id)
    }

    fn workspace_number(
        &self,
        workspace: crate::core::ids::WorkspaceId,
    ) -> Option<crate::core::ids::WorkspaceNumber> {
        self.core_snapshot()
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|candidate| candidate.number)
    }

    fn workspace_metadata(
        &self,
        workspace: crate::core::ids::WorkspaceId,
    ) -> Option<(u64, String)> {
        let snapshot = self.core_snapshot();
        let item = snapshot.workspaces.iter().find(|candidate| candidate.id == workspace)?;
        item.number?;
        let mut siblings = snapshot
            .workspaces
            .iter()
            .filter(|candidate| candidate.display == item.display && candidate.number.is_some())
            .collect::<Vec<_>>();
        siblings.sort_by_key(|candidate| candidate.number);
        let index = siblings.iter().position(|candidate| candidate.id == workspace)? as u64;
        Some((index, item.name.clone()))
    }

    fn move_core_window_to_space(
        &mut self,
        window: WindowId,
        space: SpaceId,
    ) -> Result<crate::core::state::Transition, crate::core::error::CoreError> {
        let display = self.display_uuid_for_space(space).ok_or_else(|| {
            crate::core::error::CoreError::IncompleteObservation(format!(
                "Space {space:?} has no display identity"
            ))
        })?;
        self.transition_core_command(crate::core::command::Command::Display(
            crate::core::command::DisplayCommand::MoveWindowTo {
                display: crate::core::ids::DisplayId(display),
                window: Some(Self::core_window_id(window)),
            },
        ))
    }

    fn advance_core_state(
        &mut self,
    ) -> Result<crate::core::snapshot::CoreSnapshot, crate::core::error::CoreError> {
        use crate::core::constraints::WindowConstraints;
        use crate::core::geometry::{Rect, Size};
        use crate::core::ids::{ApplicationId, DisplayId, Generation, SpaceId as CoreSpaceId};
        use crate::core::input::{
            DisplayObservation, Input, Observation, PlatformSnapshotObservation, WindowObservation,
        };

        let config = crate::interfaces::config::core_config(&self.config)?;
        let displays = self
            .space_manager
            .screens
            .iter()
            .map(|screen| {
                Ok(DisplayObservation {
                    id: DisplayId(screen.display_uuid.clone()),
                    frame: Rect::new(
                        screen.frame.origin.x,
                        screen.frame.origin.y,
                        screen.frame.size.width,
                        screen.frame.size.height,
                    )?,
                    space: screen.space.map(|space| CoreSpaceId(space.get())),
                })
            })
            .collect::<Result<Vec<_>, crate::core::geometry::GeometryError>>()
            .map_err(|error| {
                crate::core::error::CoreError::InvariantViolation(format!(
                    "invalid display geometry in core observation: {error}"
                ))
            })?;
        let active_space = self.workspace_command_space();
        let active_display = self
            .space_manager
            .screens
            .iter()
            .find(|screen| screen.space == active_space)
            .map(|screen| DisplayId(screen.display_uuid.clone()));
        let current = self.core_state.as_ref().map(|core| core.snapshot());
        let current_workspace_displays = current
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .map(|workspace| (workspace.id, workspace.display.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let current_window_displays = current
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .windows
                    .iter()
                    .filter_map(|window| {
                        let workspace = window.workspace?;
                        Some((window.id, current_workspace_displays.get(&workspace)?.clone()))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let fullscreen = self
            .space_manager
            .fullscreen_by_space
            .values()
            .flat_map(|track| track.windows.iter().filter_map(|window| window.window_id))
            .map(|window| crate::core::ids::WindowId::new(ApplicationId(window.pid), window.idx))
            .collect::<HashSet<_>>();
        let windows = self
            .window_manager
            .iter_windows()
            .filter(|(_, state)| {
                state.matches_filter(WindowFilter::EffectivelyManageable)
                    && state.info.sys_id.is_none_or(|window| {
                        self.window_manager
                            .get_window_server_info(window)
                            .is_none_or(|info| info.layer == 0)
                    })
            })
            .map(|(window, state)| {
                let id = crate::core::ids::WindowId::new(ApplicationId(window.pid), window.idx);
                let display = self
                    .space_manager
                    .screens
                    .iter()
                    .filter_map(|screen| {
                        let overlap = screen.frame.intersection(&state.frame_monotonic).area();
                        (overlap > 9.0).then_some((overlap, DisplayId(screen.display_uuid.clone())))
                    })
                    .max_by(|left, right| left.0.total_cmp(&right.0))
                    .map(|(_, display)| display)
                    .or_else(|| current_window_displays.get(&id).cloned());
                let app = self.app_manager.apps.get(&window.pid);
                Ok(WindowObservation {
                    id,
                    frame: Rect::new(
                        state.frame_monotonic.origin.x,
                        state.frame_monotonic.origin.y,
                        state.frame_monotonic.size.width,
                        state.frame_monotonic.size.height,
                    )?,
                    display,
                    platform_id: state.info.sys_id.map(|id| id.as_u32()),
                    app_id: app.and_then(|app| app.info.bundle_id.clone()),
                    app_name: app.and_then(|app| app.info.localized_name.clone()),
                    title: state.info.title.clone(),
                    ax_role: state.info.ax_role.clone(),
                    ax_subrole: state.info.ax_subrole.clone(),
                    minimized: state.info.is_minimized,
                    fullscreen: fullscreen.contains(&id),
                    constraints: WindowConstraints {
                        resizable: state.info.is_resizable,
                        preferred_size: Size {
                            width: state.frame_monotonic.size.width,
                            height: state.frame_monotonic.size.height,
                        },
                        min_size: state.info.min_size.map(|size| Size {
                            width: size.width,
                            height: size.height,
                        }),
                        max_size: state.info.max_size.map(|size| Size {
                            width: size.width,
                            height: size.height,
                        }),
                    },
                })
            })
            .collect::<Result<Vec<_>, crate::core::geometry::GeometryError>>()
            .map_err(|error| {
                crate::core::error::CoreError::InvariantViolation(format!(
                    "invalid window geometry in core observation: {error}"
                ))
            })?;
        let focused_window = self
            .main_window()
            .map(|window| crate::core::ids::WindowId::new(ApplicationId(window.pid), window.idx))
            .filter(|focused| windows.iter().any(|window| window.id == *focused));
        let core = self.core_state.as_mut().ok_or_else(|| {
            crate::core::error::CoreError::IncompleteObservation(
                "core state was not initialized".into(),
            )
        })?;
        if core.config() != &config {
            core.transition(Input::ConfigReloaded(config))?;
        }
        let transition = core.transition(Input::Observation(Observation::PlatformSnapshot(
            PlatformSnapshotObservation {
                generation: Generation(crate::sys::display_churn::epoch()),
                displays,
                active_display,
                windows,
                focused_window,
            },
        )))?;
        let mut snapshot = transition.snapshot.as_ref().clone();
        snapshot.applications = self.application_snapshots();
        Ok(snapshot)
    }

    fn prepare_core_topology_transition(&mut self) -> Result<(), crate::core::error::CoreError> {
        use crate::core::geometry::Rect;
        use crate::core::ids::{DisplayId, Generation, SpaceId as CoreSpaceId};
        use crate::core::input::{
            DisplayObservation, DisplayTopologyObservation, Input, Observation,
        };

        if self.core_state.is_none() {
            self.publish_core_snapshot()?;
        }
        let active_space = self.workspace_command_space();
        let displays = self
            .space_manager
            .screens
            .iter()
            .map(|screen| {
                Ok(DisplayObservation {
                    id: DisplayId(screen.display_uuid.clone()),
                    frame: Rect::new(
                        screen.frame.origin.x,
                        screen.frame.origin.y,
                        screen.frame.size.width,
                        screen.frame.size.height,
                    )?,
                    space: screen.space.map(|space| CoreSpaceId(space.get())),
                })
            })
            .collect::<Result<Vec<_>, crate::core::geometry::GeometryError>>()
            .map_err(|error| {
                crate::core::error::CoreError::InvariantViolation(format!(
                    "invalid display geometry in topology snapshot: {error}"
                ))
            })?;
        let active_display = self
            .space_manager
            .screens
            .iter()
            .find(|screen| screen.space == active_space)
            .map(|screen| DisplayId(screen.display_uuid.clone()));
        self.core_state
            .as_mut()
            .ok_or_else(|| {
                crate::core::error::CoreError::IncompleteObservation(
                    "cannot commit topology before the first core snapshot".into(),
                )
            })?
            .transition(Input::Observation(Observation::DisplayTopology(
                DisplayTopologyObservation {
                    generation: Generation(crate::sys::display_churn::epoch()),
                    displays,
                    active_display,
                },
            )))?;
        Ok(())
    }

    fn application_snapshots(&self) -> Vec<crate::core::snapshot::ApplicationSnapshot> {
        let frontmost_pid = self.main_window().map(|window| window.pid);
        let mut applications = self
            .app_manager
            .apps
            .iter()
            .map(|(&pid, app)| crate::core::snapshot::ApplicationSnapshot {
                id: crate::core::ids::ApplicationId(pid),
                bundle_id: app.info.bundle_id.clone(),
                name: app.info.localized_name.clone().unwrap_or_else(|| "Unknown".into()),
                frontmost: frontmost_pid == Some(pid),
                window_count: self.window_manager.window_ids_for_pid(pid).count(),
            })
            .collect::<Vec<_>>();
        applications.sort_by_key(|application| application.id);
        applications
    }

    fn build_core_snapshot(
        &self,
    ) -> Result<crate::core::snapshot::CoreSnapshot, crate::core::error::CoreError> {
        let mut snapshot = self
            .core_state
            .as_ref()
            .ok_or_else(|| {
                crate::core::error::CoreError::IncompleteObservation(
                    "core state was not initialized".into(),
                )
            })?
            .snapshot()
            .as_ref()
            .clone();
        snapshot.applications = self.application_snapshots();
        Ok(snapshot)
    }

    fn note_windowserver_activity(event: &Event) {
        let wsid = match event {
            Event::WindowFrameChanged(wid, ..) => Some(wid.idx.get()),
            Event::WindowCreated(wid, ..) => Some(wid.idx.get()),
            Event::WindowDestroyed(wid) => Some(wid.idx.get()),
            Event::WindowMinimized(wid) => Some(wid.idx.get()),
            Event::WindowDeminiaturized(wid) => Some(wid.idx.get()),
            Event::MouseDown(info, _) => info.map(|info| info.id.as_u32()),
            Event::MouseMoved(_) => None,
            Event::ResyncAppForWindow(wsid) => Some(wsid.as_u32()),
            Event::WindowServerDestroyed(wsid, _) => Some(wsid.as_u32()),
            Event::WindowServerAppeared(wsid, _) => Some(wsid.as_u32()),
            _ => None,
        };
        if let Some(wsid) = wsid {
            window_server::note_windowserver_activity(wsid);
        }
    }

    fn log_event(&self, event: &Event) {
        match event {
            Event::WindowFrameChanged(..)
            | Event::MouseDown(..)
            | Event::MouseUp
            | Event::MouseMoved(..) => {
                trace!(?event, "Event")
            }
            _ => debug!(?event, "Event"),
        }
    }

    fn is_interactive_resize_event(event: &Event) -> bool {
        matches!(
            event,
            Event::Command(Command::Layout(
                layout::LayoutCommand::ResizeWindowGrow
                    | layout::LayoutCommand::ResizeWindowShrink
                    | layout::LayoutCommand::ResizeWindowBy { .. }
                    | layout::LayoutCommand::ResizeWindowDirectional(_)
            ))
        )
    }

    fn should_suppress_layout_animation(event: &Event) -> bool {
        Self::is_interactive_resize_event(event)
            || matches!(
                event,
                Event::ApplicationLaunched { .. }
                    | Event::WindowsDiscovered { .. }
                    | Event::WindowCreated(..)
            )
    }

    fn should_update_notifications(event: &Event) -> bool {
        matches!(
            event,
            Event::WindowCreated(..)
                | Event::WindowDestroyed(..)
                | Event::WindowServerDestroyed(..)
                | Event::WindowServerAppeared(..)
                | Event::WindowsDiscovered { .. }
                | Event::ApplicationLaunched { .. }
                | Event::ApplicationTerminated(..)
                | Event::ApplicationThreadTerminated(..)
                | Event::SpaceChanged(..)
                | Event::ScreenParametersChanged(..)
        )
    }

    fn should_process_during_churn(event: &Event) -> bool {
        matches!(
            event,
            Event::DisplayChurnBegin
                | Event::DisplayChurnEnd
                | Event::ScreenParametersChanged(..)
                | Event::SpaceChanged(..)
                | Event::SpaceCreated(..)
                | Event::SpaceDestroyed(..)
                | Event::MissionControlNativeEntered
                | Event::MissionControlNativeExited
                | Event::SystemWoke
                | Event::SessionDidBecomeActive
                | Event::ApplicationLaunched { .. }
                | Event::ApplicationTerminated(..)
                | Event::ApplicationThreadTerminated(..)
                | Event::ApplicationActivated(..)
                | Event::ApplicationDeactivated(..)
                | Event::ApplicationGloballyActivated(..)
                | Event::ApplicationGloballyDeactivated(..)
                | Event::ApplicationMainWindowChanged(..)
                | Event::RegisterWmSender(..)
                | Event::ConfigUpdated(..)
                | Event::UserInput(..)
                | Event::Command(..)
                | Event::RaiseCompleted { .. }
                | Event::RaiseTimeout { .. }
                | Event::MenuOpened(..)
                | Event::MenuClosed(..)
        )
    }

    fn maybe_quarantine_during_churn(&mut self, event: &Event) -> bool {
        if !self.display_topology_manager.is_churning_or_awaiting_commit() {
            return false;
        }
        if Self::should_process_during_churn(event) {
            return false;
        }

        match event {
            Event::ResyncAppForWindow(..) => self.display_topology_manager.quarantine_resync(),
            Event::WindowServerDestroyed(..) => {
                self.display_topology_manager.quarantine_destroyed()
            }
            Event::WindowServerAppeared(..) => self.display_topology_manager.quarantine_appeared(),
            _ => {}
        }
        true
    }

    fn set_login_window_active(&mut self, active: bool) {
        self.space_activation_policy.set_login_window_active(active);
        self.recompute_and_set_active_spaces_from_current_screens();
    }

    fn handle_space_lifecycle(&mut self, space: SpaceId, created: bool) {
        if created {
            self.space_activation_policy.on_space_created(space);
        } else {
            self.space_activation_policy.on_space_destroyed(space);
        }
        self.recompute_and_set_active_spaces_from_current_screens();
    }

    #[instrument(name = "reactor::handle_event", skip(self), fields(event=?event))]
    fn handle_event(&mut self, event: Event) {
        self.log_event(&event);
        self.recording_manager.record.on_event(&event);
        let is_operation = matches!(&event, Event::Command(..));
        match &event {
            Event::UserInput(input) => {
                self.recording_manager.diagnostics.note_input(input.clone());
            }
            Event::Command(command) => {
                let before = self.core_snapshot();
                self.recording_manager.diagnostics.begin_operation(command, &before);
            }
            _ => {}
        }

        if let Some(suppress) = lifecycle_activation_suppression(&event) {
            self.workspace_switch_manager.suppress_auto_workspace_switch_until_input = suppress;
        }

        match event {
            Event::DisplayChurnBegin => {
                let mut pre_known_wsids: HashSet<WindowServerId> = HashSet::default();
                pre_known_wsids.extend(self.window_manager.iter_window_server_ids());

                let epoch = crate::sys::display_churn::epoch();
                let flags = crate::sys::display_churn::flags();
                self.display_topology_manager.begin_churn(epoch, flags, pre_known_wsids);
                return;
            }
            Event::DisplayChurnEnd => {
                let completed_flags = crate::sys::display_churn::completed_flags();
                let (epoch, _, flags) = self.display_topology_manager.current_churn().unwrap_or((
                    crate::sys::display_churn::epoch(),
                    std::time::Instant::now(),
                    completed_flags,
                ));
                let flags = flags | completed_flags;
                self.display_topology_manager.end_churn_to_awaiting(epoch, flags);
                return;
            }
            _ => {}
        }

        if self.maybe_quarantine_during_churn(&event) {
            trace!(?event, "quarantined event during display churn");
            return;
        }

        let should_update_notifications = Self::should_update_notifications(&event);

        let raised_window = self.main_window_tracker.handle_event(&event);
        // A newly managed window must land in its tiled frame immediately. Letting
        // it use the regular layout animation first exposes the app's default frame
        // for one animation interval and makes the neighboring tiles visibly jump.
        let mut suppress_layout_animation = Self::should_suppress_layout_animation(&event);
        let mut window_was_destroyed = false;

        match event {
            Event::ApplicationLaunched {
                pid,
                info,
                handle,
                visible_windows,
                window_server_info,
                is_frontmost,
                main_window,
            } => {
                AppEventHandler::handle_application_launched(
                    self,
                    pid,
                    info,
                    handle,
                    visible_windows,
                    window_server_info,
                    is_frontmost,
                    main_window,
                );
            }
            Event::ApplicationTerminated(pid) => {
                AppEventHandler::handle_application_terminated(self, pid);
            }
            Event::ApplicationThreadTerminated(pid) => {
                self.clear_menu_state_for_pid(pid);
                AppEventHandler::handle_application_thread_terminated(self, pid);
            }
            Event::ApplicationActivated(pid, quiet) => {
                self.clear_menu_state_for_non_owner(pid);
                if quiet == Quiet::No && self.move_main_window_to_auxiliary_click_workspace(pid) {
                    trace!(pid, "Moved auxiliary expansion on application activation");
                } else {
                    AppEventHandler::handle_application_activated(self, pid, quiet);
                }
            }
            Event::ApplicationDeactivated(pid) => {
                self.clear_menu_state_for_pid(pid);
            }
            Event::ApplicationGloballyDeactivated(pid) => {
                self.clear_menu_state_for_pid(pid);
                if self.is_login_window_pid(pid) {
                    self.set_login_window_active(false);
                }
            }
            Event::ResyncAppForWindow(wsid) => {
                AppEventHandler::handle_resync_app_for_window(self, wsid);
            }
            Event::ApplicationGloballyActivated(pid) => {
                self.clear_menu_state_for_non_owner(pid);
                if self.is_login_window_pid(pid) {
                    self.set_login_window_active(true);

                    let raw_spaces = self.raw_spaces_for_current_screens();
                    self.reconcile_spaces_with_display_history(&raw_spaces, false);

                    self.force_refresh_all_windows();
                } else {
                    if self.space_activation_policy.login_window_active {
                        // macOS sometimes activates loginwindow during wake without sending a
                        // corresponding deactivation. Any subsequent non-login activation
                        // indicates the user is back, so clear suppression.
                        self.set_login_window_active(false);
                    }
                    if let Some(app) = self.app_manager.apps.get(&pid) {
                        if let Err(e) = app.handle.send(Request::GetVisibleWindows) {
                            warn!(
                                "Failed to send GetVisibleWindows on global activation for app {}: {}",
                                pid, e
                            );
                        }
                    }
                    if self.move_main_window_to_auxiliary_click_workspace(pid) {
                        trace!(pid, "Moved auxiliary expansion on global activation");
                    } else if self.workspace_switch_manager.should_suppress_global_activation(pid) {
                        trace!(
                            pid,
                            "Skipping auto workspace switch for a Lift-initiated global activation"
                        );
                    } else {
                        self.handle_app_activation_workspace_switch(pid);
                    }
                }
            }
            Event::RegisterWmSender(sender) => {
                SystemEventHandler::handle_register_wm_sender(self, sender)
            }
            Event::WindowsDiscovered { pid, new, known_visible } => {
                AppEventHandler::handle_windows_discovered(self, pid, new, known_visible);
            }
            Event::WindowCreated(wid, window, ws_info, mouse_state) => {
                WindowEventHandler::handle_window_created(self, wid, window, ws_info, mouse_state);
            }
            Event::WindowDestroyed(wid) => {
                window_was_destroyed = WindowEventHandler::handle_window_destroyed(self, wid);
            }
            Event::WindowServerDestroyed(wsid, sid) => {
                SpaceEventHandler::handle_window_server_destroyed(self, wsid, sid);
            }
            Event::WindowServerAppeared(wsid, sid) => {
                SpaceEventHandler::handle_window_server_appeared(self, wsid, sid);
            }
            Event::SpaceCreated(space) => {
                self.handle_space_lifecycle(space, true);
            }
            Event::SpaceDestroyed(space) => {
                self.handle_space_lifecycle(space, false);
            }
            Event::WindowMinimized(wid) => {
                WindowEventHandler::handle_window_minimized(self, wid);
            }
            Event::WindowDeminiaturized(wid) => {
                WindowEventHandler::handle_window_deminiaturized(self, wid);
            }
            Event::WindowFrameChanged(wid, new_frame, last_seen, requested, mouse_state) => {
                suppress_layout_animation = WindowEventHandler::handle_window_frame_changed(
                    self,
                    wid,
                    new_frame,
                    last_seen,
                    requested,
                    mouse_state,
                );
            }
            Event::WindowTitleChanged(wid, new_title) => {
                WindowEventHandler::handle_window_title_changed(self, wid, new_title);
            }
            Event::ScreenParametersChanged(screens) => {
                SpaceEventHandler::handle_screen_parameters_changed(self, screens);
            }
            Event::SpaceChanged(spaces) => {
                SpaceEventHandler::handle_space_changed(self, spaces);
            }
            Event::MouseDown(info, point) => {
                self.capture_auxiliary_window_workspace_target(info, point);
            }
            Event::MouseUp => {
                DragEventHandler::handle_mouse_up(self);
                if let Some(wid) = self.window_id_under_cursor()
                    && self.best_space_for_window_id(wid).is_some()
                {
                    self.send_layout_event(LayoutEvent::WindowFocused(wid));
                }
            }
            Event::MenuOpened(pid) => SystemEventHandler::handle_menu_opened(self, pid),
            Event::MenuClosed(pid) => SystemEventHandler::handle_menu_closed(self, pid),
            Event::MouseMoved(point) => {
                if let Some(wsid) = window_server::get_window_at_point(point) {
                    window_server::note_windowserver_activity(wsid.as_u32());
                    if self.above_window != Some(wsid) {
                        self.above_window = Some(wsid);
                        WindowEventHandler::handle_mouse_moved_over_window(self, wsid);
                    }
                } else {
                    self.above_window = None;
                }
            }
            Event::SystemWoke => SystemEventHandler::handle_system_woke(self),
            Event::SessionDidBecomeActive => {}
            Event::MissionControlNativeEntered => {
                SpaceEventHandler::handle_mission_control_native_entered(self);
            }
            Event::MissionControlNativeExited => {
                SpaceEventHandler::handle_mission_control_native_exited(self);
            }
            Event::RaiseCompleted { window_id, sequence_id } => {
                SystemEventHandler::handle_raise_completed(self, window_id, sequence_id);
            }
            Event::RaiseTimeout { sequence_id } => {
                SystemEventHandler::handle_raise_timeout(self, sequence_id);
            }
            Event::ConfigUpdated(new_cfg) => {
                CommandEventHandler::handle_config_updated(self, new_cfg);
            }
            Event::UserInput(_) => {}
            Event::Command(cmd) => {
                CommandEventHandler::handle_command(self, cmd);
            }
            _ => (),
        }

        self.finalize_event_processing(
            raised_window,
            suppress_layout_animation,
            window_was_destroyed,
            should_update_notifications,
        );
        if is_operation {
            let after = self.core_snapshot();
            self.recording_manager.diagnostics.finish_operation(&after);
        }
    }

    fn finalize_event_processing(
        &mut self,
        raised_window: Option<WindowId>,
        suppress_layout_animation: bool,
        window_was_destroyed: bool,
        should_update_notifications: bool,
    ) {
        if self.display_topology_manager.is_churning_or_awaiting_commit() {
            return;
        }

        if let Some(raised_window) = raised_window {
            self.above_window = None;
            if self.best_space_for_window_id(raised_window).is_some() {
                self.send_layout_event(LayoutEvent::WindowFocused(raised_window));
            }
        }

        let mut layout_changed = false;
        if !self.is_in_drag() || window_was_destroyed {
            layout_changed = self.update_layout_or_warn(
                suppress_layout_animation,
                matches!(
                    self.workspace_switch_manager.workspace_switch_state,
                    WorkspaceSwitchState::Active
                ),
            );
            self.maybe_send_menu_update();
        }

        self.workspace_switch_manager.mark_workspace_switch_inactive();
        if self.workspace_switch_manager.active_workspace_switch.is_some() && !layout_changed {
            self.workspace_switch_manager.active_workspace_switch = None;
            trace!("Workspace switch stabilized with no further frame changes");
        }

        // Execute deferred mouse warp after workspace switch completes
        if let Some(wid) = self.workspace_switch_manager.pending_workspace_mouse_warp.take() {
            if let Some(window_center) = self.window_center_on_known_screen(wid) {
                self.warp_mouse(window_center);
            }
        }

        if should_update_notifications {
            let mut ids: Vec<u32> = self
                .window_manager
                .iter_tracked_window_server_ids()
                .map(|wsid| wsid.as_u32())
                .collect();
            ids.sort_unstable();

            if ids != self.notification_manager.last_sls_notification_ids {
                crate::sys::window_notify::update_window_notifications(&ids);

                self.notification_manager.last_sls_notification_ids = ids;
            }
        }
    }

    fn update_complete_window_server_info(&mut self, ws_info: Vec<WindowServerInfo>) {
        self.window_manager.clear_visible_windows();
        self.update_partial_window_server_info(ws_info);
    }

    fn update_partial_window_server_info(&mut self, ws_info: Vec<WindowServerInfo>) {
        // Mark visible windows and remove any corresponding observed WSID markers
        // for ids we now have server info for.
        self.window_manager.set_visible_windows(ws_info.iter().map(|info| info.id));
        for info in ws_info.iter() {
            // If we've been observing this server id from SLS callbacks, clear it.
            self.window_manager.clear_window_server_observed(info.id);
            self.window_manager.track_window_server_info(*info);

            if let Some(wid) = self.window_manager.tracked_window_id(info.id) {
                let (server_id, is_minimized, is_ax_standard, is_ax_root) =
                    if let Some(window) = self.window_manager.window_mut(wid) {
                        if info.layer == 0 {
                            window.frame_monotonic = info.frame;
                        }
                        (
                            window.info.sys_id,
                            window.info.is_minimized,
                            window.info.is_standard,
                            window.info.is_root,
                        )
                    } else {
                        continue;
                    };
                let manageable = utils::compute_window_manageability(
                    server_id,
                    is_minimized,
                    is_ax_standard,
                    is_ax_root,
                    |wsid| self.window_manager.get_window_server_info(wsid),
                );
                if let Some(window) = self.window_manager.window_mut(wid) {
                    window.is_manageable = manageable;
                }
            }
        }
    }

    fn check_for_new_windows(&mut self) {
        // TODO: Do this correctly/more optimally using CGWindowListCopyWindowInfo
        // (see notes for on_windows_discovered below).
        self.request_visible_windows_for_apps(false);
    }

    fn request_visible_windows_for_apps(&mut self, track_mission_control_refresh: bool) {
        let mut refreshed_pids = Vec::new();
        for (&pid, app) in &self.app_manager.apps {
            // Errors mean the app terminated (and a termination event is coming); ignore.
            if app.handle.send(Request::GetVisibleWindows).is_ok() {
                refreshed_pids.push(pid);
            }
        }

        if track_mission_control_refresh {
            self.mission_control_manager
                .pending_mission_control_refresh
                .extend(refreshed_pids);
        }
    }

    fn handle_fullscreen_space_transition(&mut self, spaces: &mut Vec<Option<SpaceId>>) -> bool {
        self.preserve_user_spaces_during_fullscreen_transition(spaces);

        let mut saw_fullscreen = false;
        let mut all_fullscreen = !spaces.is_empty();
        let mut refresh_spaces = Vec::new();

        for slot in spaces.iter_mut() {
            match slot {
                Some(space) if self.is_fullscreen_space(*space) => {
                    saw_fullscreen = true;
                    *slot = None;
                }
                Some(space) => {
                    all_fullscreen = false;
                    refresh_spaces.push(*space);
                }
                None => {
                    all_fullscreen = false;
                }
            }
        }

        if saw_fullscreen && all_fullscreen {
            return true;
        }

        for space in refresh_spaces {
            let mut tracks = Vec::new();
            if let Some(track) = self.space_manager.fullscreen_by_space.remove(&space.get()) {
                tracks.push(track);
            }

            let keys_to_remove: Vec<u64> = self
                .space_manager
                .fullscreen_by_space
                .iter()
                .filter(|(_, track)| {
                    track.windows.iter().any(|w| w.last_known_user_space == Some(space))
                })
                .map(|(&key, _)| key)
                .collect();

            for key in keys_to_remove {
                if let Some(track) = self.space_manager.fullscreen_by_space.remove(&key) {
                    tracks.push(track);
                }
            }

            for track in tracks {
                wait_for_native_fullscreen_transition();
                thread::sleep(Duration::from_millis(50));

                for window in track.windows {
                    if let Some(app) = self.app_manager.apps.get(&window.pid) {
                        if let Err(e) = app.handle.send(Request::GetVisibleWindows) {
                            warn!("Failed to send GetVisibleWindows to app {}: {}", window.pid, e);
                        }
                    }

                    if let (Some(window_id), Some(target_space)) =
                        (window.window_id, window.last_known_user_space)
                    {
                        if let Some(source_space) = self
                            .best_space_for_window_id(window_id)
                            .or(window.last_known_user_space)
                        {
                            if source_space != target_space {
                                if let Err(error) =
                                    self.move_core_window_to_space(window_id, target_space)
                                {
                                    warn!(?error, ?window_id, "Core rejected fullscreen restore");
                                }
                            }
                        }
                    }
                }

                self.refocus_manager.refocus_state = RefocusState::Pending(space);
                self.update_layout_or_warn(false, false);
                self.update_focus_follows_mouse_state();
            }
        }

        false
    }

    fn is_fullscreen_space(&self, space: SpaceId) -> bool {
        space_is_fullscreen(space.get())
            || self.space_manager.fullscreen_by_space.contains_key(&space.get())
    }

    pub(crate) fn has_fullscreen_windows_for_spaces(&self, spaces: &[Option<SpaceId>]) -> bool {
        spaces.iter().flatten().any(|space| {
            self.space_manager
                .fullscreen_by_space
                .values()
                .any(|track| track.windows.iter().any(|w| w.last_known_user_space == Some(*space)))
        })
    }

    fn preserve_user_spaces_during_fullscreen_transition(&self, spaces: &mut [Option<SpaceId>]) {
        let entering_fullscreen =
            self.space_manager.screens.iter().zip(spaces.iter()).any(|(screen, slot)| {
                let Some(new_space) = *slot else {
                    return false;
                };
                if !self.is_fullscreen_space(new_space) {
                    return false;
                }
                screen
                    .space
                    .is_some_and(|previous_space| !self.is_fullscreen_space(previous_space))
            });
        if !entering_fullscreen {
            return;
        }

        for (screen, slot) in self.space_manager.screens.iter().zip(spaces.iter_mut()) {
            let Some(new_space) = *slot else {
                continue;
            };
            if self.is_fullscreen_space(new_space) {
                continue;
            }
            let Some(previous_space) = screen.space else {
                continue;
            };
            if previous_space == new_space || self.is_fullscreen_space(previous_space) {
                continue;
            }

            debug!(
                display_uuid = %screen.display_uuid,
                ?previous_space,
                ?new_space,
                "Preserving previous user space during fullscreen transition"
            );
            *slot = Some(previous_space);
        }
    }

    fn set_screen_spaces(&mut self, spaces: &[Option<SpaceId>]) {
        for (space, screen) in spaces.iter().copied().zip(&mut self.space_manager.screens) {
            screen.space = space;
        }
    }

    fn reconcile_spaces_with_display_history(
        &mut self,
        spaces: &[Option<SpaceId>],
        allow_remap: bool,
    ) {
        let _ = (spaces, allow_remap);
    }

    fn finalize_space_change(
        &mut self,
        spaces: &[Option<SpaceId>],
        ws_info: Vec<WindowServerInfo>,
    ) {
        self.refocus_manager.stale_cleanup_state = if spaces.iter().all(|space| space.is_none()) {
            StaleCleanupState::Suppressed
        } else {
            StaleCleanupState::Enabled
        };
        self.expose_all_spaces();
        if let Some(main_window) = self.main_window() {
            if self.main_window_space().is_some() {
                self.send_layout_event(LayoutEvent::WindowFocused(main_window));
            }
        }
        let ws_info = self.filter_ws_info_to_active_spaces(ws_info);
        self.update_complete_window_server_info(ws_info);
        // A topology completion has one refresh owner: the topology commit,
        // or the pending-relayout fallback when no commit snapshot exists.
        // Ordinary finalization must not issue a duplicate app refresh.
        if !self.pending_space_change_manager.topology_relayout_pending {
            self.check_for_new_windows();
        }

        if let Some(space) =
            spaces.iter().copied().flatten().find(|space| self.is_space_active(*space))
            && let Some(workspace_id) = self.active_workspace_for_space(space)
            && let Some((_, workspace_name)) = self.workspace_metadata(workspace_id)
        {
            let display_uuid = self.display_uuid_for_space(space);
            let broadcast_event = BroadcastEvent::WorkspaceChanged {
                workspace_id,
                workspace_name,
                space_id: space,
                display_uuid,
            };
            _ = self.communication_manager.event_broadcaster.send(broadcast_event);
        }
    }

    fn broadcast_window_title_changed(
        &mut self,
        window_id: WindowId,
        previous_title: String,
        new_title: String,
    ) {
        if previous_title != new_title
            && let Some(space) = self.best_space_for_window_id(window_id)
            && self.is_space_active(space)
            && let Some(workspace_id) = self.active_workspace_for_space(space)
        {
            let (workspace_index, workspace_name) = self
                .workspace_metadata(workspace_id)
                .unwrap_or_else(|| (0, format!("Workspace {workspace_id:?}")));

            let display_uuid = self.display_uuid_for_space(space);

            let event = BroadcastEvent::WindowTitleChanged {
                window_id,
                workspace_id,
                workspace_index: Some(workspace_index),
                workspace_name,
                previous_title,
                new_title,
                space_id: space,
                display_uuid,
            };
            let _ = self.communication_manager.event_broadcaster.send(event);
        }
    }

    fn maybe_reapply_app_rules_for_window(&mut self, window_id: WindowId) {
        if !self.config.virtual_workspaces.reapply_app_rules_on_title_change {
            return;
        }

        let Some(space) = self.intended_space_for_window_id(window_id) else {
            return;
        };
        if !self.is_space_active(space) {
            return;
        }

        let (is_manageable, wsid) = match self.window_manager.window(window_id) {
            Some(window_state) => (
                window_state.matches_filter(WindowFilter::Manageable),
                window_state.info.sys_id,
            ),
            None => return,
        };

        if !is_manageable {
            return;
        }

        let app_info = match self.app_manager.apps.get(&window_id.pid) {
            Some(app_state) => app_state.info.clone(),
            None => return,
        };

        if let Some(window_server_id) = wsid {
            self.window_manager.mark_wsids_recent(std::iter::once(window_server_id));
        }

        self.process_windows_for_app_rules(window_id.pid, vec![window_id], app_info);
    }

    fn try_apply_pending_space_change(&mut self) {
        if let Some(mut pending) = self.pending_space_change_manager.pending_space_change.take() {
            if pending.spaces.len() == self.space_manager.screens.len() {
                if self.handle_fullscreen_space_transition(&mut pending.spaces) {
                    return;
                }
                // A pending space change is queued specifically when Mission Control is active.
                // When we apply it later, we must also recompute active spaces (normally done in
                // the regular SpaceChanged handler) to avoid staying "space-less" until the next
                // user-initiated space switch.
                self.recompute_and_set_active_spaces(&pending.spaces);
                self.set_screen_spaces(&pending.spaces);
                let ws_info = self.authoritative_window_snapshot_for_active_spaces();
                self.finalize_space_change(&pending.spaces, ws_info);
            } else {
                self.pending_space_change_manager.pending_space_change = Some(pending);
            }
        }
    }

    fn repair_spaces_after_mission_control(&mut self) {
        // First, apply any SpaceChanged that arrived while MC was active.
        self.try_apply_pending_space_change();

        // If we still have missing space ids (or no active spaces), proactively rebuild
        // per-display current spaces via CGS. This covers the common case where macOS emits
        // a transient "all None" spaces vector during Mission Control and then doesn't emit
        // a corresponding steady-state update when exiting back to the same space.
        let needs_repair = self.active_spaces.is_empty()
            || self.space_manager.screens.iter().all(|s| s.space.is_none());
        if !needs_repair || self.space_manager.screens.is_empty() {
            return;
        }

        let spaces: Vec<Option<SpaceId>> = self
            .space_manager
            .screens
            .iter()
            .map(|s| {
                crate::sys::screen::current_space_for_display_uuid(&s.display_uuid).or(s.space)
            })
            .collect();

        if spaces.iter().any(|s| s.is_some()) && spaces.len() == self.space_manager.screens.len() {
            self.set_screen_spaces(&spaces);
            self.recompute_and_set_active_spaces(&spaces);
        }
    }

    fn on_windows_discovered_with_app_info(
        &mut self,
        pid: pid_t,
        new: Vec<(WindowId, WindowInfo)>,
        known_visible: Vec<WindowId>,
        app_info: Option<AppInfo>,
    ) {
        WindowDiscoveryHandler::handle_discovery(self, pid, new, known_visible, app_info);
    }

    fn best_space_for_window(
        &self,
        frame: &CGRect,
        window_server_id: Option<WindowServerId>,
    ) -> Option<SpaceId> {
        if let Some(space) = window_server_id.and_then(crate::sys::window_server::window_space) {
            // Return None for windows whose resolved space is not a user space (e.g. native
            // fullscreen app spaces, SLSSpaceGetType != 0). Without this guard, fullscreen
            // windows fall through to best_space_for_frame which matches by geometry — and
            // fullscreen windows cover the whole screen, so they match the current user space
            // and bleed into its tile layout after Mission Control (fixes #357).
            if !crate::sys::window_server::space_is_user(space.get()) {
                return None;
            }
            if self.space_manager.screen_by_space(space).is_some() {
                return Some(space);
            }
        }

        if let Some(space) = self.best_space_for_frame(frame) {
            return Some(space);
        }

        None
    }

    fn best_space_for_frame(&self, frame: &CGRect) -> Option<SpaceId> {
        let center = frame.mid();
        self.screen_for_point(center).and_then(|screen| screen.space).or_else(|| {
            self.space_manager
                .screens
                .iter()
                .filter_map(|screen| {
                    let space = screen.space?;
                    let area = screen.frame.intersection(frame).area() as i64;
                    if area > 0 { Some((area, space)) } else { None }
                })
                .max_by_key(|(area, _)| *area)
                .map(|(_, space)| space)
        })
    }

    fn ensure_active_drag(&mut self, wid: WindowId, frame: &CGRect) {
        let needs_new_session =
            self.get_active_drag_session().map_or(true, |session| session.window != wid);
        if needs_new_session {
            let server_id = self.window_manager.window(wid).and_then(|window| window.info.sys_id);
            let origin_space = self.best_space_for_window(frame, server_id);
            let session = DragSession {
                window: wid,
                last_frame: *frame,
                origin_space,
                settled_space: origin_space,
                layout_dirty: false,
            };
            self.drag_manager.drag_state = DragState::Active { session };
        }
        self.drag_manager.skip_layout_for_window = Some(wid);
    }

    fn update_active_drag(&mut self, wid: WindowId, new_frame: &CGRect) {
        let resolved_space = match self.get_active_drag_session() {
            Some(session) if session.window == wid => self.resolve_drag_space(session, new_frame),
            _ => return,
        };

        if let Some(session) = self.get_active_drag_session_mut() {
            let frame_changed = session.last_frame != *new_frame;
            session.last_frame = *new_frame;
            if frame_changed {
                session.layout_dirty = true;
            }
            if session.settled_space != resolved_space {
                session.settled_space = resolved_space;
                session.layout_dirty = true;
                self.drag_manager.skip_layout_for_window = Some(session.window);
            }
        }
    }

    fn drag_space_candidate(&self, frame: &CGRect) -> Option<SpaceId> {
        let center = frame.mid();
        self.screen_for_point(center).and_then(|screen| screen.space)
    }

    fn resolve_drag_space(&self, session: &DragSession, frame: &CGRect) -> Option<SpaceId> {
        let server_id =
            self.window_manager.window(session.window).and_then(|window| window.info.sys_id);
        if frame.area() <= 0.0 {
            return session.settled_space.or_else(|| self.best_space_for_window(frame, server_id));
        }

        self.drag_space_candidate(frame)
            .or_else(|| self.best_space_for_window(frame, server_id))
            .or(session.settled_space)
    }

    fn best_space_for_window_state(&self, window: &WindowState) -> Option<SpaceId> {
        self.best_space_for_window(&window.frame_monotonic, window.info.sys_id)
    }

    fn best_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.window_manager
            .window(wid)
            .and_then(|window| self.best_space_for_window_state(window))
    }

    fn assigned_hidden_workspace_space(&self, wid: WindowId) -> Option<SpaceId> {
        let workspace_id = self.workspace_for_window(wid)?;
        let space = self.space_for_workspace(workspace_id)?;
        if !self.is_space_active(space) {
            return None;
        }
        (self.active_workspace_for_space(space) != Some(workspace_id)).then_some(space)
    }

    pub(crate) fn recent_workspace_target_for(
        &self,
        wid: WindowId,
    ) -> Option<managers::RecentWorkspaceTarget> {
        let target = *self.refocus_manager.recent_workspace_targets.get(&wid)?;
        if Instant::now() > target.expires_at {
            return None;
        }

        let window = self.window_manager.window(wid)?;
        if !window.matches_filter(WindowFilter::EffectivelyManageable) {
            return None;
        }
        if self.space_for_workspace(target.workspace_id) != Some(target.space) {
            return None;
        }
        if !self.is_space_active(target.space) {
            return None;
        }

        Some(target)
    }

    pub(crate) fn intended_space_for_window_state(
        &self,
        wid: WindowId,
        window: &WindowState,
    ) -> Option<SpaceId> {
        self.focus_next_window_target_for(wid)
            .map(|target| target.space)
            .or_else(|| self.recent_workspace_target_for(wid).map(|target| target.space))
            .or_else(|| self.assigned_hidden_workspace_space(wid))
            .or_else(|| self.best_space_for_window_state(window))
    }

    pub(crate) fn intended_space_for_window_id(&self, wid: WindowId) -> Option<SpaceId> {
        self.window_manager
            .window(wid)
            .and_then(|window| self.intended_space_for_window_state(wid, window))
    }

    pub(crate) fn remember_recent_workspace_target(&mut self, wid: WindowId) {
        let target = self.workspace_for_window(wid).and_then(|workspace_id| {
            self.space_for_workspace(workspace_id).map(|space| (space, workspace_id))
        });
        let Some((space, workspace_id)) = target else {
            return;
        };
        let _ = self.remember_recent_workspace_target_for(wid, space, workspace_id);
    }

    pub(crate) fn remember_recent_workspace_target_for_slot(
        &mut self,
        wid: WindowId,
        workspace: usize,
    ) -> bool {
        let target = u8::try_from(workspace)
            .ok()
            .and_then(|number| number.checked_add(1))
            .and_then(|number| crate::core::ids::WorkspaceNumber::try_from(number).ok())
            .and_then(|number| {
                let snapshot = self.core_snapshot();
                snapshot
                    .workspaces
                    .iter()
                    .find(|candidate| candidate.number == Some(number))
                    .and_then(|candidate| {
                        self.space_for_workspace(candidate.id).map(|space| (space, candidate.id))
                    })
            });
        let Some((space, workspace_id)) = target else {
            return false;
        };
        self.remember_recent_workspace_target_for(wid, space, workspace_id)
    }

    fn remember_recent_workspace_target_for(
        &mut self,
        wid: WindowId,
        space: SpaceId,
        workspace_id: crate::core::ids::WorkspaceId,
    ) -> bool {
        if !self.is_space_active(space) {
            return false;
        }

        self.refocus_manager.recent_workspace_targets.insert(
            wid,
            managers::RecentWorkspaceTarget {
                space,
                workspace_id,
                expires_at: Instant::now() + RECENT_WORKSPACE_TARGET_TIMEOUT,
            },
        );
        true
    }

    fn finalize_active_drag(&mut self) -> bool {
        let Some(session) = self.take_active_drag_session() else {
            return false;
        };
        let wid = session.window;

        // During a drag the window server can continue reporting the origin
        // space even after the user has moved the window onto another display.
        // Trust the drag session’s resolved space (or the final frame’s screen)
        // before falling back to the server-reported space so that cross-display
        // drags do not snap the window back to the original monitor.
        let final_space = session
            .settled_space
            .or_else(|| self.best_space_for_frame(&session.last_frame))
            .or_else(|| self.best_space_for_window_id(wid));

        let needs_layout = if session.origin_space != final_space {
            if session.origin_space.is_some() {
                self.send_layout_event(LayoutEvent::Changed);
            }
            if let Some(space) = final_space {
                if let Some(active_ws) = self.active_workspace_for_space(space) {
                    let assigned = match self.move_core_window_to_space(wid, space) {
                        Ok(_) => true,
                        Err(error) => {
                            warn!(?error, ?wid, "Core rejected cross-display drag assignment");
                            false
                        }
                    };
                    if !assigned {
                        warn!("Failed to assign window {:?} to workspace {:?}", wid, active_ws);
                    } else {
                        let _ = self.remember_recent_workspace_target_for(wid, space, active_ws);
                    }
                }
                self.send_layout_event(LayoutEvent::WindowAdded(space, wid));
            }
            self.drag_manager.skip_layout_for_window = Some(wid);
            true
        } else if session.layout_dirty {
            self.drag_manager.skip_layout_for_window = Some(wid);
            true
        } else {
            false
        };

        if let Err(error) = self.advance_core_state() {
            debug!(?error, ?wid, "Core deferred final drag observation");
        }

        needs_layout
    }

    fn window_center_on_known_screen(&self, wid: WindowId) -> Option<CGPoint> {
        let window_center = self.window_manager.window(wid)?.frame_monotonic.mid();
        self.screen_for_point(window_center).map(|_| window_center)
    }

    fn has_visible_window_server_ids_for_pid(&self, pid: pid_t) -> bool {
        self.window_manager.iter_visible_window_server_ids().any(|wsid| {
            self.window_manager.tracked_window_id(wsid).is_some_and(|wid| wid.pid == pid)
        })
    }

    pub fn warp_mouse(&mut self, point: CGPoint) {
        if let Some(event_tap_tx) = self.communication_manager.event_tap_tx.as_ref() {
            self.above_window = None;
            _ = event_tap_tx.send(crate::actor::event_tap::Request::Warp(point));
        }
    }

    fn warp_mouse_to_space_center(&mut self, space: SpaceId) -> bool {
        let Some(screen) = self.space_manager.screen_by_space(space) else {
            return false;
        };
        self.warp_mouse(screen.frame.mid());
        true
    }

    fn try_focus_or_warp_without_raise(
        &mut self,
        warp_space: Option<SpaceId>,
        focus_window: &mut Option<WindowId>,
    ) -> bool {
        if let Some(wid) = self.window_id_under_cursor() {
            *focus_window = Some(wid);
            return false;
        }
        if self.focus_untracked_window_under_cursor() {
            return true;
        }
        self.config.settings.mouse_follows_focus
            && warp_space.is_some_and(|space| self.warp_mouse_to_space_center(space))
    }

    fn insert_app_handle_for_window(
        &self,
        app_handles: &mut HashMap<pid_t, AppThreadHandle>,
        wid: WindowId,
    ) {
        if let Some(app) = self.app_manager.apps.get(&wid.pid) {
            app_handles.insert(wid.pid, app.handle.clone());
        }
    }

    fn expose_all_spaces(&mut self) {
        let spaces: Vec<SpaceId> = self
            .space_manager
            .screens
            .iter()
            .filter_map(|screen| screen.space)
            .filter(|space| self.is_space_active(*space))
            .collect();
        for space in spaces {
            self.expose_space_if_known(space);
        }
    }

    fn window_is_standard(&self, id: WindowId) -> bool {
        self.window_manager
            .window(id)
            .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
    }

    fn send_layout_event(&mut self, event: LayoutEvent) {
        if let LayoutEvent::WindowFocused(window) = &event {
            let input = crate::core::input::Input::Observation(
                crate::core::input::Observation::FocusChanged {
                    window: Some(Self::core_window_id(*window)),
                },
            );
            if let Err(error) = self.transition_core_input(input) {
                debug!(?error, ?window, "Core rejected focus observation");
            }
        }
        if let Err(error) = self.advance_core_state() {
            debug!(?error, "Core deferred a lifecycle observation");
        }
        self.prepare_refocus_after_layout_event(&event);
        self.handle_layout_response(layout::EventResponse::default(), None);
    }

    // Returns true if the window should be raised on mouse over considering
    // active workspace membership and potential occlusion of floating windows above it.
    fn should_raise_on_mouse_over(&self, wid: WindowId) -> bool {
        let Some(window) = self.window_manager.window(wid) else {
            return false;
        };

        if !window.matches_filter(WindowFilter::EffectivelyManageable)
            && !self.is_window_floating(wid)
        {
            return false;
        }

        let candidate_frame = window.frame_monotonic;

        if matches!(self.menu_manager.menu_state, MenuState::Open(_)) {
            trace!(?wid, "Skipping autoraise while menu open");
            return false;
        }

        let Some(space) = self.best_space_for_window(&candidate_frame, window.info.sys_id) else {
            return false;
        };
        if !self.is_space_active(space) {
            return false;
        }

        if !self.is_window_in_active_workspace(space, wid) {
            trace!("Ignoring mouse over window {:?} - not in active workspace", wid);
            return false;
        }

        let Some(candidate_wsid) = window.info.sys_id else {
            return true;
        };

        let order = {
            let space_id = space.get();
            crate::sys::window_server::space_window_list_for_connection(&[space_id], 0, false)
        };
        let candidate_u32 = candidate_wsid.as_u32();
        let candidate_level = window_level(candidate_u32);
        let candidate_sub_level = window_sub_level(candidate_u32);

        for above_u32 in order {
            if above_u32 == candidate_u32 {
                break;
            }

            let above_wsid = WindowServerId::new(above_u32);
            let Some(above_wid) = self.window_manager.tracked_window_id(above_wsid) else {
                continue;
            };

            if !self.is_window_floating(above_wid) {
                continue;
            }

            let Some(above_state) = self.window_manager.window(above_wid) else {
                continue;
            };
            let above_frame = above_state.frame_monotonic;
            if !candidate_frame.contains_rect(above_frame) {
                continue;
            }

            let above_level = window_level(above_u32);
            let above_sub_level = window_sub_level(above_u32);
            if candidate_level
                .zip(above_level)
                .is_some_and(|(candidate, above)| candidate == above)
                && candidate_sub_level == above_sub_level
            {
                return false;
            }
        }

        true
    }

    fn process_windows_for_app_rules(
        &mut self,
        _pid: pid_t,
        window_ids: Vec<WindowId>,
        app_info: AppInfo,
    ) {
        let mut focus_windows = Vec::new();
        for &window in &window_ids {
            let decision = self.window_rule_decision(window, Some(&app_info));
            if self.workspace_for_window(window).is_none()
                && matches!(&decision, RuleDecision::Managed { focus: true, .. })
            {
                focus_windows.push(window);
            }
            if let Some(state) = self.window_manager.window_mut(window) {
                state.ignore_app_rule = matches!(decision, RuleDecision::Unmanaged { .. });
            }
        }
        if let Err(error) = self.advance_core_state() {
            warn!(?error, "Core deferred window-rule reconciliation");
            return;
        }
        for window in window_ids {
            let Some(target) = self.recent_workspace_target_for(window) else {
                continue;
            };
            let Some(workspace) = self.workspace_number(target.workspace_id) else {
                continue;
            };
            let command = crate::core::command::WorkspaceCommand::MoveWindow {
                workspace,
                window: Some(Self::core_window_id(window)),
            };
            if let Err(error) =
                self.transition_core_command(crate::core::command::Command::Workspace(command))
            {
                warn!(?error, ?window, "Core rejected a pending workspace target");
            }
        }
        for window in focus_windows {
            self.focus_new_rule_window(window);
        }
    }

    fn focus_new_rule_window(&mut self, window: WindowId) {
        let Some(window_space) = self
            .window_manager
            .window(window)
            .and_then(|state| self.intended_space_for_window_state(window, state))
        else {
            return;
        };
        let Some(target_workspace) = self.workspace_for_window(window) else {
            return;
        };
        let Some(active_workspace) = self.active_workspace_for_space(window_space) else {
            return;
        };
        let mut response = layout::EventResponse::default();
        if target_workspace != active_workspace {
            let Some(number) = self.workspace_number(target_workspace) else {
                return;
            };
            self.store_current_floating_positions(window_space);
            self.workspace_switch_manager
                .start_workspace_switch(WorkspaceSwitchOrigin::Auto);
            let transition =
                match self.transition_core_command(crate::core::command::Command::Workspace(
                    crate::core::command::WorkspaceCommand::Activate(number),
                )) {
                    Ok(transition) => transition,
                    Err(error) => {
                        warn!(
                            ?error,
                            ?window,
                            "Core rejected rule-requested workspace activation"
                        );
                        return;
                    }
                };
            response = self.layout_response_for_transition(&transition);
        }
        response.focus_window = Some(window);
        if !response.raise_windows.contains(&window) {
            response.raise_windows.push(window);
        }
        self.handle_layout_response(response, Some(window_space));
    }

    fn handle_app_activation_workspace_switch(&mut self, pid: pid_t) {
        self.handle_app_activation_workspace_switch_for_window(pid, None);
    }

    fn handle_app_activation_workspace_switch_for_window(
        &mut self,
        pid: pid_t,
        preferred_window: Option<WindowId>,
    ) {
        if self.workspace_switch_manager.suppress_auto_workspace_switch_until_input {
            debug!(
                pid,
                "Skipping automatic workspace switch for lifecycle-restored activation before user input"
            );
            return;
        }

        if self.workspace_switch_manager.active_workspace_switch.is_some() {
            trace!(
                "Skipping auto workspace switch for pid {} because a workspace switch is in progress",
                pid
            );
            return;
        }

        if self.workspace_switch_manager.manual_switch_in_progress() {
            debug!(
                "Skipping auto workspace switch for pid {} because a manual switch is in progress",
                pid
            );
            return;
        }

        if let Some(active_space) = get_active_space_number()
            && space_is_fullscreen(active_space.get())
        {
            debug!(
                "Skipping auto workspace switch for pid {} because the active space is fullscreen",
                pid
            );
            return;
        }

        if let Some(wsid) = self.activation_from_unmanageable_window(pid) {
            if !self.has_manageable_window_on_inactive_workspace(pid) {
                debug!(
                    ?wsid,
                    "Skipping auto workspace switch for pid {} because the activated window is not manageable",
                    pid
                );
                return;
            }
            debug!(
                ?wsid,
                pid, "Unmanageable controller activated; restoring the app's manageable window"
            );
        }

        let visible_spaces: HashSet<SpaceId> = self.iter_active_spaces().collect();
        let app_is_on_visible_workspace = self
            .window_manager
            .iter_windows()
            .filter(|(wid, _)| {
                if wid.pid != pid {
                    return false;
                }
                preferred_window.is_none_or(|preferred| *wid == preferred)
            })
            .any(|(wid, window_state)| {
                let Some(space) = self.intended_space_for_window_state(wid, window_state) else {
                    return false;
                };
                if !visible_spaces.contains(&space) {
                    return false;
                }
                let Some(active_workspace) = self.active_workspace_for_space(space) else {
                    return false;
                };
                self.workspace_for_window(wid)
                    .is_some_and(|window_workspace| window_workspace == active_workspace)
            });

        if app_is_on_visible_workspace {
            debug!("App {} is already on a visible workspace, not switching.", pid);
            return;
        }

        let Some(bundle_id) =
            self.app_manager.apps.get(&pid).and_then(|app| app.info.bundle_id.as_deref())
        else {
            return;
        };

        if self
            .config
            .settings
            .auto_focus_blacklist
            .iter()
            .any(|blocked| blocked == bundle_id)
        {
            debug!(
                "App {} is blacklisted for auto-focus workspace switching, ignoring activation",
                bundle_id
            );
            return;
        }

        debug!(
            "App activation detected: {} (pid: {}), checking for workspace switch",
            bundle_id, pid
        );

        let app_window = preferred_window
            .filter(|wid| wid.pid == pid && self.window_is_standard(*wid))
            .or_else(|| {
                self.main_window().filter(|wid| wid.pid == pid && self.window_is_standard(*wid))
            })
            .or_else(|| {
                self.window_manager
                    .window_ids_for_pid(pid)
                    .find(|wid| self.window_is_standard(*wid))
            });

        let Some(app_window_id) = app_window else {
            return;
        };

        let Some(window_state) = self.window_manager.window(app_window_id) else {
            return;
        };
        let Some(window_space) = self.intended_space_for_window_state(app_window_id, window_state)
        else {
            return;
        };

        self.maybe_auto_switch_to_window_workspace(pid, app_window_id, window_space);
    }

    fn maybe_auto_switch_to_window_workspace(
        &mut self,
        pid: pid_t,
        app_window_id: WindowId,
        window_space: SpaceId,
    ) {
        let Some(window_workspace) = self.workspace_for_window(app_window_id) else {
            return;
        };

        let Some(current_workspace) = self.active_workspace_for_space(window_space) else {
            return;
        };

        if window_workspace != current_workspace {
            if let Some((workspace_index, _)) = self.workspace_metadata(window_workspace)
                && let Some(number) = self.workspace_number(window_workspace)
            {
                debug!(
                    "Auto-switching to workspace {} for activated app (pid: {})",
                    workspace_index, pid
                );

                self.store_current_floating_positions(window_space);
                self.workspace_switch_manager
                    .start_workspace_switch(WorkspaceSwitchOrigin::Auto);

                let transition =
                    self.transition_core_command(crate::core::command::Command::Workspace(
                        crate::core::command::WorkspaceCommand::Activate(number),
                    ));
                let mut response = match transition {
                    Ok(transition) => self.layout_response_for_transition(&transition),
                    Err(error) => {
                        warn!(
                            ?error,
                            ?app_window_id,
                            "Core rejected automatic workspace switch"
                        );
                        return;
                    }
                };
                response.focus_window = Some(app_window_id);
                if !response.raise_windows.contains(&app_window_id) {
                    response.raise_windows.push(app_window_id);
                }
                self.handle_layout_response(response, Some(window_space));
            }
        }
    }

    fn handle_layout_response(
        &mut self,
        response: layout::EventResponse,
        workspace_switch_space: Option<SpaceId>,
    ) {
        let workspace_switch_generation = if self.workspace_switch_manager.workspace_switch_state
            == WorkspaceSwitchState::Active
        {
            self.workspace_switch_manager.active_workspace_switch
        } else {
            None
        };
        if let Some(generation) = workspace_switch_generation
            && let Err(e) = self
                .communication_manager
                .raise_manager_tx
                .try_send(raise_manager::Event::WorkspaceSwitchStarted { generation })
        {
            warn!("Failed to supersede stale workspace raises: {}", e);
        }

        if self.is_in_drag() {
            self.workspace_switch_manager.mark_workspace_switch_inactive();
            return;
        }

        let mut pending_refocus_space =
            match std::mem::replace(&mut self.refocus_manager.refocus_state, RefocusState::None) {
                RefocusState::Pending(space) => Some(space),
                RefocusState::None => None,
            };
        let layout::EventResponse {
            raise_windows,
            mut focus_window,
            ..
        } = response;

        let original_focus = focus_window;

        let focus_quiet = workspace_switch_space.map_or(Quiet::No, |_| Quiet::Yes);

        let handled_without_raise = if raise_windows.is_empty() && focus_window.is_none() {
            if matches!(
                self.workspace_switch_manager.workspace_switch_state,
                WorkspaceSwitchState::Active
            ) && !self.is_in_drag()
            {
                // During an explicit workspace switch, do not let a cursor
                // window from another display steal the switch's focus target.
                let cursor_window = self.window_id_under_cursor().filter(|wid| {
                    workspace_switch_space.is_none_or(|space| {
                        self.best_space_for_window_id(*wid) == Some(space)
                            && self.is_window_in_active_workspace(space, *wid)
                    })
                });

                if let Some(wid) = cursor_window {
                    // Avoid duplicate focus events for the already focused window.
                    if self.main_window() != Some(wid) {
                        focus_window = Some(wid);
                    }
                    false
                } else {
                    // Empty workspaces still need to become the command
                    // context. With no window to focus, moving the pointer is
                    // the only platform action that transfers that context to
                    // the target display when mouse-follows-focus is enabled.
                    let warp_space =
                        workspace_switch_space.or_else(|| self.workspace_command_space());
                    if let Some(space) = workspace_switch_space {
                        if self.space_for_cursor_screen() == Some(space)
                            && self.focus_untracked_window_under_cursor()
                        {
                            true
                        } else {
                            self.config.settings.mouse_follows_focus
                                && warp_space
                                    .is_some_and(|space| self.warp_mouse_to_space_center(space))
                        }
                    } else {
                        self.try_focus_or_warp_without_raise(warp_space, &mut focus_window)
                    }
                }
            } else if let Some(space) = pending_refocus_space.take() {
                if let Some(wid) = self.last_focused_window_in_space(space) {
                    focus_window = Some(wid);
                    false
                } else if !self.is_in_drag() {
                    self.try_focus_or_warp_without_raise(Some(space), &mut focus_window)
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        let require_visible_focus = matches!(
            self.workspace_switch_manager.workspace_switch_state,
            WorkspaceSwitchState::Inactive
        );

        if let Some(wid) = focus_window
            && let Some(state) = self.window_manager.window(wid)
            && let Some(wsid) = state.info.sys_id
        {
            if require_visible_focus && !self.window_manager.is_window_visible(wsid) {
                focus_window = None;
            } else if !self
                .intended_space_for_window_state(wid, state)
                .is_some_and(|space| self.is_space_active(space))
            {
                focus_window = None;
            }
        }

        if raise_windows.is_empty() && focus_window.is_none() {
            if handled_without_raise {
                self.workspace_switch_manager.mark_workspace_switch_inactive();
            }
            if handled_without_raise
                || matches!(
                    self.workspace_switch_manager.workspace_switch_state,
                    WorkspaceSwitchState::Inactive
                )
            {
                return;
            }
        }

        if let Some(space) = pending_refocus_space {
            // Preserve the pending refocus request if it was not consumed above.
            if matches!(self.refocus_manager.refocus_state, RefocusState::None) {
                self.refocus_manager.refocus_state = RefocusState::Pending(space);
            }
        }

        let mut app_handles = HashMap::default();
        for &wid in raise_windows.iter() {
            self.insert_app_handle_for_window(&mut app_handles, wid);
        }

        if let Some(wid) = original_focus {
            self.insert_app_handle_for_window(&mut app_handles, wid);
        }

        let raise_windows: Vec<WindowId> = raise_windows
            .into_iter()
            .filter(|wid| self.is_window_on_active_space(*wid))
            .collect();
        let focus_window = focus_window.filter(|wid| self.is_window_on_active_space(*wid));
        if focus_window.is_some() {
            self.above_window = None;
        }

        let mut windows_by_app_and_screen = HashMap::default();
        for &wid in &raise_windows {
            windows_by_app_and_screen
                .entry((wid.pid, self.intended_space_for_window_id(wid)))
                .or_insert(vec![])
                .push(wid);
        }
        let focus_window_with_warp = focus_window.map(|wid| {
            let warp = if self.config.settings.mouse_follows_focus {
                if self.workspace_switch_manager.workspace_switch_state
                    == WorkspaceSwitchState::Active
                {
                    // During workspace switches, defer mouse warping until after layout completes.
                    self.workspace_switch_manager.pending_workspace_mouse_warp = Some(wid);
                    None
                } else {
                    self.window_center_on_known_screen(wid)
                }
            } else {
                None
            };
            (wid, warp)
        });
        let frontmost_pid = self.main_window().map(|wid| wid.pid);
        let quiet_activation_pid = workspace_switch_generation
            .and_then(|_| focus_window_with_warp.as_ref().map(|(wid, _)| wid.pid))
            .filter(|pid| Some(*pid) != frontmost_pid);

        let msg = raise_manager::Event::RaiseRequest(RaiseRequest {
            raise_windows: windows_by_app_and_screen.into_values().collect(),
            focus_window: focus_window_with_warp,
            app_handles,
            focus_quiet,
            workspace_switch_generation,
        });

        match self.communication_manager.raise_manager_tx.try_send(msg) {
            Ok(()) => {
                if let Some(pid) = quiet_activation_pid {
                    self.workspace_switch_manager.expect_quiet_activation(pid);
                }
            }
            Err(e) => warn!("Failed to send raise request to raise manager: {}", e),
        }
    }

    fn collect_drag_swap_candidates(
        &self,
        wid: WindowId,
        space: SpaceId,
    ) -> Vec<(WindowId, CGRect)> {
        self.window_manager
            .iter_windows()
            .filter_map(|(other_wid, other_state)| {
                if other_wid == wid {
                    return None;
                }
                let other_space = self.best_space_for_window_state(other_state)?;
                if other_space != space
                    || !self.is_window_in_active_workspace(space, other_wid)
                    || self.is_window_floating(other_wid)
                {
                    return None;
                }
                Some((other_wid, other_state.frame_monotonic))
            })
            .collect()
    }

    fn maybe_swap_on_drag(&mut self, wid: WindowId, new_frame: CGRect) {
        if !self.is_in_drag() {
            trace!(?wid, "Skipping swap: not in drag (mouse up received)");
            return;
        }

        let server_id = {
            let Some(window) = self.window_manager.window(wid) else {
                return;
            };
            window.info.sys_id
        };

        let Some(space) = self
            .get_active_drag_session()
            .and_then(|session| session.settled_space)
            .or_else(|| self.best_space_for_window(&new_frame, server_id))
        else {
            return;
        };

        let origin_space_hint = self
            .get_active_drag_session()
            .and_then(|session| session.origin_space)
            .or_else(|| {
                let frame = self.core_drag_snapshot().origin_frame?;
                let frame = CGRect::new(
                    CGPoint::new(frame.origin.x, frame.origin.y),
                    CGSize::new(frame.size.width, frame.size.height),
                );
                self.best_space_for_window(&frame, server_id)
            });

        if let Some(origin_space) = origin_space_hint
            && origin_space != space
        {
            if let Some((pending_wid, pending_target)) = self.get_pending_drag_swap()
                && pending_wid == wid
            {
                trace!(
                    ?wid,
                    ?pending_target,
                    ?origin_space,
                    ?space,
                    "Clearing pending drag swap; dragged window entered new space"
                );
                self.drag_manager.drag_state = DragState::Inactive;
            }
            trace!(
                ?wid,
                ?origin_space,
                ?space,
                "Resetting drag swap tracking after space change"
            );
            let _ = self.transition_core_input(crate::core::input::Input::Observation(
                crate::core::input::Observation::Drag(
                    crate::core::interaction::DragObservation::Cancelled,
                ),
            ));
            return;
        }

        if !self.is_window_in_active_workspace(space, wid) {
            return;
        }

        let candidates = self.collect_drag_swap_candidates(wid, space);

        let previous_pending = self.get_pending_drag_swap();
        let core_window =
            crate::core::ids::WindowId::new(crate::core::ids::ApplicationId(wid.pid), wid.idx);
        let core_frame = match crate::core::geometry::Rect::new(
            new_frame.origin.x,
            new_frame.origin.y,
            new_frame.size.width,
            new_frame.size.height,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                warn!(?error, ?wid, "Ignoring invalid drag frame");
                return;
            }
        };
        let candidates = candidates
            .into_iter()
            .filter_map(|(window, frame)| {
                Some(crate::core::interaction::DragCandidate {
                    window: crate::core::ids::WindowId::new(
                        crate::core::ids::ApplicationId(window.pid),
                        window.idx,
                    ),
                    frame: crate::core::geometry::Rect::new(
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    )
                    .ok()?,
                })
            })
            .collect();
        let transition = match self.transition_core_input(crate::core::input::Input::Observation(
            crate::core::input::Observation::Drag(
                crate::core::interaction::DragObservation::Updated {
                    window: core_window,
                    frame: core_frame,
                    candidates,
                },
            ),
        )) {
            Ok(transition) => transition,
            Err(error) => {
                warn!(?error, ?wid, "Core rejected drag update");
                return;
            }
        };
        let active_target = transition
            .snapshot
            .drag
            .target
            .map(|target| WindowId::new(target.application.0, target.index.get()));
        if let Some(target_wid) = active_target {
            if previous_pending != Some((wid, target_wid)) {
                trace!(
                    ?wid,
                    ?target_wid,
                    "Detected swap candidate; deferring until MouseUp"
                );
            }

            self.drag_manager.skip_layout_for_window = Some(wid);
            return;
        }

        if let Some((pending_wid, pending_target)) = previous_pending
            && pending_wid == wid
        {
            trace!(
                ?wid,
                ?pending_target,
                "Clearing pending drag swap; overlap ended before MouseUp"
            );
        }

        if self.drag_manager.skip_layout_for_window == Some(wid) {
            self.drag_manager.skip_layout_for_window = None;
        }
        // wait for mouse::up before doing *anything*
    }

    fn window_id_under_cursor(&self) -> Option<WindowId> {
        self.tracked_window_under_cursor().map(|(_, wid)| wid)
    }

    fn window_server_id_under_cursor(&self) -> Option<WindowServerId> {
        window_server::window_under_cursor()
    }

    fn tracked_window_under_cursor(&self) -> Option<(WindowServerId, WindowId)> {
        let wsid = self.window_server_id_under_cursor()?;
        let wid = self.window_manager.tracked_window_id(wsid)?;
        Some((wsid, wid))
    }

    fn activation_from_unmanageable_window(&self, pid: pid_t) -> Option<WindowServerId> {
        let (wsid, wid) = self.tracked_window_under_cursor()?;
        let window = self.window_manager.window(wid)?;
        (wid.pid == pid && !window.matches_filter(WindowFilter::EffectivelyManageable))
            .then_some(wsid)
    }

    fn has_manageable_window_on_inactive_workspace(&self, pid: pid_t) -> bool {
        self.window_manager.iter_windows().any(|(wid, window)| {
            if wid.pid != pid || !window.matches_filter(WindowFilter::EffectivelyManageable) {
                return false;
            }
            let Some(window_workspace) = self.workspace_for_window(wid) else {
                return false;
            };
            let Some(space) = self.intended_space_for_window_state(wid, window) else {
                return false;
            };
            self.active_workspace_for_space(space) != Some(window_workspace)
        })
    }

    fn capture_auxiliary_window_workspace_target(
        &mut self,
        info: Option<WindowServerInfo>,
        point: CGPoint,
    ) {
        self.refocus_manager.auxiliary_window_workspace_target = None;
        let Some(info) = info else {
            return;
        };
        let wsid = info.id;

        let pid = if let Some(wid) = self.window_manager.tracked_window_id(wsid) {
            let Some(window) = self.window_manager.window(wid) else {
                return;
            };
            if window.matches_filter(WindowFilter::EffectivelyManageable) {
                return;
            }
            wid.pid
        } else {
            // Floating controllers commonly live above the normal window layer and
            // may disappear as soon as they are clicked. Only trust such a surface
            // when the same app already has a manageable window hidden on an
            // inactive Lift workspace; this excludes ordinary menus and system UI.
            if !untracked_window_is_focusable(&info)
                && !self.has_manageable_window_on_inactive_workspace(info.pid)
            {
                return;
            }
            info.pid
        };

        if !self.app_manager.apps.contains_key(&pid) {
            return;
        }

        let Some(space) = self.space_for_point(point) else {
            return;
        };
        let Some(workspace_id) = self.active_workspace_for_space(space) else {
            return;
        };

        debug!(
            ?wsid,
            pid,
            ?space,
            ?workspace_id,
            "Auxiliary window clicked; remembering its expansion workspace"
        );
        self.refocus_manager.auxiliary_window_workspace_target =
            Some(managers::AuxiliaryWindowWorkspaceTarget {
                pid,
                space,
                workspace_id,
                expires_at: Instant::now() + AUXILIARY_WINDOW_EXPANSION_TIMEOUT,
            });
    }

    fn auxiliary_window_workspace_target(
        &mut self,
        pid: pid_t,
    ) -> Option<managers::AuxiliaryWindowWorkspaceTarget> {
        let target = self.refocus_manager.auxiliary_window_workspace_target?;
        if Instant::now() > target.expires_at
            || self.active_workspace_for_space(target.space) != Some(target.workspace_id)
        {
            self.refocus_manager.auxiliary_window_workspace_target = None;
            return None;
        }
        if target.pid != pid {
            return None;
        }
        Some(target)
    }

    fn take_auxiliary_window_workspace_target(
        &mut self,
        pid: pid_t,
    ) -> Option<managers::AuxiliaryWindowWorkspaceTarget> {
        self.auxiliary_window_workspace_target(pid)?;
        self.refocus_manager.auxiliary_window_workspace_target.take()
    }

    fn move_window_to_auxiliary_click_workspace(&mut self, window: WindowId) -> bool {
        let Some(target) = self.take_auxiliary_window_workspace_target(window.pid) else {
            return false;
        };
        if !self
            .window_manager
            .window(window)
            .is_some_and(|state| state.matches_filter(WindowFilter::EffectivelyManageable))
        {
            return false;
        }

        let _ =
            self.remember_recent_workspace_target_for(window, target.space, target.workspace_id);
        if self.workspace_for_window(window) == Some(target.workspace_id) {
            return true;
        }
        let Some(workspace) = self.workspace_number(target.workspace_id) else {
            return false;
        };
        let command = crate::core::command::WorkspaceCommand::MoveWindow {
            workspace,
            window: Some(Self::core_window_id(window)),
        };
        match self.transition_core_command(crate::core::command::Command::Workspace(command)) {
            Ok(_) => {
                debug!(
                    ?window,
                    ?target.space,
                    ?target.workspace_id,
                    "Moved expanded auxiliary window to its click workspace"
                );
                true
            }
            Err(error) => {
                warn!(
                    ?error,
                    ?window,
                    ?target.workspace_id,
                    "Core rejected auxiliary expansion workspace assignment"
                );
                false
            }
        }
    }

    fn move_main_window_to_auxiliary_click_workspace(&mut self, pid: pid_t) -> bool {
        let Some(window) = self.main_window_tracker.main_window_for_pid(pid) else {
            return false;
        };
        self.move_window_to_auxiliary_click_workspace(window)
    }

    fn focus_untracked_window_under_cursor(&mut self) -> bool {
        let Some(wsid) = self.window_server_id_under_cursor() else {
            return false;
        };
        if self.window_manager.tracked_window_id(wsid).is_some() {
            return false;
        }

        let window_info = self
            .window_manager
            .get_window_server_info(wsid)
            .or_else(|| window_server::get_window(wsid));

        let Some(info) = window_info else { return false };
        if !untracked_window_is_focusable(&info) {
            trace!(
                ?wsid,
                layer = info.layer,
                "Skipping non-application surface under cursor"
            );
            return false;
        }
        window_server::make_key_window(info.pid, wsid).is_ok()
    }

    fn last_focused_window_in_space(&self, space: SpaceId) -> Option<WindowId> {
        let active_workspace = self.active_workspace_for_space(space)?;
        let wid = self.last_tiled_window_in_workspace(active_workspace)?;
        let window = self.window_manager.window(wid)?;

        if self.best_space_for_window_id(wid)? != space {
            return None;
        }
        if window
            .info
            .sys_id
            .is_some_and(|wsid| !self.window_manager.is_window_visible(wsid))
        {
            return None;
        }
        Some(wid)
    }

    fn request_refocus_if_hidden(&mut self, space: SpaceId, window_id: WindowId) {
        if self.window_in_non_active_workspace(space, window_id) {
            self.refocus_manager.refocus_state = RefocusState::Pending(space);
        }
    }

    fn window_in_non_active_workspace(&self, space: SpaceId, window_id: WindowId) -> bool {
        let Some(active_workspace) = self.active_workspace_for_space(space) else {
            return false;
        };
        self.workspace_for_window(window_id)
            .is_some_and(|window_workspace| window_workspace != active_workspace)
    }

    fn prepare_refocus_after_layout_event(&mut self, event: &LayoutEvent) {
        match event {
            LayoutEvent::WindowAdded(space, wid) => {
                self.request_refocus_if_hidden(*space, *wid);
            }
            _ => {}
        }
    }

    #[instrument(skip(self))]
    fn raise_window(&mut self, wid: WindowId, quiet: Quiet, warp: Option<CGPoint>) {
        let mut app_handles = HashMap::default();
        if let Some(app) = self.app_manager.apps.get(&wid.pid) {
            app_handles.insert(wid.pid, app.handle.clone());
        }
        _ = self
            .communication_manager
            .raise_manager_tx
            .send(raise_manager::Event::RaiseRequest(RaiseRequest {
                raise_windows: vec![vec![wid]],
                focus_window: Some((wid, warp)),
                app_handles,
                focus_quiet: quiet,
                workspace_switch_generation: None,
            }));
    }

    pub(crate) fn request_focus_next_window(&mut self) {
        self.refocus_manager.focus_next_window_deadline =
            Some(Instant::now() + FOCUS_NEXT_WINDOW_TIMEOUT);
        self.refocus_manager.focus_next_window_target = self.focus_next_window_target();
    }

    pub(crate) fn cancel_focus_next_window(&mut self) {
        self.refocus_manager.focus_next_window_deadline = None;
        self.refocus_manager.focus_next_window_target = None;
    }

    fn focus_next_window_target(&self) -> Option<managers::FocusNextWindowTarget> {
        let space = self.exec_command_space()?;
        let workspace_id = self.active_workspace_for_space(space)?;
        Some(managers::FocusNextWindowTarget { space, workspace_id })
    }

    fn exec_command_space(&self) -> Option<SpaceId> {
        self.main_window_space()
            .or_else(|| get_active_space_number())
            .or_else(|| self.space_for_cursor_screen())
            .or_else(|| self.space_manager.first_known_space())
            .filter(|space| self.is_space_active(*space))
    }

    pub(crate) fn focus_next_window_target_for(
        &self,
        wid: WindowId,
    ) -> Option<managers::FocusNextWindowTarget> {
        let deadline = self.refocus_manager.focus_next_window_deadline?;
        if Instant::now() > deadline {
            return None;
        }

        let target = self.refocus_manager.focus_next_window_target?;
        let window = self.window_manager.window(wid)?;
        if !window.matches_filter(WindowFilter::EffectivelyManageable) {
            return None;
        }
        if self.space_for_workspace(target.workspace_id) != Some(target.space) {
            return None;
        }
        if self.active_workspace_for_space(target.space) != Some(target.workspace_id) {
            return None;
        }

        Some(target)
    }

    fn consume_focus_next_window_for(&mut self, wid: WindowId) -> bool {
        if let Err(error) = self.advance_core_state() {
            debug!(
                ?error,
                ?wid,
                "Deferring exec-window focus until core observation succeeds"
            );
            return false;
        }
        let Some(deadline) = self.refocus_manager.focus_next_window_deadline else {
            return false;
        };

        if Instant::now() > deadline {
            self.cancel_focus_next_window();
            return false;
        }

        if !self
            .window_manager
            .window(wid)
            .is_some_and(|window| window.matches_filter(WindowFilter::EffectivelyManageable))
        {
            return false;
        }

        if let Some(target) = self.focus_next_window_target_for(wid)
            && self.workspace_for_window(wid) != Some(target.workspace_id)
        {
            let Some(workspace) = self.workspace_number(target.workspace_id) else {
                return false;
            };
            let command = crate::core::command::WorkspaceCommand::MoveWindow {
                workspace,
                window: Some(Self::core_window_id(wid)),
            };
            if let Err(error) =
                self.transition_core_command(crate::core::command::Command::Workspace(command))
            {
                debug!(
                    ?error,
                    ?wid,
                    "Failed to assign exec window to its command workspace"
                );
                return false;
            }
        }

        let Some(window) = self.window_manager.window(wid) else {
            return false;
        };
        let Some(space) = self.intended_space_for_window_state(wid, window) else {
            return false;
        };
        if !self.is_space_active(space) || !self.is_window_in_active_workspace(space, wid) {
            return false;
        }

        self.cancel_focus_next_window();
        let warp = if self.config.settings.mouse_follows_focus {
            self.window_center_on_known_screen(wid)
        } else {
            None
        };
        self.raise_window(wid, Quiet::No, warp);
        self.send_layout_event(LayoutEvent::WindowFocused(wid));
        true
    }

    fn consume_focus_next_window_from<I>(&mut self, windows: I) -> bool
    where I: IntoIterator<Item = WindowId> {
        for wid in windows {
            if self.consume_focus_next_window_for(wid) {
                return true;
            }
        }
        false
    }

    fn clear_menu_state_for_pid(&mut self, pid: pid_t) {
        if matches!(self.menu_manager.menu_state, MenuState::Open(owner) if owner == pid) {
            debug!(pid, "Clearing menu-open state for deactivated app");
            self.menu_manager.menu_state = MenuState::Closed;
            self.update_focus_follows_mouse_state();
        }
    }

    fn clear_menu_state_for_non_owner(&mut self, pid: pid_t) {
        if matches!(self.menu_manager.menu_state, MenuState::Open(owner) if owner != pid) {
            debug!(pid, "Clearing stale menu-open state after app focus changed");
            self.menu_manager.menu_state = MenuState::Closed;
            self.update_focus_follows_mouse_state();
        }
    }

    fn set_focus_follows_mouse_enabled(&self, enabled: bool) {
        if let Some(event_tap_tx) = self.communication_manager.event_tap_tx.as_ref() {
            event_tap_tx.send(event_tap::Request::SetFocusFollowsMouseEnabled(enabled));
        }
    }

    fn update_focus_follows_mouse_state(&self) {
        let should_enable = self.config.settings.focus_follows_mouse
            && matches!(self.menu_manager.menu_state, MenuState::Closed)
            && !self.is_mission_control_active();
        self.set_focus_follows_mouse_enabled(should_enable);
    }

    fn set_mission_control_active(&mut self, active: bool) {
        if self.is_mission_control_active() == active {
            return;
        }
        if let Err(error) = self.transition_core_input(crate::core::input::Input::Observation(
            crate::core::input::Observation::MissionControl { active },
        )) {
            warn!(?error, active, "Core rejected Mission Control observation");
            return;
        }
        self.update_focus_follows_mouse_state();
    }

    fn refresh_windows_after_mission_control(&mut self) {
        debug!("Refreshing window state after Mission Control");
        // Skip when on a fullscreen space: kAXWindowsAttribute is space-filtered, so
        // apps omit their Desktop windows. check_for_new_windows sends an untracked
        // GetVisibleWindows whose response bypasses pending_mission_control_refresh,
        // causing those Desktop windows to be dropped from the layout, and other
        // windows in the layout to be incorrecctly resized.
        if !crate::sys::window_server::active_space_is_user() {
            return;
        }
        let ws_info = window_server::get_visible_windows_with_layer(None);
        self.update_partial_window_server_info(ws_info);
        self.mission_control_manager.pending_mission_control_refresh.clear();
        self.force_refresh_all_windows();
        self.check_for_new_windows();
        self.update_layout_or_warn(false, false);
        self.maybe_send_menu_update();
    }

    fn force_refresh_all_windows(&mut self) { self.request_visible_windows_for_apps(true); }

    fn request_close_window(&mut self, wid: WindowId) {
        if let Some(app) = self.app_manager.apps.get(&wid.pid) {
            if let Err(err) = app.handle.send(Request::CloseWindow(wid)) {
                warn!(?wid, "Failed to send close window request: {}", err);
            }
        }
    }

    fn main_window(&self) -> Option<WindowId> { self.main_window_tracker.main_window() }

    fn main_window_space(&self) -> Option<SpaceId> {
        // TODO: Optimize this with a cache or something.
        let wid = self.main_window()?;
        self.intended_space_for_window_id(wid)
    }

    fn workspace_command_space(&self) -> Option<SpaceId> {
        let candidate = self
            .space_for_cursor_screen()
            .or_else(|| self.main_window_space())
            .or_else(|| get_active_space_number())
            .or_else(|| self.space_manager.first_known_space());

        candidate.filter(|space| self.is_space_active(*space))
    }

    fn space_for_cursor_screen(&self) -> Option<SpaceId> {
        current_cursor_location().ok().and_then(|point| self.space_for_point(point))
    }

    fn space_for_point(&self, point: CGPoint) -> Option<SpaceId> {
        self.screen_for_point(point)
            .or_else(|| self.closest_screen_to_point(point))
            .and_then(|screen| screen.space)
    }

    fn screen_for_point(&self, point: CGPoint) -> Option<&ScreenInfo> {
        self.space_manager.screens.iter().find(|screen| screen.frame.contains(point))
    }

    fn closest_screen_to_point(&self, point: CGPoint) -> Option<&ScreenInfo> {
        self.space_manager.screens.iter().min_by(|a, b| {
            let da = Self::rectangle_distance_sq(a.frame, point);
            let db = Self::rectangle_distance_sq(b.frame, point);
            da.total_cmp(&db)
        })
    }

    fn rectangle_distance_sq(frame: CGRect, point: CGPoint) -> f64 {
        let min_x = frame.origin.x;
        let max_x = frame.origin.x + frame.size.width;
        let min_y = frame.origin.y;
        let max_y = frame.origin.y + frame.size.height;

        let dx = if point.x < min_x {
            min_x - point.x
        } else if point.x > max_x {
            point.x - max_x
        } else {
            0.0
        };

        let dy = if point.y < min_y {
            min_y - point.y
        } else if point.y > max_y {
            point.y - max_y
        } else {
            0.0
        };

        dx * dx + dy * dy
    }

    fn current_screen_center(&self) -> Option<CGPoint> {
        if let Ok(point) = current_cursor_location() {
            if let Some(screen) = self.screen_for_point(point) {
                return Some(screen.frame.mid());
            }
        }

        if let Some(space) = self.main_window_space() {
            if let Some(screen) = self.space_manager.screen_by_space(space) {
                return Some(screen.frame.mid());
            }
        }

        if let Some(space) = get_active_space_number() {
            if let Some(screen) = self.space_manager.screen_by_space(space) {
                return Some(screen.frame.mid());
            }
        }

        self.space_manager.screens.first().map(|screen| screen.frame.mid())
    }

    fn screen_for_direction_from_point(
        &self,
        origin: CGPoint,
        direction: Direction,
    ) -> Option<&ScreenInfo> {
        fn interval_gap(a_min: f64, a_max: f64, b_min: f64, b_max: f64) -> f64 {
            if a_max < b_min {
                b_min - a_max
            } else if b_max < a_min {
                a_min - b_max
            } else {
                0.0
            }
        }

        let mut best: Option<(f64, f64, &ScreenInfo)> = None;

        for screen in &self.space_manager.screens {
            let frame = screen.frame;

            if frame.contains(origin) {
                continue;
            }

            let min = frame.min();
            let max = frame.max();

            let (primary_dist, orth_gap) = match direction {
                Direction::Left => {
                    if max.x > origin.x {
                        continue;
                    }
                    (origin.x - max.x, interval_gap(min.y, max.y, origin.y, origin.y))
                }
                Direction::Right => {
                    if min.x < origin.x {
                        continue;
                    }
                    (min.x - origin.x, interval_gap(min.y, max.y, origin.y, origin.y))
                }
                Direction::Up => {
                    // Smaller y means visually "up".
                    if max.y > origin.y {
                        continue;
                    }
                    (origin.y - max.y, interval_gap(min.x, max.x, origin.x, origin.x))
                }
                Direction::Down => {
                    if min.y < origin.y {
                        continue;
                    }
                    (min.y - origin.y, interval_gap(min.x, max.x, origin.x, origin.x))
                }
            };

            let should_replace = best.as_ref().map_or(true, |(best_primary, best_orth, _)| {
                primary_dist < *best_primary
                    || (primary_dist == *best_primary && orth_gap < *best_orth)
            });

            if should_replace {
                best = Some((primary_dist, orth_gap, screen));
            }
        }

        best.map(|(_, _, screen)| screen)
    }

    fn screen_for_selector(
        &self,
        selector: &DisplaySelector,
        origin_override: Option<CGPoint>,
    ) -> Option<&ScreenInfo> {
        match selector {
            DisplaySelector::Direction(direction) => {
                let origin = origin_override.or_else(|| self.current_screen_center())?;
                self.screen_for_direction_from_point(origin, *direction)
            }
            DisplaySelector::Index(index) => self.screens_in_physical_order().get(*index).copied(),
            DisplaySelector::Uuid(uuid) => {
                self.space_manager.screens.iter().find(|screen| screen.display_uuid == *uuid)
            }
        }
    }

    fn screens_in_physical_order(&self) -> Vec<&ScreenInfo> {
        let mut screens: Vec<&ScreenInfo> = self.space_manager.screens.iter().collect();
        screens.sort_by(|a, b| {
            let x_order = a.frame.origin.x.total_cmp(&b.frame.origin.x);
            if x_order == std::cmp::Ordering::Equal {
                a.frame.origin.y.total_cmp(&b.frame.origin.y)
            } else {
                x_order
            }
        });
        screens
    }

    fn store_current_floating_positions(&mut self, space: SpaceId) {
        if self.active_workspace_for_space(space).is_some()
            && let Err(error) = self.advance_core_state()
        {
            debug!(?error, ?space, "Core deferred floating-position observation");
        }
    }

    pub(crate) fn update_layout_or_warn(
        &mut self,
        is_resize: bool,
        is_workspace_switch: bool,
    ) -> bool {
        self.update_layout_or_warn_with(is_resize, is_workspace_switch, "Layout update failed")
    }

    pub(crate) fn update_layout_or_warn_with(
        &mut self,
        is_resize: bool,
        is_workspace_switch: bool,
        context: &'static str,
    ) -> bool {
        managers::update_layout(self, is_resize, is_workspace_switch).unwrap_or_else(|e| {
            warn!(error = ?e, "{}", context);
            false
        })
    }
}
