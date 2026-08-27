use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use tokio::sync::mpsc;
use tracing::{debug, trace};

use super::TransactionId;
use crate::actor::app::{AppThreadHandle, Request, WindowId, pid_t};
use crate::actor::reactor::Reactor;
use crate::common::collections::HashMap;
use crate::common::config::AnimationEasing;
use crate::sys::geometry::{Round, SameAs};
use crate::sys::power;
use crate::sys::screen::SpaceId;
use crate::sys::timer::Timer;
use crate::sys::window_animation::{WindowAnimationProxy, WindowAnimationUpdate};
use crate::sys::window_server::WindowServerId;

pub type Sender = mpsc::UnboundedSender<Message>;
pub type Receiver = mpsc::UnboundedReceiver<Message>;

#[derive(Debug)]
pub enum Message {
    Replace(Animation),
    SkipToEnd(Animation),
}

#[derive(Debug, Default)]
pub struct AnimationManager {
    active: Option<ActiveAnimation>,
}

#[derive(Debug)]
struct ActiveAnimation {
    animation: Animation,
    started_at: Instant,
    next_frame: u32,
}

#[derive(Debug)]
pub struct Animation {
    interval: Duration,
    frames: u32,
    easing: AnimationEasing,
    windows: Vec<AnimatedWindow>,
    handled_windows: Vec<WindowId>,
}

#[derive(Debug)]
struct AnimatedWindow {
    handle: AppThreadHandle,
    wid: WindowId,
    start: CGRect,
    finish: CGRect,
    is_focus: bool,
    resizes: bool,
    window_server_id: Option<WindowServerId>,
    proxy: Option<WindowAnimationProxy>,
    txid: TransactionId,
}

impl AnimatedWindow {
    fn frame_after(&self, frame: u32, total_frames: u32, easing: AnimationEasing) -> CGRect {
        if frame == 0 {
            return if self.is_focus {
                CGRect {
                    origin: self.start.origin,
                    size: self.finish.size,
                }
            } else {
                self.start
            };
        }

        let t = f64::from(frame) / f64::from(total_frames);
        let mut rect = get_frame(self.start, self.finish, t, easing);
        if self.is_focus {
            rect.size = self.finish.size;
        }
        rect
    }
}

impl AnimationManager {
    pub fn new() -> Self { Self::default() }

    pub async fn run(mut rx: Receiver) {
        let mut manager = Self::new();
        let mut tick_timer = Timer::manual();

        loop {
            tokio::select! {
                message = rx.recv() => {
                    let Some(message) = message else {
                        manager.finish_active();
                        break;
                    };
                    if let Some(delay) = manager.handle_message(message) {
                        tick_timer.set_next_fire(delay);
                    }
                }
                _ = tick_timer.next(), if manager.active.is_some() => {
                    if let Some(delay) = manager.tick() {
                        tick_timer.set_next_fire(delay);
                    }
                }
            }
        }
    }

    pub fn handle_message(&mut self, message: Message) -> Option<Duration> {
        match message {
            Message::Replace(animation) => {
                self.active = match self.active.take() {
                    Some(active) => Some(active.replace_with(animation)),
                    None => ActiveAnimation::start(animation),
                };
                self.active.as_ref().map(|active| active.animation.interval)
            }
            Message::SkipToEnd(mut animation) => {
                self.finish_active();
                animation.skip_to_end();
                None
            }
        }
    }

    pub fn tick(&mut self) -> Option<Duration> { self.tick_at(Instant::now()) }

    fn tick_at(&mut self, now: Instant) -> Option<Duration> {
        let active = self.active.as_mut()?;
        active.send_next_frame_at(now);
        if active.is_complete() {
            let mut active = self.active.take().expect("animation disappeared while ticking");
            active.animation.end();
            None
        } else {
            Some(active.delay_until_next_frame(now))
        }
    }

    fn finish_active(&mut self) {
        if let Some(active) = self.active.take() {
            active.animation.skip_to_end_and_end();
        }
    }

