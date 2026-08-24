use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::ids::DisplayId;
use super::rules::WindowRule;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoreConfig {
    pub focus_follows_mouse: bool,
    pub mouse_follows_focus: bool,
    pub mouse_hides_on_focus: bool,
    pub drag_swap_fraction: f64,
    pub auto_destroy_empty_workspaces: bool,
    pub display_migration_priority: Vec<DisplayId>,
    pub window_rules: Vec<WindowRule>,
    pub layout: LayoutConfig,
    pub animation: AnimationConfig,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            focus_follows_mouse: false,
            mouse_follows_focus: false,
            mouse_hides_on_focus: false,
            drag_swap_fraction: 0.3,
            auto_destroy_empty_workspaces: true,
            display_migration_priority: Vec::new(),
            window_rules: Vec::new(),
            layout: LayoutConfig::default(),
            animation: AnimationConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutConfig {
    pub gaps: Gaps,
    pub gaps_by_display: BTreeMap<DisplayId, Gaps>,
}

impl LayoutConfig {
    pub fn gaps_for(&self, display: &DisplayId) -> Gaps {
        self.gaps_by_display.get(display).copied().unwrap_or(self.gaps)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Gaps {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub horizontal: f64,
    pub vertical: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimationConfig {
    pub enabled: bool,
    pub duration_seconds: f64,
    pub frames_per_second: f64,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_seconds: 0.2,
            frames_per_second: 120.0,
        }
    }
}
