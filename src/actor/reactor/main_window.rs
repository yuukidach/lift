use super::Event;
use crate::actor::app::{Quiet, WindowId, pid_t};
use crate::common::collections::HashMap;

#[derive(Default)]
pub(crate) struct MainWindowTracker {
    apps: HashMap<pid_t, AppState>,
    global_frontmost: Option<pid_t>,
}

struct AppState {
    is_frontmost: bool,
    frontmost_is_quiet: Quiet,
    main_window: Option<WindowId>,
}

impl MainWindowTracker {
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
                });
                (pid, Quiet::No)
            }
            &Event::ApplicationThreadTerminated(pid) => {
                self.apps.remove(&pid);
                return None;
            }
            &Event::WindowDestroyed(window) => {
                if let Some(app) = self.apps.get_mut(&window.pid)
                    && app.main_window == Some(window)
                {
                    app.main_window = None;
                }
                return None;
            }
            &Event::ApplicationActivated(pid, quiet) => {
                let app = self.apps.get_mut(&pid)?;
                app.is_frontmost = true;
                app.frontmost_is_quiet = quiet;
                (pid, quiet)
            }
            &Event::ApplicationDeactivated(pid) => {
                let app = self.apps.get_mut(&pid)?;
                app.is_frontmost = false;
                return None;
            }
            &Event::ApplicationGloballyActivated(pid) => {
                self.global_frontmost = Some(pid);
                let Some(app) = self.apps.get_mut(&pid) else {
                    return None;
                };
                app.is_frontmost = true;
                (pid, app.frontmost_is_quiet)
            }
            &Event::ApplicationGloballyDeactivated(pid) => {
                if self.global_frontmost == Some(pid) {
                    self.global_frontmost = None;
                }
                if let Some(app) = self.apps.get_mut(&pid) {
                    app.is_frontmost = false;
                }
                return None;
            }
            &Event::ApplicationMainWindowChanged(pid, wid, quiet) => {
                let app = self.apps.get_mut(&pid)?;
                app.main_window = wid;
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
}