    pub fn animate_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        is_resize: bool,
        skip_wid: Option<WindowId>,
    ) -> bool {
        let Some(active_ws) = reactor.active_workspace_for_space(space) else {
            return false;
        };
        let mut anim = Animation::new(
            reactor.config.settings.animation_duration,
            reactor.config.settings.animation_fps,
            reactor.config.settings.animation_easing,
        );
        let mut animated_count = 0;
        let mut any_frame_changed = false;

        for &(wid, target_frame) in layout {
            if skip_wid == Some(wid) {
                anim.mark_handled(wid);
                trace!(
                    ?wid,
                    "Skipping animated layout update for window currently being dragged"
                );
                continue;
            }

            let target_frame = target_frame.round();
            let (current_frame, window_server_id, txid) = {
                let registry = reactor.window_manager.as_mut();
                match registry.window_mut(wid) {
                    Some(window) => {
                        let current_frame = window.frame_monotonic;
                        if target_frame.same_as(current_frame) {
                            continue;
                        }
                        let wsid = window.info.sys_id;
                        if let Some(wsid) = wsid {
                            if reactor
                                .transaction_manager
                                .get_target_frame(wsid)
                                .is_some_and(|pending| pending.same_as(target_frame))
                            {
                                trace!(?wid, ?target_frame, "Skipping redundant layout request");
                                continue;
                            }
                        }
                        any_frame_changed = true;
                        let txid = wsid
                            .map(|wsid| reactor.transaction_manager.generate_next_txid(wsid))
                            .unwrap_or_default();
                        (current_frame, wsid, txid)
                    }
                    None => {
                        debug!(?wid, "Skipping - window no longer exists");
                        continue;
                    }
                }
            };

            let Some(app_state) = &reactor.app_manager.apps.get(&wid.pid) else {
                debug!(?wid, "Skipping for window - app no longer exists");
                continue;
            };

            let is_active = reactor.workspace_for_window(wid) == Some(active_ws);

            if is_active {
                trace!(?wid, ?current_frame, ?target_frame, "Animating visible window");
                anim.add_window_with_server_id(
                    &app_state.handle,
                    wid,
                    current_frame,
                    target_frame,
                    false,
                    window_server_id,
                    txid,
                );
                animated_count += 1;
                if let Some(wsid) = window_server_id {
                    reactor.transaction_manager.update_txid_entries([(wsid, txid, target_frame)]);
                }
            } else {
                anim.mark_handled(wid);
                trace!(
                    ?wid,
                    ?current_frame,
                    ?target_frame,
                    "Direct positioning hidden window"
                );
                if let Some(wsid) = window_server_id {
                    reactor.transaction_manager.update_txid_entries([(wsid, txid, target_frame)]);
                }
                if let Err(e) =
                    app_state.handle.send(Request::SetWindowFrame(wid, target_frame, txid, true))
                {
                    debug!(?wid, ?e, "Failed to send frame request for hidden window");
                    continue;
                }
            }

            if let Some(window) = reactor.window_manager.window_mut(wid) {
                window.frame_monotonic = target_frame;
            }
        }

        if animated_count > 0 {
            let low_power = power::is_low_power_mode_enabled();
            let layout_animate = reactor.config.settings.animate;
            let skip_anim = is_resize
                || !layout_animate
                || reactor.config.settings.animation_duration <= 0.0
                || low_power;

            if let Some(tx) = &reactor.animation_tx {
                let message = if skip_anim {
                    Message::SkipToEnd(anim)
                } else {
                    Message::Replace(anim)
                };
                if let Err(err) = tx.send(message) {
                    match err.0 {
                        Message::Replace(mut animation) => animation.skip_to_end(),
                        Message::SkipToEnd(mut animation) => animation.skip_to_end(),
                    }
                }
            } else {
                anim.skip_to_end();
            }
        }

        any_frame_changed
    }

    pub fn instant_layout(
        reactor: &mut Reactor,
        space: SpaceId,
        layout: &[(WindowId, CGRect)],
        skip_wid: Option<WindowId>,
    ) -> bool {
        let mut per_app: HashMap<pid_t, Vec<(WindowId, CGRect)>> = HashMap::default();
        let mut any_frame_changed = false;

        for &(wid, target_frame) in layout {
            if skip_wid == Some(wid) {
                trace!(?wid, "Skipping layout update for window currently being dragged");
                continue;
            }

            let is_hidden = !reactor.is_window_in_active_workspace(space, wid);
            let registry = reactor.window_manager.as_mut();
            let Some(window) = registry.window_mut(wid) else {
                debug!(?wid, "Skipping layout - window no longer exists");
                continue;
            };
            let target_frame = target_frame.round();
            let current_frame = window.frame_monotonic;
            if target_frame.same_as(current_frame) {
                continue;
            }
            if let Some(wsid) = window.info.sys_id {
                if reactor
                    .transaction_manager
                    .get_target_frame(wsid)
                    .is_some_and(|pending| pending.same_as(target_frame))
                {
                    trace!(?wid, ?target_frame, "Skipping redundant instant layout request");
                    continue;
                }
            }
            any_frame_changed = true;
            trace!(
                ?wid,
                ?current_frame,
                ?target_frame,
                hidden = is_hidden,
                "Instant workspace positioning"
            );

            per_app.entry(wid.pid).or_default().push((wid, target_frame));
            window.frame_monotonic = target_frame;
        }

        for (pid, frames) in per_app {
            if frames.is_empty() {
                continue;
            }

            let Some(app_state) = reactor.app_manager.apps.get(&pid) else {
                debug!(?pid, "Skipping layout update for app - app no longer exists");
                continue;
            };

            let handle = app_state.handle.clone();

            let (first_wid, first_target) = frames[0];
            let mut txid = TransactionId::default();
            let mut has_txid = false;
            let mut txid_entries: Vec<(WindowServerId, TransactionId, CGRect)> = Vec::new();
            if let Some(window) = reactor.window_manager.window_mut(first_wid) {
                if let Some(wsid) = window.info.sys_id {
                    txid = reactor.transaction_manager.generate_next_txid(wsid);
                    has_txid = true;
                    txid_entries.push((wsid, txid, first_target));
                }
            }

            if has_txid {
                for (wid, frame) in frames.iter().skip(1) {
                    if let Some(w) = reactor.window_manager.window_mut(*wid)
                        && let Some(wsid) = w.info.sys_id
                    {
                        reactor.transaction_manager.set_last_sent_txid(wsid, txid);
                        txid_entries.push((wsid, txid, *frame));
                    }
                }
                reactor.transaction_manager.update_txid_entries(txid_entries);
            }

            let frames_to_send = frames.clone();
            if let Err(e) = handle.send(Request::SetBatchWindowFrame(frames_to_send, txid, true)) {
                debug!(
                    ?pid,
                    ?e,
                    "Failed to send batch frame request - app may have quit"
                );
                continue;
            }
        }

        any_frame_changed
    }
}

