//! Gesture handling via a dedicated CGEventTap.
//!
//! This actor runs on the main thread and handles trackpad swipe gestures for
//! workspace switching.

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

use objc2::exception;
use objc2_app_kit::{NSEvent, NSEventPhase, NSEventType, NSTouchPhase, NSTouchType};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation as CGTapLoc,
    CGEventTapOptions as CGTapOpt, CGEventTapProxy, CGEventType,
};
use tracing::{trace, warn};

use crate::actor;
use crate::actor::reactor;
use crate::actor::wm_controller::{self, WmCommand, WmEvent};
use crate::common::config::{Config, HapticPattern};
use crate::model::layout::LayoutCommand as LC;
use crate::sys::haptics;

const K_CGS_EVENT_TYPE_FIELD: CGEventField = CGEventField(55);
const K_CGS_EVENT_DOCK_CONTROL: i64 = 30;
const K_GESTURE_HID_TYPE_FIELD: CGEventField = CGEventField(110);
const K_GESTURE_SWIPE_MOTION_FIELD: CGEventField = CGEventField(123);
const K_IOHID_EVENT_TYPE_DOCK_SWIPE: i64 = 23;
const K_CG_GESTURE_MOTION_HORIZONTAL: i64 = 1;

#[derive(Debug)]
pub enum GestureRequest {
    ConfigUpdated(Config),
}

pub type Sender = actor::Sender<GestureRequest>;
pub type Receiver = actor::Receiver<GestureRequest>;

pub struct GestureTap {
    config: RefCell<Config>,
    wm_sender: wm_controller::Sender,
    swipe: RefCell<Option<SwipeHandler>>,
    tap: RefCell<Option<crate::sys::event_tap::EventTap>>,
    requests_rx: Option<Receiver>,
}

#[derive(Debug, Clone)]
struct SwipeConfig {
    enabled: bool,
    consume_dock_swipe: bool,
    invert_horizontal: bool,
    vertical_tolerance: f64,
    skip_empty_workspaces: Option<bool>,
    fingers: usize,
    distance_pct: f64,
    haptics_enabled: bool,
    haptic_pattern: HapticPattern,
}

impl SwipeConfig {
    fn from_config(config: &Config) -> Self {
        let g = &config.settings.gestures;
        let vt_norm = if g.swipe_vertical_tolerance > 1.0 && g.swipe_vertical_tolerance <= 100.0 {
            (g.swipe_vertical_tolerance / 100.0).clamp(0.0, 1.0)
        } else if g.swipe_vertical_tolerance > 100.0 {
            1.0
        } else {
            g.swipe_vertical_tolerance.max(0.0).min(1.0)
        };
        SwipeConfig {
            enabled: g.enabled,
            consume_dock_swipe: g.consume_dock_swipe,
            invert_horizontal: g.invert_horizontal_swipe,
            vertical_tolerance: vt_norm,
            skip_empty_workspaces: if g.skip_empty { Some(true) } else { None },
            fingers: g.fingers.max(1),
            distance_pct: g.distance_pct.clamp(0.01, 1.0),
            haptics_enabled: g.haptics_enabled,
            haptic_pattern: g.haptic_pattern,
        }
    }
}

#[derive(Default, Debug)]
struct SwipeState {
    phase: GesturePhase,
    start_x: f64,
    start_y: f64,
}

impl SwipeState {
    fn reset(&mut self) {
        self.phase = GesturePhase::Idle;
        self.start_x = 0.0;
        self.start_y = 0.0;
    }
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
enum GesturePhase {
    #[default]
    Idle,
    Armed,
    Committed,
}

struct SwipeHandler {
    cfg: SwipeConfig,
    state: RefCell<SwipeState>,
}

struct CallbackCtx {
    this: Rc<GestureTap>,
    consumes: bool,
}

unsafe fn drop_gesture_ctx(ptr: *mut std::ffi::c_void) {
    unsafe { drop(Box::from_raw(ptr as *mut CallbackCtx)) };
}

impl GestureTap {
    pub fn new(config: Config, wm_sender: wm_controller::Sender, requests_rx: Receiver) -> Self {
        let swipe = Self::build_gesture_handler(&config);
        GestureTap {
            config: RefCell::new(config),
            wm_sender,
            swipe: RefCell::new(swipe),
            tap: RefCell::new(None),
            requests_rx: Some(requests_rx),
        }
    }

