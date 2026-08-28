//! Input processing via a CGEventTap on a dedicated thread.
//!
//! The `EventTap` (aka input processor) owns a `Default`-mode CGEventTap and
//! runs its own CFRunLoop on a dedicated thread (`input` thread). This isolates
//! keyboard/mouse input processing from main-thread stalls (layout computation,
//! animation, WindowServer IPC).
//!
//! Shared state between the input thread and the main thread uses lock-free
//! `Arc<ArcSwap<T>>` primitives:
//! - `SharedHotkeyTable`: hotkey bindings, written by the input thread on
//!   config/layout changes, read by the callback.
//! - `SharedHitRects`: stack-line indicator frames, written by the main-thread
//!   `StackLine` actor, read by the callback.
//!
//! Requests from the main thread arrive via the actor channel (`Receiver`).
//! The main thread's `GestureTap` is a separate `ListenOnly` tap for gestures.

use std::cell::{Cell, RefCell};
use std::panic::AssertUnwindSafe;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventMask, CGEventTapOptions as CGTapOpt, CGEventTapProxy, CGEventType,
};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, error, trace, warn};

use super::reactor::{self, Event};
use super::stack_line;
use crate::actor;
use crate::actor::wm_controller::{self, WmCommand, WmEvent};
use crate::common::collections::{HashMap, HashSet};
use crate::common::config::Config;
use crate::sys::event::{self, Hotkey, KeyCode, MouseState, set_mouse_state};
use crate::sys::hotkey::{
    Modifiers, is_modifier_key, key_code_from_event, modifier_flag_for_key,
    modifiers_from_flags_with_keys,
};
use crate::sys::screen::{CoordinateConverter, SpaceId};
use crate::sys::{power, window_server};
use crate::ui::stack_line::point_hits_indicator_frame;

const MOUSE_MOVE_MIN_INTERVAL_NS_NORMAL: u64 = 8_000_000; // 8ms ~= 125 Hz
const MOUSE_MOVE_MIN_DISTANCE_PX_SQ_NORMAL: f64 = 4.0; // 2px^2
const MOUSE_MOVE_MIN_INTERVAL_NS_LOW_POWER: u64 = 16_000_000; // 16ms ~= 62 Hz
const MOUSE_MOVE_MIN_DISTANCE_PX_SQ_LOW_POWER: f64 = 9.0; // 3px^2
const RESIZE_MODE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum Request {
    Warp(CGPoint),
    EnforceHidden,
    ScreenParametersChanged(Vec<(CGRect, Option<SpaceId>)>, CoordinateConverter),
    SpaceChanged(Vec<Option<SpaceId>>),
    SetEventProcessing(bool),
    SetFocusFollowsMouseEnabled(bool),
    SetHotkeys(Vec<(String, WmCommand)>),
    EnterResizeMode,
    KeyboardLayoutChanged,
    ConfigUpdated(Config),
    SetLowPowerMode(bool),
}

pub struct EventTap {
    events_tx: reactor::Sender,
    requests_rx: Option<Receiver>,
    state: RefCell<State>,
    event_mask: Cell<CGEventMask>,
    tap: RefCell<Option<crate::sys::event_tap::EventTap>>,
    tap_generation: Cell<u64>,
    disable_hotkey: RefCell<Option<Hotkey>>,
    hotkey_specs: RefCell<Vec<(String, WmCommand)>>,
    hotkeys: SharedHotkeyTable,
    wm_sender: wm_controller::Sender,
    stack_line_tx: stack_line::Sender,
    stack_line_hit_rects: stack_line::SharedHitRects,
}

// SAFETY: EventTap is constructed on the input thread and all access occurs on
// that same thread (CFRunLoop callback + channel recv both run on the input
// thread's run loop). The Send impl is required only to move the struct across
// the thread::spawn boundary.
unsafe impl Send for EventTap {}