impl ActiveAnimation {
    fn start(animation: Animation) -> Option<Self> { Self::start_at(animation, Instant::now()) }

    fn start_at(mut animation: Animation, started_at: Instant) -> Option<Self> {
        if animation.is_empty() {
            return None;
        }
        animation.begin();
        Some(Self {
            animation,
            started_at,
            next_frame: 1,
        })
    }

    fn replace_with(mut self, mut next: Animation) -> Self {
        let current = self.current_frames();
        let continuing = next.patch_starts_from(&current);
        next.adopt_proxies_from(&mut self.animation);
        next.begin_windows_not_in(&continuing);
        next.carry_over(self.animation, &current);
        Self {
            animation: next,
            started_at: Instant::now(),
            next_frame: 1,
        }
    }

    fn send_next_frame_at(&mut self, now: Instant) {
        let elapsed_frames = now
            .saturating_duration_since(self.started_at)
            .as_secs_f64()
            .div_euclid(self.animation.interval.as_secs_f64())
            .floor() as u32;
        let frame = self.next_frame.max(elapsed_frames).min(self.animation.frames);
        self.animation.send_frame(frame);
        self.next_frame = frame.saturating_add(1);
    }

    fn is_complete(&self) -> bool { self.next_frame > self.animation.frames }

    fn delay_until_next_frame(&self, now: Instant) -> Duration {
        let deadline =
            self.started_at + self.animation.interval.mul_f64(f64::from(self.next_frame));
        deadline.saturating_duration_since(now)
    }

