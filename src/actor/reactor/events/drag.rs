use tracing::trace;

use crate::actor::reactor::{DragState, Reactor};
use crate::core::input::{Input, Observation};
use crate::core::interaction::DragObservation;

pub struct DragEventHandler;

impl DragEventHandler {
    pub fn handle_mouse_up(reactor: &mut Reactor) {
        let mut need_layout_refresh = false;

        let drag_snapshot = reactor.core_drag_snapshot();
        let mut pending_swap = reactor.get_pending_drag_swap();

        if let Some(dragged) = drag_snapshot.window {
            if let Err(error) = reactor.transition_core_input(Input::Observation(
                Observation::Drag(DragObservation::Committed { window: dragged }),
            )) {
                trace!(?error, "Core rejected drag completion");
                pending_swap = None;
            }
        }

        if let Some((dragged_wid, target_wid)) = pending_swap {
            trace!(?dragged_wid, ?target_wid, "Performing deferred swap on MouseUp");

            reactor.drag_manager.skip_layout_for_window = Some(dragged_wid);

            let windows_exist = {
                let registry = reactor.window_manager.as_ref();
                registry.contains_window(dragged_wid) && registry.contains_window(target_wid)
            };
            if !windows_exist {
                trace!(
                    ?dragged_wid,
                    ?target_wid,
                    "Skipping deferred swap; one of the windows no longer exists"
                );
            } else {
                need_layout_refresh = true;
            }
        }

        let finalize_needs_layout = reactor.finalize_active_drag();

        reactor.drag_manager.drag_state = DragState::Inactive;

        if finalize_needs_layout || reactor.drag_manager.skip_layout_for_window.is_some() {
            need_layout_refresh = true;
        }

        if need_layout_refresh {
            let skip_layout_occurred = reactor.drag_manager.skip_layout_for_window.is_some();
            let _ = reactor.update_layout_or_warn(false, false);
            if skip_layout_occurred {
                let _ = reactor.update_layout_or_warn(false, false);
            }
        }

        reactor.drag_manager.skip_layout_for_window = None;
    }
}