struct State {
    hide_count: u32,
    mouse_hides_on_focus: bool,
    focus_follows_mouse_config_enabled: bool,
    converter: CoordinateConverter,
    screens: Vec<CGRect>,
    event_processing_enabled: bool,
    focus_follows_mouse_enabled: bool,
    stack_line_enabled: bool,
    disable_hotkey_active: bool,
    low_power_mode: bool,
    pressed_keys: HashSet<KeyCode>,
    current_flags: CGEventFlags,
    screen_spaces: Vec<(CGRect, SpaceId)>,
    last_mouse_move_loc: Option<CGPoint>,
    last_mouse_move_timestamp: u64,
    resize_mode_deadline: Option<Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            hide_count: 0,
            mouse_hides_on_focus: false,
            focus_follows_mouse_config_enabled: false,
            converter: CoordinateConverter::default(),
            screens: Vec::new(),
            event_processing_enabled: false,
            focus_follows_mouse_enabled: true,
            stack_line_enabled: false,
            disable_hotkey_active: false,
            low_power_mode: power::is_low_power_mode_enabled(),
            pressed_keys: HashSet::default(),
            current_flags: CGEventFlags::empty(),
            screen_spaces: Vec::new(),
            last_mouse_move_loc: None,
            last_mouse_move_timestamp: 0,
            resize_mode_deadline: None,
        }
    }
}

pub type Sender = actor::Sender<Request>;
pub type Receiver = actor::Receiver<Request>;

pub type SharedHotkeyTable = Arc<ArcSwap<HashMap<Hotkey, Vec<WmCommand>>>>;

struct CallbackCtx {
    this: Arc<EventTap>,
    recovery_tx: tokio::sync::mpsc::UnboundedSender<Recovery>,
    tap_generation: u64,
}

#[derive(Clone, Copy, Debug)]
enum Recovery {
    TapInvalidated(u64),
}

unsafe fn drop_mouse_ctx(ptr: *mut std::ffi::c_void) {
    unsafe { drop(Box::from_raw(ptr as *mut CallbackCtx)) };
}

unsafe extern "C-unwind" fn event_tap_invalidated(user_info: *mut std::ffi::c_void) {
    if user_info.is_null() {
        return;
    }
    let ctx = unsafe { &*(user_info as *const CallbackCtx) };
    let _ = ctx.recovery_tx.send(Recovery::TapInvalidated(ctx.tap_generation));
}

impl EventTap {
    #[inline]
    fn stack_line_hover_enabled(&self, state: &State) -> bool { state.stack_line_enabled }

    #[inline]
    fn focus_follows_mouse_handler_enabled(state: &State) -> bool {
        state.focus_follows_mouse_config_enabled && state.focus_follows_mouse_enabled
    }

    fn keyboard_handlers_enabled(&self) -> bool {
        self.disable_hotkey.borrow().is_some() || !self.hotkeys.load().is_empty()
    }

    fn mouse_move_handlers_enabled(&self) -> bool {
        let state = self.state.borrow();
        state.event_processing_enabled
            && (self.stack_line_hover_enabled(&state)
                || Self::focus_follows_mouse_handler_enabled(&state))
    }

    fn desired_event_mask(&self) -> CGEventMask {
        build_event_mask(
            self.keyboard_handlers_enabled(),
            self.mouse_move_handlers_enabled(),
        )
    }

    fn create_tap_with_mask(
        self: &Arc<Self>,
        mask: CGEventMask,
        recovery_tx: tokio::sync::mpsc::UnboundedSender<Recovery>,
    ) -> Option<crate::sys::event_tap::EventTap> {
        let tap_generation = self.tap_generation.get().wrapping_add(1);
        let ctx = Box::new(CallbackCtx {
            this: Arc::clone(self),
            recovery_tx,
            tap_generation,
        });
        let ctx_ptr = Box::into_raw(ctx) as *mut std::ffi::c_void;

        let tap = unsafe {
            crate::sys::event_tap::EventTap::new_with_options_and_invalidation_callback(
                CGTapOpt::Default,
                mask,
                Some(mouse_callback),
                ctx_ptr,
                Some(drop_mouse_ctx),
                Some(event_tap_invalidated),
            )
        };

        if tap.is_none() {
            unsafe { drop(Box::from_raw(ctx_ptr as *mut CallbackCtx)) };
        }

        if tap.is_some() {
            self.tap_generation.set(tap_generation);
        }
        tap
    }

    fn rebuild_event_tap_mask_if_needed(
        self: &Arc<Self>,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<Recovery>,
    ) {
        let next_mask = self.desired_event_mask();
        if next_mask == self.event_mask.get() {
            return;
        }

        let Some(new_tap) = self.create_tap_with_mask(next_mask, recovery_tx.clone()) else {
            warn!("Failed to rebuild event tap with updated mask");
            return;
        };

        let old_tap = self.tap.borrow_mut().replace(new_tap);
        drop(old_tap);
        self.event_mask.set(next_mask);
    }