    fn current_frames(&self) -> Vec<(WindowId, CGRect)> {
        let frame = self.next_frame.saturating_sub(1);
        self.animation
            .windows
            .iter()
            .map(|window| {
                (
                    window.wid,
                    window.frame_after(frame, self.animation.frames, self.animation.easing),
                )
            })
            .collect()
    }
}

impl Animation {
    pub fn new(duration: f64, fps: f64, easing: AnimationEasing) -> Self {
        let fps = if fps.is_finite() && fps > 0.0 {
            fps
        } else {
            60.0
        };
        let duration = if duration.is_finite() && duration >= 0.0 {
            duration
        } else {
            0.0
        };
        let interval = Duration::from_secs_f64(1.0 / fps);
        Self {
            interval,
            frames: ((duration * fps).ceil() as u32).max(1),
            easing,
            windows: vec![],
            handled_windows: vec![],
        }
    }

    pub fn add_window(
        &mut self,
        handle: &AppThreadHandle,
        wid: WindowId,
        start: CGRect,
        finish: CGRect,
        is_focus: bool,
        txid: TransactionId,
    ) {
        self.add_window_with_server_id(handle, wid, start, finish, is_focus, None, txid);
    }

    fn add_window_with_server_id(
        &mut self,
        handle: &AppThreadHandle,
        wid: WindowId,
        start: CGRect,
        finish: CGRect,
        is_focus: bool,
        window_server_id: Option<WindowServerId>,
        txid: TransactionId,
    ) {
        self.windows.push(AnimatedWindow {
            handle: handle.clone(),
            wid,
            start,
            finish,
            is_focus,
            resizes: !start.size.same_as(finish.size),
            window_server_id,
            proxy: None,
            txid,
        });
        self.mark_handled(wid);
    }

    fn mark_handled(&mut self, wid: WindowId) {
        if !self.handled_windows.contains(&wid) {
            self.handled_windows.push(wid);
        }
    }

    pub fn skip_to_end(&mut self) {
        for window in &self.windows {
            _ = window.handle.send(Request::SetWindowFrame(
                window.wid,
                window.finish,
                window.txid,
                true,
            ));
        }
    }

    pub fn is_empty(&self) -> bool { self.windows.is_empty() }

    fn begin(&mut self) { self.begin_windows_not_in(&[]); }

    fn begin_windows_not_in(&mut self, skip: &[WindowId]) {
        self.prepare_proxies();
        for window in &mut self.windows {
            if skip.contains(&window.wid) {
                if window.proxy.is_some() {
                    _ = window.handle.send(Request::BeginWindowAnimation(
                        window.wid,
                        Some((window.finish, window.txid)),
                    ));
                }
                continue;
            }
            let target = window.proxy.as_ref().map(|_| (window.finish, window.txid));
            _ = window.handle.send(Request::BeginWindowAnimation(window.wid, target));
            if window.proxy.is_none() && window.is_focus {
                let frame = CGRect {
                    origin: window.start.origin,
                    size: window.finish.size,
                };
                _ = window.handle.send(Request::AnimationFrame {
                    wid: window.wid,
                    frame,
                    set_size: true,
                    txid: window.txid,
                });
            }
        }
    }

    fn prepare_proxies(&mut self) {
        let candidates: Vec<_> = self
            .windows
            .iter()
            .enumerate()
            .filter_map(|(index, window)| {
                (window.resizes && window.proxy.is_none())
                    .then_some(window.window_server_id)
                    .flatten()
                    .map(|id| (index, id, window.start))
            })
            .collect();
        let requests: Vec<_> = candidates.iter().map(|&(_, id, frame)| (id, frame)).collect();
        for ((index, _, _), proxy) in
            candidates.into_iter().zip(WindowAnimationProxy::create_batch(&requests))
        {
            self.windows[index].proxy = proxy;
        }
    }

