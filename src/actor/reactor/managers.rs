use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use std::time::{Duration, Instant};
use tracing::trace;

use super::replay::Record;
use super::{
    AppState, Event, FullscreenSpaceTrack, PendingSpaceChange, ScreenInfo, WorkspaceSwitchOrigin,
    WorkspaceSwitchState,
};
use crate::actor;
use crate::actor::app::{WindowId, pid_t};
use crate::actor::broadcast::BroadcastSender;
use crate::actor::reactor::Reactor;
use crate::actor::reactor::animation::AnimationManager;
use crate::actor::{
    event_tap, gesture_tap, menu_bar, raise_manager, stack_line, window_notify, wm_controller,
};
use crate::common::collections::{HashMap, HashSet};
use crate::core::ids::WorkspaceId;
use crate::model::WindowRegistry;
use crate::sys::screen::SpaceId;

/// Manages window state and lifecycle
pub type WindowManager = Box<WindowRegistry>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecentWorkspaceTarget {
    pub space: SpaceId,
    pub workspace_id: WorkspaceId,
    pub expires_at: Instant,
}

/// Manages application state and rules
pub struct AppManager {
    pub apps: HashMap<pid_t, AppState>,
}

impl AppManager {
    pub fn new() -> Self {
        AppManager { apps: HashMap::default() }
    }
}

/// Manages space and screen state
pub struct SpaceManager {
    pub screens: Vec<ScreenInfo>,
    pub fullscreen_by_space: HashMap<u64, FullscreenSpaceTrack>,
    pub has_seen_display_set: bool,
}

impl SpaceManager {
    pub fn screen_by_space(&self, space: SpaceId) -> Option<&ScreenInfo> {
        self.screens.iter().find(|screen| screen.space == Some(space))
    }

    pub fn iter_known_spaces(&self) -> impl Iterator<Item = SpaceId> + '_ {
        self.screens.iter().filter_map(|screen| screen.space)
    }

    pub fn first_known_space(&self) -> Option<SpaceId> {
        self.iter_known_spaces().next()
    }
}

/// Manages drag operations and window swapping
pub struct DragManager {
    pub drag_state: super::DragState,
    pub skip_layout_for_window: Option<WindowId>,
}

/// Manages window notifications
pub struct NotificationManager {
    pub last_sls_notification_ids: Vec<u32>,
    pub _window_notify_tx: Option<window_notify::Sender>,
}

/// Manages menu state and interactions
pub struct MenuManager {
    pub menu_state: super::MenuState,
    pub menu_tx: Option<menu_bar::Sender>,
}

/// Tracks platform refreshes requested after Mission Control.
pub struct MissionControlManager {
    pub pending_mission_control_refresh: HashSet<pid_t>,
}

/// Manages workspace switching state
pub struct WorkspaceSwitchManager {
    pub workspace_switch_state: super::WorkspaceSwitchState,
    pub workspace_switch_generation: u64,
    pub active_workspace_switch: Option<u64>,
    pub pending_workspace_switch_origin: Option<WorkspaceSwitchOrigin>,
    pub pending_workspace_mouse_warp: Option<WindowId>,
    /// Carbon global-activation events do not carry the app actor's `Quiet` bit.
    pub quiet_activation_deadlines: HashMap<pid_t, Instant>,
}

impl WorkspaceSwitchManager {
    const QUIET_ACTIVATION_GRACE: Duration = Duration::from_secs(1);

    pub fn start_workspace_switch(&mut self, origin: WorkspaceSwitchOrigin) {
        self.workspace_switch_generation = self.workspace_switch_generation.wrapping_add(1);
        self.active_workspace_switch = Some(self.workspace_switch_generation);
        self.workspace_switch_state = WorkspaceSwitchState::Active;
        self.pending_workspace_switch_origin = Some(origin);
    }

    pub fn manual_switch_in_progress(&self) -> bool {
        self.workspace_switch_state == WorkspaceSwitchState::Active
            && self.pending_workspace_switch_origin == Some(WorkspaceSwitchOrigin::Manual)
    }

    pub fn mark_workspace_switch_inactive(&mut self) {
        self.workspace_switch_state = WorkspaceSwitchState::Inactive;
        self.pending_workspace_switch_origin = None;
    }

    pub fn expect_quiet_activation(&mut self, pid: pid_t) {
        let now = Instant::now();
        self.quiet_activation_deadlines.retain(|_, deadline| *deadline > now);
        self.quiet_activation_deadlines.insert(pid, now + Self::QUIET_ACTIVATION_GRACE);
    }