    fn rebuild_invalidated_event_tap(
        self: &Arc<Self>,
        generation: u64,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<Recovery>,
    ) {
        if generation != self.tap_generation.get() {
            debug!(generation, "Ignoring invalidation from a replaced event tap");
            return;
        }
        let Some(new_tap) = self.create_tap_with_mask(self.event_mask.get(), recovery_tx.clone())
        else {
            error!(generation, "Failed to recreate invalidated event tap");
            return;
        };
        let old_tap = self.tap.borrow_mut().replace(new_tap);
        drop(old_tap);
        self.reconcile_after_tap_recreated();
        warn!(generation, "Recreated invalidated event tap");
    }

    fn reconcile_after_tap_recreated(&self) {
        let mut state = self.state.borrow_mut();
        state.pressed_keys.clear();
        state.current_flags = CGEventFlags::empty();
        state.reconcile_modifier_keys();
        drop(state);
        self.refresh_disable_hotkey_state(&mut self.state.borrow_mut());
    }

    pub fn new(
        config: Config,
        events_tx: reactor::Sender,
        requests_rx: Receiver,
        wm_sender: wm_controller::Sender,
        stack_line_tx: stack_line::Sender,
        stack_line_hit_rects: stack_line::SharedHitRects,
    ) -> Self {
        let disable_hotkey = config
            .settings
            .focus_follows_mouse_disable_hotkey
            .clone()
            .and_then(|spec| spec.to_hotkey());
        let mut state = State::default();
        state.mouse_hides_on_focus = config.settings.mouse_hides_on_focus;
        state.focus_follows_mouse_config_enabled = config.settings.focus_follows_mouse;
        state.stack_line_enabled = config.settings.ui.stack_line.enabled;
        state.disable_hotkey_active = disable_hotkey
            .as_ref()
            .map(|target| state.compute_disable_hotkey_active(target))
            .unwrap_or(false);
        let event_mask = build_event_mask(
            disable_hotkey.is_some(),
            state.event_processing_enabled
                && (state.stack_line_enabled || Self::focus_follows_mouse_handler_enabled(&state)),
        );
        EventTap {
            events_tx,
            requests_rx: Some(requests_rx),
            state: RefCell::new(state),
            event_mask: Cell::new(event_mask),
            tap: RefCell::new(None),
            tap_generation: Cell::new(0),
            disable_hotkey: RefCell::new(disable_hotkey),
            hotkey_specs: RefCell::new(Vec::new()),
            hotkeys: Arc::new(ArcSwap::from_pointee(HashMap::default())),
            wm_sender,
            stack_line_tx,
            stack_line_hit_rects,
        }
    }

    pub async fn run(mut self) {
        use tracing::Span;

        use crate::sys::timer::Timer;

        enum Tick {
            Request(Request),
            Watchdog,
            Recovery(Recovery),
        }

        let requests_rx = self.requests_rx.take().unwrap();

        let this = Arc::new(self);

        let mask = this.event_mask.get();
        let (recovery_tx, recovery_rx) = tokio::sync::mpsc::unbounded_channel();
        let tap = this.create_tap_with_mask(mask, recovery_tx.clone());

        if let Some(tap) = tap {
            *this.tap.borrow_mut() = Some(tap);
        } else {
            return;
        }

        if this.state.borrow().mouse_hides_on_focus {
            if let Err(e) = window_server::allow_hide_mouse() {
                error!(
                    "Could not enable mouse hiding: {e:?}. \
                    mouse_hides_on_focus will have no effect."
                );
            }
        }

        let watchdog = Timer::repeating(Duration::from_secs(5), Duration::from_secs(5));

        let mut merged = StreamExt::merge(
            StreamExt::merge(
                UnboundedReceiverStream::new(requests_rx)
                    .map(|(span, req)| (span, Tick::Request(req))),
                watchdog.map(|()| (Span::none(), Tick::Watchdog)),
            ),
            UnboundedReceiverStream::new(recovery_rx)
                .map(|recovery| (Span::none(), Tick::Recovery(recovery))),
        );

        while let Some((span, tick)) = merged.next().await {
            let _guard = span.enter();
            match tick {
                Tick::Request(request) => this.on_request(request, &recovery_tx),
                Tick::Watchdog => {
                    let tap_enabled = this.tap.borrow().is_some();
                    if let Some(tap) = this.tap.borrow().as_ref() {
                        tap.set_enabled(true);
                    }
                    // Full modifier reconciliation: prune any pressed_keys not
                    // reflected in the last known flags.
                    let mut state = this.state.borrow_mut();
                    state.reconcile_modifier_keys();
                    trace!(
                        tap_enabled,
                        event_mask = this.event_mask.get(),
                        pressed_keys = state.pressed_keys.len(),
                        disable_hotkey_active = state.disable_hotkey_active,
                        event_processing = state.event_processing_enabled,
                        "watchdog tick"
                    );
                }
                Tick::Recovery(Recovery::TapInvalidated(generation)) => {
                    this.rebuild_invalidated_event_tap(generation, &recovery_tx);
                }
            }
        }
    }