    fn finish_all(&mut self) {
        for window in &mut self.windows {
            if window.proxy.is_none() {
                _ = window.handle.send(Request::AnimationFrame {
                    wid: window.wid,
                    frame: window.finish,
                    set_size: true,
                    txid: window.txid,
                });
            }
            _ = window.handle.send(Request::EndWindowAnimation(window.wid, window.proxy.take()));
        }
    }

    fn send_frame(&self, frame: u32) {
        let t = f64::from(frame) / f64::from(self.frames);
        let update = self
            .windows
            .iter()
            .any(|window| window.proxy.is_some())
            .then(WindowAnimationUpdate::new);
        for window in &self.windows {
            let mut rect = get_frame(window.start, window.finish, t, self.easing);
            if window.is_focus {
                rect.size = window.finish.size;
            }
            if let (Some(update), Some(proxy)) = (&update, &window.proxy) {
                update.set_frame(proxy, rect);
                continue;
            }
            let set_size = window.resizes && !window.is_focus;
            _ = window.handle.send(Request::AnimationFrame {
                wid: window.wid,
                frame: rect,
                set_size,
                txid: window.txid,
            });
        }
        if let Some(update) = update {
            update.commit();
        }
    }

    fn end(&mut self) { self.finish_all(); }

    fn patch_starts_from(&mut self, current_frames: &[(WindowId, CGRect)]) -> Vec<WindowId> {
        let mut continuing = Vec::new();
        for &(wid, current_frame) in current_frames {
            let Some(window) = self.windows.iter_mut().find(|window| window.wid == wid) else {
                continue;
            };
            window.start = current_frame;
            continuing.push(wid);
        }
        continuing
    }

    fn adopt_proxies_from(&mut self, previous: &mut Animation) {
        for window in &mut self.windows {
            let Some(previous) =
                previous.windows.iter_mut().find(|previous| previous.wid == window.wid)
            else {
                continue;
            };
            window.proxy = previous.proxy.take();
        }
    }

    fn carry_over(&mut self, previous: Animation, current_frames: &[(WindowId, CGRect)]) {
        for mut window in previous.windows {
            if self.handled_windows.contains(&window.wid) {
                if window.proxy.is_some() {
                    _ = window
                        .handle
                        .send(Request::EndWindowAnimation(window.wid, window.proxy.take()));
                }
                continue;
            }
            if self.windows.iter().any(|existing| existing.wid == window.wid) {
                continue;
            }
            if let Some(&(_, current_frame)) =
                current_frames.iter().find(|(wid, _)| *wid == window.wid)
            {
                window.start = current_frame;
            }
            self.windows.push(window);
        }
    }

    fn skip_to_end_and_end(mut self) { self.finish_all(); }
}

fn get_frame(a: CGRect, b: CGRect, t: f64, easing: AnimationEasing) -> CGRect {
    let s = ease(t, easing);
    CGRect {
        origin: CGPoint {
            x: blend(a.origin.x, b.origin.x, s),
            y: blend(a.origin.y, b.origin.y, s),
        },
        size: CGSize {
            width: blend(a.size.width, b.size.width, s),
            height: blend(a.size.height, b.size.height, s),
        },
    }
}