    pub async fn run(mut self) {
        let mut requests_rx = self.requests_rx.take().unwrap();

        let this = Rc::new(self);

        if this.gesture_handlers_enabled() {
            this.create_and_install_tap();
        }

        while let Some((span, request)) = requests_rx.recv().await {
            let _guard = span.enter();
            this.on_request(request);
        }
    }

    fn on_request(self: &Rc<Self>, request: GestureRequest) {
        match request {
            GestureRequest::ConfigUpdated(new_config) => {
                *self.config.borrow_mut() = new_config;
                self.update_gesture_handlers();
            }
        }
    }

    fn build_gesture_handler(config: &Config) -> Option<SwipeHandler> {
        let swipe_cfg = SwipeConfig::from_config(config);
        if swipe_cfg.enabled {
            Some(SwipeHandler {
                cfg: swipe_cfg,
                state: RefCell::new(SwipeState::default()),
            })
        } else {
            None
        }
    }

    fn update_gesture_handlers(self: &Rc<Self>) {
        let config = self.config.borrow();
        let swipe = Self::build_gesture_handler(&config);
        let was_enabled = self.gesture_handlers_enabled();
        *self.swipe.borrow_mut() = swipe;
        let is_enabled = self.gesture_handlers_enabled();

        if !was_enabled && is_enabled {
            self.create_and_install_tap();
        } else if was_enabled && !is_enabled {
            *self.tap.borrow_mut() = None;
        }
    }

    fn gesture_handlers_enabled(&self) -> bool { self.swipe.borrow().is_some() }

    fn create_and_install_tap(self: &Rc<Self>) {
        let mask = gesture_event_mask();
        let tap_location = CGTapLoc::HIDEventTap;
        let tap = unsafe {
            let ctx_ptr = Box::into_raw(Box::new(CallbackCtx {
                this: Rc::clone(self),
                consumes: true,
            })) as *mut std::ffi::c_void;
            match crate::sys::event_tap::EventTap::new_at_location_with_options(
                tap_location,
                CGTapOpt::Default,
                mask,
                Some(gesture_callback),
                ctx_ptr,
                Some(drop_gesture_ctx),
            ) {
                Some(tap) => Some(tap),
                None => {
                    drop(Box::from_raw(ctx_ptr as *mut CallbackCtx));
                    let ctx_ptr = Box::into_raw(Box::new(CallbackCtx {
                        this: Rc::clone(self),
                        consumes: false,
                    })) as *mut std::ffi::c_void;
                    match crate::sys::event_tap::EventTap::new_at_location_listen_only(
                        tap_location,
                        mask,
                        Some(gesture_callback),
                        ctx_ptr,
                        Some(drop_gesture_ctx),
                    ) {
                        Some(tap) => {
                            warn!(
                                "Falling back to listen-only HID gesture tap; workspace swipe events will pass through to macOS"
                            );
                            Some(tap)
                        }
                        None => {
                            drop(Box::from_raw(ctx_ptr as *mut CallbackCtx));
                            None
                        }
                    }
                }
            }
        };

        if let Some(tap) = tap {
            *self.tap.borrow_mut() = Some(tap);
        } else {
            tracing::warn!("Failed to create gesture event tap");
        }
    }

    fn on_event(self: &Rc<Self>, event_type: CGEventType, event: &CGEvent) -> bool {
        let swipe_handler = self.swipe.borrow();
        if swipe_handler.is_none() {
            return true;
        }

        if is_physical_horizontal_dock_swipe(event_type, event) {
            if swipe_handler.as_ref().is_some_and(|handler| handler.cfg.consume_dock_swipe) {
                return false;
            }
            return true;
        }

        if event_type.0 != NSEventType::Gesture.0 as u32 {
            return true;
        }

        if let Some(nsevent) = NSEvent::eventWithCGEvent(event)
            && nsevent.r#type() == NSEventType::Gesture
            && let Some(handler) = swipe_handler.as_ref()
        {
            self.handle_gesture_event(handler, &nsevent);
        }

        true
    }