    fn on_request(
        self: &Arc<Self>,
        request: Request,
        recovery_tx: &tokio::sync::mpsc::UnboundedSender<Recovery>,
    ) {
        let mut should_rebuild_mask = false;
        let mut state = self.state.borrow_mut();
        match request {
            Request::Warp(point) => {
                if let Err(e) = event::warp_mouse(point) {
                    warn!("Failed to warp mouse: {e:?}");
                }
                if state.mouse_hides_on_focus && state.hide_count == 0 {
                    debug!("Hiding mouse");
                    state.hide_mouse();
                }
            }
            Request::EnforceHidden => {
                if state.hide_count > 0 {
                    state.hide_mouse();
                }
            }
            Request::ScreenParametersChanged(screens_with_spaces, converter) => {
                state.screens = screens_with_spaces.iter().map(|(frame, _)| *frame).collect();
                state.screen_spaces = screens_with_spaces
                    .into_iter()
                    .filter_map(|(frame, maybe_space)| maybe_space.map(|space| (frame, space)))
                    .collect();
                state.converter = converter;
            }
            Request::SpaceChanged(spaces) => {
                state.screen_spaces = state
                    .screens
                    .iter()
                    .copied()
                    .zip(spaces.into_iter())
                    .filter_map(|(frame, maybe_space)| maybe_space.map(|space| (frame, space)))
                    .collect();
            }
            Request::SetEventProcessing(enabled) => {
                state.event_processing_enabled = enabled;
                state.reset(enabled);
                should_rebuild_mask = true;
            }
            Request::SetFocusFollowsMouseEnabled(enabled) => {
                debug!(
                    "focus_follows_mouse temporarily {}",
                    if enabled { "enabled" } else { "disabled" }
                );
                state.focus_follows_mouse_enabled = enabled;
                state.reset(enabled);
                should_rebuild_mask = true;
            }
            Request::SetHotkeys(bindings) => {
                *self.hotkey_specs.borrow_mut() = bindings;
                self.rebuild_hotkeys_for_current_layout();
                should_rebuild_mask = true;
            }
            Request::EnterResizeMode => state.enter_resize_mode(Instant::now()),
            Request::KeyboardLayoutChanged => {
                self.rebuild_hotkeys_for_current_layout();
                should_rebuild_mask = true;
            }
            Request::ConfigUpdated(new_config) => {
                let mouse_hides_on_focus = new_config.settings.mouse_hides_on_focus;
                let focus_follows_mouse_config_enabled = new_config.settings.focus_follows_mouse;
                let stack_line_enabled = new_config.settings.ui.stack_line.enabled;
                let disable_hotkey = new_config
                    .settings
                    .focus_follows_mouse_disable_hotkey
                    .clone()
                    .and_then(|spec| spec.to_hotkey());
                *self.disable_hotkey.borrow_mut() = disable_hotkey;
                {
                    let prev_mouse_hides_on_focus = state.mouse_hides_on_focus;
                    state.mouse_hides_on_focus = mouse_hides_on_focus;
                    state.focus_follows_mouse_config_enabled = focus_follows_mouse_config_enabled;
                    state.stack_line_enabled = stack_line_enabled;
                    let prev_active = state.disable_hotkey_active;
                    state.disable_hotkey_active = self
                        .disable_hotkey
                        .borrow()
                        .as_ref()
                        .map(|target| state.compute_disable_hotkey_active(target))
                        .unwrap_or(false);
                    if prev_active && !state.disable_hotkey_active {
                        state.reset(true);
                    }
                    if prev_mouse_hides_on_focus
                        && !state.mouse_hides_on_focus
                        && state.hide_count > 0
                    {
                        debug!("Showing mouse after disabling mouse_hides_on_focus");
                        state.show_mouse();
                    }
                }
                should_rebuild_mask = true;
            }
            Request::SetLowPowerMode(enabled) => {
                if state.low_power_mode != enabled {
                    debug!("low_power_mode changed in event tap: {}", enabled);
                    state.low_power_mode = enabled;
                    state.last_mouse_move_loc = None;
                    state.last_mouse_move_timestamp = 0;
                }
            }
        }
        drop(state);

        if should_rebuild_mask {
            self.rebuild_event_tap_mask_if_needed(recovery_tx);
        }
    }