fn ease(t: f64, easing: AnimationEasing) -> f64 {
    use std::f64::consts::PI;

    let t = t.clamp(0.0, 1.0);
    match easing {
        AnimationEasing::EaseInOut | AnimationEasing::EaseInOutCubic => {
            if t < 0.5 {
                4.0 * t.powi(3)
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
        AnimationEasing::Linear => t,
        AnimationEasing::EaseInSine => 1.0 - (t * PI / 2.0).cos(),
        AnimationEasing::EaseOutSine => (t * PI / 2.0).sin(),
        AnimationEasing::EaseInOutSine => -((PI * t).cos() - 1.0) / 2.0,
        AnimationEasing::EaseInQuad => t.powi(2),
        AnimationEasing::EaseOutQuad => 1.0 - (1.0 - t).powi(2),
        AnimationEasing::EaseInOutQuad => {
            if t < 0.5 {
                2.0 * t.powi(2)
            } else {
                1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
            }
        }
        AnimationEasing::EaseInCubic => t.powi(3),
        AnimationEasing::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
        AnimationEasing::EaseInQuart => t.powi(4),
        AnimationEasing::EaseOutQuart => 1.0 - (1.0 - t).powi(4),
        AnimationEasing::EaseInOutQuart => {
            if t < 0.5 {
                8.0 * t.powi(4)
            } else {
                1.0 - (-2.0 * t + 2.0).powi(4) / 2.0
            }
        }
        AnimationEasing::EaseInQuint => t.powi(5),
        AnimationEasing::EaseOutQuint => 1.0 - (1.0 - t).powi(5),
        AnimationEasing::EaseInOutQuint => {
            if t < 0.5 {
                16.0 * t.powi(5)
            } else {
                1.0 - (-2.0 * t + 2.0).powi(5) / 2.0
            }
        }
        AnimationEasing::EaseInExpo => {
            if t == 0.0 {
                0.0
            } else {
                2.0_f64.powf(10.0 * t - 10.0)
            }
        }
        AnimationEasing::EaseOutExpo => {
            if t == 1.0 {
                1.0
            } else {
                1.0 - 2.0_f64.powf(-10.0 * t)
            }
        }
        AnimationEasing::EaseInOutExpo => {
            if t == 0.0 {
                0.0
            } else if t == 1.0 {
                1.0
            } else if t < 0.5 {
                2.0_f64.powf(20.0 * t - 10.0) / 2.0
            } else {
                (2.0 - 2.0_f64.powf(-20.0 * t + 10.0)) / 2.0
            }
        }
        AnimationEasing::EaseInCirc => 1.0 - (1.0 - t.powi(2)).sqrt(),
        AnimationEasing::EaseOutCirc => (1.0 - (t - 1.0).powi(2)).sqrt(),
        AnimationEasing::EaseInOutCirc => {
            if t < 0.5 {
                (1.0 - (1.0 - (2.0 * t).powi(2)).sqrt()) / 2.0
            } else {
                ((1.0 - (-2.0 * t + 2.0).powi(2)).sqrt() + 1.0) / 2.0
            }
        }
    }
}

fn blend(a: f64, b: f64, s: f64) -> f64 { (1.0 - s) * a + s * b }

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn rect(origin_x: f64, origin_y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(origin_x, origin_y), CGSize::new(width, height))
    }

    fn empty_animation() -> Animation { Animation::new(0.3, 100.0, AnimationEasing::EaseInOut) }

    fn animation(handle: &AppThreadHandle, wid: WindowId, from: CGRect, to: CGRect) -> Animation {
        let mut animation = empty_animation();
        animation.add_window(handle, wid, from, to, false, TransactionId::default());
        animation
    }

    fn collect_requests(rx: &mut crate::actor::Receiver<Request>) -> Vec<Request> {
        let mut requests = Vec::new();
        while let Ok((_, request)) = rx.try_recv() {
            requests.push(request);
        }
        requests
    }

    fn assert_set_window_frame(request: &Request, wid: WindowId, frame: CGRect) {
        match request {
            Request::SetWindowFrame(req_wid, req_frame, txid, eui) => {
                assert_eq!(*req_wid, wid);
                assert_eq!(*req_frame, frame);
                assert_eq!(*txid, TransactionId::default());
                assert!(*eui);
            }
            _ => panic!("expected SetWindowFrame, got {request:?}"),
        }
    }

    fn assert_animation_frame(request: &Request, wid: WindowId, frame: CGRect) {
        match request {
            Request::AnimationFrame {
                wid: req_wid,
                frame: req_frame,
                set_size,
                txid,
            } => {
                assert_eq!(*req_wid, wid);
                assert_eq!(*req_frame, frame);
                assert!(*set_size, "expected a set_size frame");
                assert_eq!(*txid, TransactionId::default());
            }
            _ => panic!("expected AnimationFrame, got {request:?}"),
        }
    }

    fn assert_animation_pos(request: &Request, wid: WindowId, pos: CGPoint) {
        match request {
            Request::AnimationFrame {
                wid: req_wid,
                frame,
                set_size,
                txid,
            } => {
                assert_eq!(*req_wid, wid);
                assert_eq!(frame.origin, pos);
                assert!(!*set_size, "expected a position-only frame");
                assert_eq!(*txid, TransactionId::default());
            }
            _ => panic!("expected AnimationFrame, got {request:?}"),
        }
    }

    #[test]
    fn animation_uses_configured_timing_and_resizes_every_frame() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let mut animation = Animation::new(0.15, 60.0, AnimationEasing::EaseOutCubic);
        assert_eq!(animation.interval, Duration::from_secs_f64(1.0 / 60.0));
        assert_eq!(animation.frames, 9);
        animation.add_window(
            &handle,
            wid,
            rect(0.0, 0.0, 100.0, 100.0),
            rect(50.0, 60.0, 200.0, 200.0),
            false,
            TransactionId::default(),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(animation));
        let _ = collect_requests(&mut rx);
        manager.tick();
        let requests = collect_requests(&mut rx);
        let Request::AnimationFrame { frame, set_size, .. } = &requests[0] else {
            panic!("expected animation frame")
        };
        assert!(*set_size);
        assert!(frame.size.width > 100.0 && frame.size.width < 200.0);
    }

    #[test]
    fn position_only_animation_avoids_expensive_size_writes() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let animation = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 100.0, 100.0),
            rect(400.0, 300.0, 100.0, 100.0),
        );
        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(animation));
        let _ = collect_requests(&mut rx);

        manager.tick();
        assert!(matches!(collect_requests(&mut rx).as_slice(), [
            Request::AnimationFrame { set_size: false, .. }
        ]));
    }

    #[test]
    fn delayed_tick_catches_up_to_wall_clock_progress() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let mut animation = Animation::new(0.2, 60.0, AnimationEasing::Linear);
        animation.add_window(
            &handle,
            wid,
            rect(0.0, 0.0, 100.0, 100.0),
            rect(120.0, 0.0, 100.0, 100.0),
            false,
            TransactionId::default(),
        );
        let started_at = Instant::now();
        let mut manager = AnimationManager {
            active: ActiveAnimation::start_at(animation, started_at),
        };
        let _ = collect_requests(&mut rx);

        manager.tick_at(started_at + Duration::from_millis(70));
        let requests = collect_requests(&mut rx);
        let Request::AnimationFrame { frame, .. } = &requests[0] else {
            panic!("expected animation frame")
        };
        assert_eq!(frame.origin.x, 40.0);
        assert_eq!(manager.active.as_ref().unwrap().next_frame, 5);
    }

    #[test]
    fn all_easing_curves_preserve_endpoints_and_move_monotonically() {
        use AnimationEasing::*;
        for easing in [
            EaseInOut,
            Linear,
            EaseInSine,
            EaseOutSine,
            EaseInOutSine,
            EaseInQuad,
            EaseOutQuad,
            EaseInOutQuad,
            EaseInCubic,
            EaseOutCubic,
            EaseInOutCubic,
            EaseInQuart,
            EaseOutQuart,
            EaseInOutQuart,
            EaseInQuint,
            EaseOutQuint,
            EaseInOutQuint,
            EaseInExpo,
            EaseOutExpo,
            EaseInOutExpo,
            EaseInCirc,
            EaseOutCirc,
            EaseInOutCirc,
        ] {
            assert!((ease(0.0, easing) - 0.0).abs() < 1e-9, "{easing:?}");
            assert!((ease(1.0, easing) - 1.0).abs() < 1e-9, "{easing:?}");
            let values =
                (0..=20).map(|step| ease(f64::from(step) / 20.0, easing)).collect::<Vec<_>>();
            assert!(values.windows(2).all(|pair| pair[0] <= pair[1]), "{easing:?}");
        }
    }

    #[test]
    fn replacement_uses_last_animated_frame_for_continuing_windows() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let first = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
        );
        let second = animation(
            &handle,
            wid,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        assert!(matches!(
            collect_requests(&mut rx).as_slice(),
            [Request::BeginWindowAnimation(req_wid, None)] if *req_wid == wid
        ));

        manager.tick();
        let continuing_frame = manager.active.as_ref().unwrap().current_frames()[0].1;
        assert_animation_pos(&collect_requests(&mut rx)[0], wid, continuing_frame.origin);

        manager.handle_message(Message::Replace(second));
        assert!(collect_requests(&mut rx).is_empty());

        let resumed_start = manager.active.as_ref().unwrap().animation.windows[0].start;
        assert_eq!(resumed_start, continuing_frame);

        manager.tick();
        let expected_next = get_frame(
            resumed_start,
            rect(80.0, 90.0, 10.0, 10.0),
            1.0 / 30.0,
            AnimationEasing::EaseInOut,
        );
        assert_animation_pos(&collect_requests(&mut rx)[0], wid, expected_next.origin);
    }

    fn animation_contains(manager: &AnimationManager, wid: WindowId) -> bool {
        manager
            .active
            .as_ref()
            .is_some_and(|active| active.animation.windows.iter().any(|w| w.wid == wid))
    }

    #[test]
    fn replacement_only_restarts_changed_windows() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid1 = WindowId::new(1, 1);
        let wid2 = WindowId::new(1, 2);
        let wid3 = WindowId::new(1, 3);
        let mut first = empty_animation();
        first.add_window(
            &handle,
            wid1,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        first.add_window(
            &handle,
            wid2,
            rect(10.0, 0.0, 10.0, 10.0),
            rect(60.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        let mut second = empty_animation();
        second.add_window(
            &handle,
            wid1,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        second.add_window(
            &handle,
            wid3,
            rect(20.0, 0.0, 10.0, 10.0),
            rect(90.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        assert_eq!(collect_requests(&mut rx).len(), 2);
        manager.handle_message(Message::Replace(second));

        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 1);
        assert!(
            matches!(requests[0], Request::BeginWindowAnimation(req_wid, None) if req_wid == wid3)
        );
        assert!(animation_contains(&manager, wid2));

        let carried = manager
            .active
            .as_ref()
            .unwrap()
            .animation
            .windows
            .iter()
            .find(|w| w.wid == wid2)
            .unwrap();
        assert_eq!(carried.finish, rect(60.0, 60.0, 10.0, 10.0));
    }

    #[test]
    fn replacement_does_not_carry_over_explicitly_handled_windows() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid1 = WindowId::new(1, 1);
        let wid2 = WindowId::new(1, 2);
        let mut first = empty_animation();
        first.add_window(
            &handle,
            wid1,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        first.add_window(
            &handle,
            wid2,
            rect(10.0, 0.0, 10.0, 10.0),
            rect(60.0, 60.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        let mut second = empty_animation();
        second.add_window(
            &handle,
            wid1,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
            false,
            TransactionId::default(),
        );
        second.mark_handled(wid2);

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        let _ = collect_requests(&mut rx);
        manager.handle_message(Message::Replace(second));

        assert!(!animation_contains(&manager, wid2));
    }

    #[test]
    fn skip_to_end_finishes_active_animation_and_applies_new_layout() {
        let (tx, mut rx) = crate::actor::channel();
        let handle = AppThreadHandle::new_for_test(tx);
        let wid = WindowId::new(1, 1);
        let first = animation(
            &handle,
            wid,
            rect(0.0, 0.0, 10.0, 10.0),
            rect(50.0, 60.0, 10.0, 10.0),
        );
        let second = animation(
            &handle,
            wid,
            rect(50.0, 60.0, 10.0, 10.0),
            rect(80.0, 90.0, 10.0, 10.0),
        );

        let mut manager = AnimationManager::new();
        manager.handle_message(Message::Replace(first));
        manager.handle_message(Message::SkipToEnd(second));

        let requests = collect_requests(&mut rx);
        assert_eq!(requests.len(), 4);
        assert!(
            matches!(requests[0], Request::BeginWindowAnimation(req_wid, None) if req_wid == wid)
        );
        assert_animation_frame(&requests[1], wid, rect(50.0, 60.0, 10.0, 10.0));
        assert!(
            matches!(requests[2], Request::EndWindowAnimation(req_wid, None) if req_wid == wid)
        );
        assert_set_window_frame(&requests[3], wid, rect(80.0, 90.0, 10.0, 10.0));
    }
}
