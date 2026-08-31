use std::time::{Duration, Instant};

use super::Event;
use crate::actor::app::{Quiet, WindowId, pid_t};
use crate::common::collections::HashMap;

const ACTIVATION_SETTLE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct MainWindowTracker {
    apps: HashMap<pid_t, AppState>,
    global_frontmost: Option<pid_t>,
    last_clicked_window: Option<(WindowId, Instant)>,
}

struct AppState {
    is_frontmost: bool,
    frontmost_is_quiet: Quiet,
    main_window: Option<WindowId>,
    deactivated_main_window: Option<WindowId>,
    activation_preference: Option<(WindowId, Instant)>,
}

impl MainWindowTracker {
    pub(super) fn handle_mouse_down(&mut self, window: Option<WindowId>) {
        let now = Instant::now();
        self.last_clicked_window = window.map(|window| (window, now));

        let Some(window) = window else { return };
        if self.global_frontmost != Some(window.pid) {
            return;
        }
        let Some(app) = self.apps.get_mut(&window.pid) else {
            return;
        };
        // A real click is stronger evidence than the activation preference. In
        // particular, it lets the user immediately choose another window while
        // an app is still inside the short activation settling interval.
        app.main_window = Some(window);
        app.activation_preference = Some((window, now));
    }

    #[must_use]
    pub fn handle_event(&mut self, event: &Event) -> Option<WindowId> {
        let (event_pid, quiet_edge) = match event {
            &Event::ApplicationLaunched {
                pid, is_frontmost, main_window, ..
            } => {
                self.apps.insert(pid, AppState {
                    is_frontmost,
                    frontmost_is_quiet: Quiet::No,
                    main_window,
                    deactivated_main_window: None,
                    activation_preference: None,
                });
                (pid, Quiet::No)
            }
            &Event::ApplicationThreadTerminated(pid) => {
                self.apps.remove(&pid);
                return None;
            }
            &Event::WindowDestroyed(window) => {
                if let Some(app) = self.apps.get_mut(&window.pid) {
                    if app.main_window == Some(window) {
                        app.main_window = None;
                    }
                    if app.deactivated_main_window == Some(window) {
                        app.deactivated_main_window = None;
                    }
                    if app.activation_preference.is_some_and(|(wid, _)| wid == window) {
                        app.activation_preference = None;
                    }
                }
                return None;
            }
            &Event::ApplicationActivated(pid, quiet) => {
                let preferred = self.begin_activation(pid)?;
                let app = self.apps.get_mut(&pid)?;
                app.is_frontmost = true;
                app.frontmost_is_quiet = quiet;
                app.main_window = preferred;
                (pid, quiet)
            }
            &Event::ApplicationDeactivated(pid) => {
                let app = self.apps.get_mut(&pid)?;
                app.deactivated_main_window = app.main_window;
                app.activation_preference = None;
                app.is_frontmost = false;
                return None;
            }
            &Event::ApplicationGloballyActivated(pid) => {
                self.global_frontmost = Some(pid);
                let preferred = self.begin_activation(pid)?;
                let Some(app) = self.apps.get_mut(&pid) else {
                    return None;
                };
                app.is_frontmost = true;
                app.main_window = preferred;
                (pid, app.frontmost_is_quiet)
            }
            &Event::ApplicationGloballyDeactivated(pid) => {
                if self.global_frontmost == Some(pid) {
                    self.global_frontmost = None;
                }
                if let Some(app) = self.apps.get_mut(&pid) {
                    app.deactivated_main_window = app.main_window;
                    app.activation_preference = None;
                    app.is_frontmost = false;
                }
                return None;
            }
            &Event::ApplicationMainWindowChanged(pid, wid, quiet) => {
                let app = self.apps.get_mut(&pid)?;
                let preferred = app
                    .activation_preference
                    .filter(|(_, started)| started.elapsed() < ACTIVATION_SETTLE_TIMEOUT)
                    .map(|(window, _)| window);
                if quiet == Quiet::No && app.is_frontmost && preferred.is_some() && wid != preferred
                {
                    // Chrome and a few other apps can briefly expose the previous
                    // AXMainWindow while handling an external open request. Raising
                    // it here steals focus from the window the app selected for the
                    // URL. Keep the pre-deactivation (or directly clicked) window
                    // until activation has settled. Quiet changes come from Lift's
                    // own raise path, so they are authoritative window selections.
                    app.main_window = preferred;
                } else {
                    app.activation_preference = None;
                    app.main_window = wid;
                }
                (pid, quiet)
            }
            _ => return None,
        };
        if Some(event_pid) == self.global_frontmost && quiet_edge == Quiet::No {
            if let Some(wid) = self.main_window() {
                return Some(wid);
            }
        }
        None
    }

    fn begin_activation(&mut self, pid: pid_t) -> Option<Option<WindowId>> {
        let now = Instant::now();
        let clicked_window = self
            .last_clicked_window
            .take()
            .filter(|(window, clicked)| {
                window.pid == pid && clicked.elapsed() < ACTIVATION_SETTLE_TIMEOUT
            })
            .map(|(window, _)| window);
        let app = self.apps.get_mut(&pid)?;
        let stable_preference = app
            .activation_preference
            .filter(|(_, started)| started.elapsed() < ACTIVATION_SETTLE_TIMEOUT)
            .map(|(window, _)| window)
            .or(clicked_window)
            .or(app.deactivated_main_window);
        let preferred = stable_preference.or(app.main_window);
        app.deactivated_main_window = None;
        app.activation_preference = stable_preference.map(|window| (window, now));
        Some(preferred)
    }

    pub fn main_window(&self) -> Option<WindowId> {
        let pid = self.global_frontmost?;
        match self.apps.get(&pid) {
            Some(&AppState {
                is_frontmost: true,
                main_window: Some(window),
                ..
            }) => Some(window),
            _ => None,
        }
    }

    pub(super) fn main_window_for_pid(&self, pid: pid_t) -> Option<WindowId> {
        self.apps.get(&pid)?.main_window
    }
}