    fn refresh_disable_hotkey_state(&self, state: &mut State) {
        let Some(target) = self.disable_hotkey.borrow().as_ref().cloned() else {
            return;
        };
        let prev_active = state.disable_hotkey_active;
        state.disable_hotkey_active = state.compute_disable_hotkey_active(&target);
        if state.disable_hotkey_active != prev_active {
            if state.disable_hotkey_active {
                debug!(?target, "focus_follows_mouse disabled while hotkey held");
            } else {
                debug!(?target, "focus_follows_mouse re-enabled after hotkey release");
                state.reset(true);
            }
        }
    }

    fn on_event(self: &Arc<Self>, event_type: CGEventType, event: &CGEvent) -> bool {
        // Check if the tap was re-enabled after being disabled by timeout or
        // user input. If so, clear pressed_keys to avoid phantom modifiers
        // from lost key-up events during the disabled period.
        if let Some(tap) = self.tap.borrow().as_ref() {
            if tap.take_reenabled_flag() {
                let mut state = self.state.borrow_mut();
                debug!(
                    "Event tap was re-enabled; clearing pressed_keys to prevent phantom modifiers"
                );
                state.pressed_keys.clear();
                state.current_flags = CGEvent::flags(Some(event));
                state.reconcile_modifier_keys();
                drop(state);
                self.refresh_disable_hotkey_state(&mut self.state.borrow_mut());
            }
        }

        let mut state = self.state.borrow_mut();

        if !matches!(
            event_type,
            CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged
        ) {
            // Keep modifier-only hotkey state in sync even when macOS drops a
            // key-up/flags-changed event (common after system UI interruptions).
            let flags = CGEvent::flags(Some(event));
            if flags != state.current_flags {
                state.current_flags = flags;
                state.reconcile_modifier_keys();
                self.refresh_disable_hotkey_state(&mut state);
            }
        }

        match event_type {
            CGEventType::LeftMouseDown | CGEventType::RightMouseDown => {
                set_mouse_state(MouseState::Down);

                let loc = CGEvent::location(Some(event));

                // The event tap is the single source of hit-testing for
                // stack-line indicators. Only forward the click and
                // suppress propagation when it lands on a visible,
                // non-occluded indicator.
                let hits_stack_line = self
                    .stack_line_hit_rects
                    .load()
                    .iter()
                    .copied()
                    .any(|frame| point_hits_indicator_frame(loc, frame));
                if hits_stack_line && !window_server::is_point_occluded_by_external_window(loc) {
                    let _ = self.stack_line_tx.try_send(stack_line::Event::MouseDown(loc));
                    return false;
                }
            }
            CGEventType::LeftMouseDragged | CGEventType::RightMouseDragged => {
                set_mouse_state(MouseState::Down);
            }
            CGEventType::LeftMouseUp | CGEventType::RightMouseUp => set_mouse_state(MouseState::Up),
            _ => {}
        }

        if matches!(
            event_type,
            CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged
        ) {
            return self.handle_keyboard_event(event_type, event, &mut state);
        }

        if !state.event_processing_enabled {
            trace!("Mouse event processing disabled, ignoring {:?}", event_type);
            return true;
        }

        if state.hide_count > 0 {
            debug!("Showing mouse");
            state.show_mouse();
        }
        match event_type {
            CGEventType::LeftMouseDown => {
                let loc = CGEvent::location(Some(event));
                let window =
                    window_server::get_window_at_point(loc).and_then(window_server::get_window);
                _ = self.events_tx.send(Event::MouseDown(window, loc));
            }
            CGEventType::RightMouseUp | CGEventType::LeftMouseUp => {
                _ = self.events_tx.send(Event::MouseUp);
            }
            CGEventType::MouseMoved => {
                let loc = CGEvent::location(Some(event));
                let ts = CGEvent::timestamp(Some(event));
                let sampling = mouse_move_sampling_profile(state.low_power_mode);
                if !state.should_sample_mouse_move(loc, ts, sampling) {
                    return true;
                }

                // stack line hover feedback
                if state.stack_line_enabled {
                    let hits = self
                        .stack_line_hit_rects
                        .load()
                        .iter()
                        .copied()
                        .any(|frame| point_hits_indicator_frame(loc, frame))
                        && !window_server::is_point_occluded_by_external_window(loc);
                    let _ = self.stack_line_tx.try_send(stack_line::Event::MouseMoved {
                        point: loc,
                        hits_indicator: hits,
                    });
                }

                // ffm — forward mouse move coordinates to the reactor.
                // All level-based filtering and window hit-testing happens in
                // the reactor so that blocking SLS IPC calls do not stall the
                // event tap thread.
                if state.focus_follows_mouse_config_enabled
                    && state.focus_follows_mouse_enabled
                    && !state.disable_hotkey_active
                {
                    _ = self.events_tx.send(Event::MouseMoved(loc));
                }
            }
            _ => (),
        }

        true
    }