    fn handle_gesture_event(&self, handler: &SwipeHandler, nsevent: &NSEvent) {
        let cfg = &handler.cfg;
        let state = &handler.state;

        let mut st = state.borrow_mut();

        let phase = nsevent.phase();
        if matches!(phase, NSEventPhase::Ended | NSEventPhase::Cancelled) {
            st.reset();
            return;
        }
        if matches!(phase, NSEventPhase::Began) {
            st.reset();
        }

        let touches = nsevent.allTouches();
        let mut sum_x = 0.0f64;
        let mut sum_y = 0.0f64;
        let mut touch_count = 0usize;
        let mut active_count = 0usize;
        let mut too_many_touches = false;

        for t in touches.iter() {
            let phase = t.phase();
            let ended =
                phase.contains(NSTouchPhase::Ended) || phase.contains(NSTouchPhase::Cancelled);

            touch_count += 1;
            if touch_count > cfg.fingers {
                too_many_touches = true;
                break;
            }

            if !ended && let Some((x, y)) = touch_normalized_position(&t) {
                sum_x += x;
                sum_y += y;
                active_count += 1;
            }
        }

        if too_many_touches || touch_count != cfg.fingers || active_count == 0 {
            st.reset();
            return;
        }

        let avg_x = sum_x / active_count as f64;
        let avg_y = sum_y / active_count as f64;

        match st.phase {
            GesturePhase::Idle => {
                st.start_x = avg_x;
                st.start_y = avg_y;
                st.phase = GesturePhase::Armed;
                trace!(
                    "swipe armed: start_x={:.3} start_y={:.3}",
                    st.start_x, st.start_y
                );
            }
            GesturePhase::Armed => {
                let dx = avg_x - st.start_x;
                let dy = avg_y - st.start_y;
                let horizontal = dx.abs();
                let vertical = dy.abs();

                if horizontal >= cfg.distance_pct && vertical <= cfg.vertical_tolerance {
                    let mut dir_left = dx < 0.0;
                    if cfg.invert_horizontal {
                        dir_left = !dir_left;
                    }
                    let cmd = if dir_left {
                        LC::NextWorkspace(cfg.skip_empty_workspaces)
                    } else {
                        LC::PrevWorkspace(cfg.skip_empty_workspaces)
                    };

                    if cfg.haptics_enabled {
                        let _ = haptics::perform_haptic(cfg.haptic_pattern);
                    }
                    self.wm_sender.send(WmEvent::Command(WmCommand::ReactorCommand(
                        reactor::Command::Layout(cmd),
                    )));
                    st.phase = GesturePhase::Committed;
                }
            }
            GesturePhase::Committed => {
                if active_count == 0 {
                    st.reset();
                }
            }
        }
    }
}

fn gesture_event_mask() -> CGEventMask {
    (1u64 << (NSEventType::Gesture.0 as u64)) | (1u64 << (K_CGS_EVENT_DOCK_CONTROL as u64))
}

fn is_physical_horizontal_dock_swipe(event_type: CGEventType, event: &CGEvent) -> bool {
    let cgs_type = CGEvent::integer_value_field(Some(event), K_CGS_EVENT_TYPE_FIELD);
    let hid_type = CGEvent::integer_value_field(Some(event), K_GESTURE_HID_TYPE_FIELD);
    let motion = CGEvent::integer_value_field(Some(event), K_GESTURE_SWIPE_MOTION_FIELD);

    (event_type.0 as i64 == K_CGS_EVENT_DOCK_CONTROL || cgs_type == K_CGS_EVENT_DOCK_CONTROL)
        && hid_type == K_IOHID_EVENT_TYPE_DOCK_SWIPE
        && motion == K_CG_GESTURE_MOTION_HORIZONTAL
}

#[inline]
fn touch_normalized_position(touch: &objc2_app_kit::NSTouch) -> Option<(f64, f64)> {
    if touch.r#type() != NSTouchType::Indirect || touch.isResting() {
        return None;
    }

    let position = std::panic::catch_unwind(AssertUnwindSafe(|| {
        exception::catch(AssertUnwindSafe(|| touch.normalizedPosition())).ok()
    }))
    .ok()
    .flatten()?;
    let x = position.x.clamp(0.0, 1.0) as f64;
    let y = position.y.clamp(0.0, 1.0) as f64;
    Some((x, y))
}

unsafe extern "C-unwind" fn gesture_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event_ref: core::ptr::NonNull<CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let ctx = unsafe { &*(user_info as *const CallbackCtx) };
        let event = unsafe { event_ref.as_ref() };
        (ctx.this.on_event(event_type, event), ctx.consumes)
    }));

    match result {
        Ok((true, _)) => event_ref.as_ptr(),
        Ok((false, true)) => core::ptr::null_mut(),
        Ok((false, false)) => event_ref.as_ptr(),
        Err(_) => event_ref.as_ptr(),
    }
}