    pub fn should_suppress_global_activation(&mut self, pid: pid_t) -> bool {
        let now = Instant::now();
        self.quiet_activation_deadlines.retain(|_, deadline| *deadline > now);
        self.quiet_activation_deadlines.contains_key(&pid)
    }
}

/// Manages refocus and cleanup state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusNextWindowTarget {
    pub space: SpaceId,
    pub workspace_id: WorkspaceId,
}

pub struct RefocusManager {
    pub stale_cleanup_state: super::StaleCleanupState,
    pub refocus_state: super::RefocusState,
    pub focus_next_window_deadline: Option<Instant>,
    pub focus_next_window_target: Option<FocusNextWindowTarget>,
    pub recent_workspace_targets: HashMap<WindowId, RecentWorkspaceTarget>,
}

/// Manages communication channels to other actors
pub struct CommunicationManager {
    pub event_tap_tx: Option<event_tap::Sender>,
    pub gesture_tap_tx: Option<gesture_tap::Sender>,
    pub stack_line_tx: Option<stack_line::Sender>,
    pub raise_manager_tx: raise_manager::Sender,
    pub event_broadcaster: BroadcastSender,
    pub wm_sender: Option<wm_controller::Sender>,
    pub events_tx: Option<actor::Sender<Event>>,
}

/// Manages recording state
pub struct RecordingManager {
    pub record: Record,
}

pub type LayoutResult = Vec<(SpaceId, Vec<(WindowId, CGRect)>)>;

pub fn update_layout(
        reactor: &mut Reactor,
        is_resize: bool,
        is_workspace_switch: bool,
    ) -> Result<bool, crate::model::reactor::ReactorError> {
        let layout_result = calculate_layout(reactor);
        apply_layout(reactor, layout_result, is_resize, is_workspace_switch)
    }

fn calculate_layout(reactor: &mut Reactor) -> LayoutResult {
        let core_snapshot = reactor
            .advance_core_state()
            .map_err(|error| {
                tracing::warn!(?error, "Core layout planning deferred");
                error
            })
            .ok();
        let live_windows: HashSet<WindowId> =
            reactor.window_manager.iter_windows().map(|(wid, _)| wid).collect();
        if live_windows.is_empty() {
            return LayoutResult::new();
        }

        let screens = reactor.space_manager.screens.clone();
        let mut layout_result = LayoutResult::new();

        for screen in screens {
            let Some(space) = screen.space else {
                continue;
            };
            if !reactor.is_space_active(space) {
                continue;
            }
            let layout: Vec<(WindowId, CGRect)> = core_snapshot
                .as_ref()
                .map(|snapshot| {
                    crate::runtime::placement::frames_for_display(
                        snapshot,
                        &crate::core::ids::DisplayId(screen.display_uuid.clone()),
                    )
                })
                .unwrap_or_default()
                .into_iter()
                .map(|(window, frame)| {
                    (
                        WindowId::new(window.application.0, window.index.get()),
                        CGRect::new(
                            CGPoint::new(frame.origin.x, frame.origin.y),
                            CGSize::new(frame.size.width, frame.size.height),
                        ),
                    )
                })
                .collect();
            layout_result.push((space, layout));
        }

        layout_result
    }

fn apply_layout(
        reactor: &mut Reactor,
        layout_result: LayoutResult,
        is_resize: bool,
        is_workspace_switch: bool,
    ) -> Result<bool, crate::model::reactor::ReactorError> {
        let main_window = reactor.main_window();
        trace!(?main_window);
        let skip_wid = reactor
            .drag_manager
            .skip_layout_for_window
            .take()
            .or_else(|| {
                reactor.core_drag_snapshot().window.map(|window| {
                    WindowId::new(window.application.0, window.index.get())
                })
            });
        let mut any_frame_changed = false;

        for (space, layout) in layout_result {
            let suppress_animation = is_workspace_switch
                || reactor.workspace_switch_manager.active_workspace_switch.is_some();
            if suppress_animation {
                any_frame_changed |=
                    AnimationManager::instant_layout(reactor, space, &layout, skip_wid);
            } else {
                any_frame_changed |=
                    AnimationManager::animate_layout(reactor, space, &layout, is_resize, skip_wid);
            }
        }

        reactor.maybe_send_menu_update();
        Ok(any_frame_changed)
}

/// Manages pending space changes
pub struct PendingSpaceChangeManager {
    pub pending_space_change: Option<PendingSpaceChange>,
    pub topology_relayout_pending: bool,
    pub pending_removed_display_uuids: HashSet<String>,
}