    fn handle_keyboard_event(
        &self,
        event_type: CGEventType,
        event: &CGEvent,
        state: &mut State,
    ) -> bool {
        let key_code_opt = key_code_from_event(event);

        if let Some(key_code) = key_code_opt {
            match event_type {
                CGEventType::KeyDown => state.note_key_down(key_code),
                CGEventType::KeyUp => state.note_key_up(key_code),
                CGEventType::FlagsChanged => state.note_flags_changed(key_code),
                _ => {}
            }
        }

        let flags = CGEvent::flags(Some(event));
        state.current_flags = flags;
        self.refresh_disable_hotkey_state(state);

        if event_type == CGEventType::KeyDown {
            if let Some(key_code) = key_code_opt {
                if let Some(action) = state.resize_mode_action(key_code, Instant::now()) {
                    match action {
                        ResizeModeAction::Resize(direction) => {
                            let command = WmCommand::ReactorCommand(reactor::Command::Layout(
                                crate::model::layout::LayoutCommand::ResizeWindowDirectional(
                                    direction,
                                ),
                            ));
                            self.events_tx.send(Event::UserInput(
                                crate::runtime::diagnostics::UserInputTrace {
                                    source: "resize_mode".into(),
                                    input: key_code.to_string(),
                                    command: serde_json::to_value(&command).unwrap_or_else(|error| {
                                        serde_json::json!({"serialization_error": error.to_string()})
                                    }),
                                },
                            ));
                            self.wm_sender.send(WmEvent::Command(command));
                        }
                        ResizeModeAction::Exit => debug!("Exited resize mode"),
                    }
                    return false;
                }
                let hotkey = Hotkey::new(
                    modifiers_from_flags_with_keys(state.current_flags, &state.pressed_keys),
                    key_code,
                );
                let bindings = self.hotkeys.load();
                if let Some(commands) = bindings.get(&hotkey) {
                    for cmd in commands {
                        self.events_tx.send(Event::UserInput(
                            crate::runtime::diagnostics::UserInputTrace {
                                source: "hotkey".into(),
                                input: hotkey.to_string(),
                                command: serde_json::to_value(cmd).unwrap_or_else(|error| {
                                    serde_json::json!({"serialization_error": error.to_string()})
                                }),
                            },
                        ));
                        if matches!(cmd, WmCommand::Wm(wm_controller::WmCmd::ResizeMode)) {
                            state.enter_resize_mode(Instant::now());
                            debug!("Entered resize mode");
                        } else {
                            self.wm_sender.send(WmEvent::Command(cmd.clone()));
                        }
                    }
                    return false;
                }
            }
        }

        true
    }

    fn rebuild_hotkeys_for_current_layout(&self) {
        let specs = self.hotkey_specs.borrow();
        let mut map: HashMap<Hotkey, Vec<WmCommand>> = HashMap::default();

        for (spec, command) in specs.iter() {
            let Ok(hotkey) = Hotkey::from_str(spec) else {
                warn!(%spec, "Skipping hotkey that no longer resolves for current keyboard layout");
                continue;
            };

            if hotkey.modifiers.has_generic_modifiers() {
                for expanded_mods in hotkey.modifiers.expand_to_specific() {
                    let expanded_hotkey = Hotkey::new(expanded_mods, hotkey.key_code);
                    let entry = map.entry(expanded_hotkey).or_default();
                    if !entry.contains(command) {
                        entry.push(command.clone());
                    }
                }
            } else {
                let entry = map.entry(hotkey).or_default();
                if !entry.contains(command) {
                    entry.push(command.clone());
                }
            }
        }

        trace!(
            "Updated hotkey bindings for current keyboard layout: {}",
            map.len()
        );
        self.hotkeys.store(Arc::new(map));
    }
}

unsafe extern "C-unwind" fn mouse_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event_ref: core::ptr::NonNull<CGEvent>,
    user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let ctx = unsafe { &*(user_info as *const CallbackCtx) };
        let event = unsafe { event_ref.as_ref() };
        ctx.this.on_event(event_type, event)
    }));

    match result {
        Ok(true) => event_ref.as_ptr(),
        Ok(false) => core::ptr::null_mut(),
        Err(_) => event_ref.as_ptr(),
    }
}

impl State {
    fn enter_resize_mode(&mut self, now: Instant) {
        self.resize_mode_deadline = Some(now + RESIZE_MODE_TIMEOUT);
    }

    fn resize_mode_action(&mut self, key_code: KeyCode, now: Instant) -> Option<ResizeModeAction> {
        let deadline = self.resize_mode_deadline?;
        if now > deadline {
            self.resize_mode_deadline = None;
            return None;
        }
        let direction = match key_code {
            KeyCode::ArrowLeft => Some(crate::model::layout::Direction::Left),
            KeyCode::ArrowRight => Some(crate::model::layout::Direction::Right),
            KeyCode::ArrowUp => Some(crate::model::layout::Direction::Up),
            KeyCode::ArrowDown => Some(crate::model::layout::Direction::Down),
            KeyCode::Escape | KeyCode::Enter | KeyCode::NumpadEnter => {
                self.resize_mode_deadline = None;
                return Some(ResizeModeAction::Exit);
            }
            _ => {
                self.resize_mode_deadline = None;
                return None;
            }
        };
        self.resize_mode_deadline = Some(now + RESIZE_MODE_TIMEOUT);
        direction.map(ResizeModeAction::Resize)
    }

    fn hide_mouse(&mut self) {
        if let Err(e) = event::hide_mouse() {
            warn!("Failed to hide mouse: {e:?}");
        }
        self.hide_count += 1;
    }

    fn show_mouse(&mut self) {
        while self.hide_count > 0 {
            if let Err(e) = event::show_mouse() {
                warn!("Failed to show mouse: {e:?}");
            }
            self.hide_count -= 1;
        }
    }

    #[inline]
    fn should_sample_mouse_move(
        &mut self,
        loc: CGPoint,
        timestamp: u64,
        sampling: (u64, f64),
    ) -> bool {
        let Some(last_loc) = self.last_mouse_move_loc else {
            self.last_mouse_move_loc = Some(loc);
            self.last_mouse_move_timestamp = timestamp;
            return true;
        };

        let dx = loc.x - last_loc.x;
        let dy = loc.y - last_loc.y;
        let dist_sq = dx * dx + dy * dy;
        let elapsed = timestamp.saturating_sub(self.last_mouse_move_timestamp);

        if dist_sq < sampling.1 && elapsed < sampling.0 {
            return false;
        }

        self.last_mouse_move_loc = Some(loc);
        self.last_mouse_move_timestamp = timestamp;
        true
    }

    fn note_key_down(&mut self, key_code: KeyCode) { self.pressed_keys.insert(key_code); }

    fn note_key_up(&mut self, key_code: KeyCode) { self.pressed_keys.remove(&key_code); }

    fn note_flags_changed(&mut self, key_code: KeyCode) {
        if !is_modifier_key(key_code) {
            return;
        }
        // Determine whether this modifier is currently pressed by checking
        // the authoritative CGEventFlags, not our tracked set.
        if let Some(flag) = modifier_flag_for_key(key_code) {
            if self.current_flags.contains(flag) {
                self.pressed_keys.insert(key_code);
            } else {
                self.pressed_keys.remove(&key_code);
            }
        }
    }

    fn reconcile_modifier_keys(&mut self) {
        self.pressed_keys.retain(|key| {
            if let Some(flag) = modifier_flag_for_key(*key) {
                self.current_flags.contains(flag)
            } else {
                true // non-modifier keys are not reconciled here
            }
        });
    }

    fn compute_disable_hotkey_active(&self, target: &Hotkey) -> bool {
        let active_mods = modifiers_from_flags_with_keys(self.current_flags, &self.pressed_keys);

        let check_modifier = |left: Modifiers, right: Modifiers| -> bool {
            let target_has_left = target.modifiers.contains(left);
            let target_has_right = target.modifiers.contains(right);
            let active_has_left = active_mods.contains(left);
            let active_has_right = active_mods.contains(right);

            if target_has_left && target_has_right {
                active_has_left || active_has_right
            } else if target_has_left {
                active_has_left
            } else if target_has_right {
                active_has_right
            } else {
                true
            }
        };

        let shift_ok = check_modifier(Modifiers::SHIFT_LEFT, Modifiers::SHIFT_RIGHT);
        let ctrl_ok = check_modifier(Modifiers::CONTROL_LEFT, Modifiers::CONTROL_RIGHT);
        let alt_ok = check_modifier(Modifiers::ALT_LEFT, Modifiers::ALT_RIGHT);
        let meta_ok = check_modifier(Modifiers::META_LEFT, Modifiers::META_RIGHT);

        if !(shift_ok && ctrl_ok && alt_ok && meta_ok) {
            return false;
        }

        self.base_key_active(target.key_code)
    }

    fn base_key_active(&self, key_code: KeyCode) -> bool {
        if is_modifier_key(key_code) {
            modifier_flag_for_key(key_code)
                .map(|flag| self.current_flags.contains(flag))
                .unwrap_or(false)
        } else {
            self.pressed_keys.contains(&key_code)
        }
    }

    fn reset(&mut self, enabled: bool) {
        self.resize_mode_deadline = None;
        if enabled {
            self.last_mouse_move_loc = None;
            self.last_mouse_move_timestamp = 0;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResizeModeAction {
    Resize(crate::model::layout::Direction),
    Exit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::layout::Direction;

    #[test]
    fn resize_mode_maps_arrows_and_refreshes_its_deadline() {
        let mut state = State::default();
        let now = Instant::now();
        state.enter_resize_mode(now);

        assert_eq!(
            state.resize_mode_action(KeyCode::ArrowLeft, now + Duration::from_secs(9)),
            Some(ResizeModeAction::Resize(Direction::Left))
        );
        assert_eq!(
            state.resize_mode_action(KeyCode::ArrowDown, now + Duration::from_secs(18)),
            Some(ResizeModeAction::Resize(Direction::Down))
        );
    }

    #[test]
    fn resize_mode_exits_explicitly_or_after_inactivity() {
        let mut state = State::default();
        let now = Instant::now();
        state.enter_resize_mode(now);
        assert_eq!(
            state.resize_mode_action(KeyCode::Escape, now),
            Some(ResizeModeAction::Exit)
        );
        assert!(state.resize_mode_deadline.is_none());

        state.enter_resize_mode(now);
        assert_eq!(
            state.resize_mode_action(KeyCode::ArrowRight, now + RESIZE_MODE_TIMEOUT),
            Some(ResizeModeAction::Resize(Direction::Right))
        );
        assert_eq!(
            state.resize_mode_action(
                KeyCode::ArrowRight,
                now + RESIZE_MODE_TIMEOUT + RESIZE_MODE_TIMEOUT + Duration::from_millis(1),
            ),
            None
        );
        assert!(state.resize_mode_deadline.is_none());
    }
}

#[inline]
fn mouse_move_sampling_profile(low_power_mode: bool) -> (u64, f64) {
    if low_power_mode {
        (
            MOUSE_MOVE_MIN_INTERVAL_NS_LOW_POWER,
            MOUSE_MOVE_MIN_DISTANCE_PX_SQ_LOW_POWER,
        )
    } else {
        (
            MOUSE_MOVE_MIN_INTERVAL_NS_NORMAL,
            MOUSE_MOVE_MIN_DISTANCE_PX_SQ_NORMAL,
        )
    }
}

fn build_event_mask(keyboard_enabled: bool, mouse_move_enabled: bool) -> CGEventMask {
    let mut m: u64 = 0;
    let add = |m: &mut u64, ty: CGEventType| *m |= 1u64 << (ty.0 as u64);

    for ty in [
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
    ] {
        add(&mut m, ty);
    }
    if mouse_move_enabled {
        add(&mut m, CGEventType::MouseMoved);
    }
    if keyboard_enabled {
        for ty in [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ] {
            add(&mut m, ty);
        }
    }
    m
}
