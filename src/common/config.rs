use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::bail;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::collections::HashMap;
use crate::actor::wm_controller::WmCommand;
use crate::core::ids::WORKSPACE_SLOTS;
use crate::sys::hotkey::{Hotkey, HotkeySpec};

const MAX_WORKSPACES: usize = WORKSPACE_SLOTS;

// Used only to make unknown-command errors point to the current spelling.
const DEPRECATED_MAP: &[(&str, &str)] = &[
    ("stack_windows", "toggle_stack"),
    ("unstack_windows", "toggle_stack"),
    ("toggle_tile_orientation", "toggle_orientation"),
];

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ConfigCommand {
    SetAnimate(bool),
    SetAnimationDuration(f64),
    SetAnimationFps(f64),
    SetAnimationEasing(AnimationEasing),

    SetMouseFollowsFocus(bool),
    SetMouseHidesOnFocus(bool),
    SetFocusFollowsMouse(bool),

    SetOuterGaps {
        top: f64,
        left: f64,
        bottom: f64,
        right: f64,
    },
    SetInnerGaps {
        horizontal: f64,
        vertical: f64,
    },

    SetWorkspaceNames(Vec<String>),

    /// Generic setter for arbitrary config paths using dot-separated keys.
    /// Example: key = "settings.animate", value = true
    Set {
        key: String,
        value: Value,
    },

    GetConfig,
    SaveConfig,
    ReloadConfig,
}

pub fn data_dir() -> PathBuf { data_dir_for(&dirs::home_dir().unwrap()) }
fn data_dir_for(home: &Path) -> PathBuf { home.join(".lift") }
pub fn restore_file() -> PathBuf { data_dir().join("layout.ron") }
pub fn diagnostics_file() -> PathBuf { data_dir().join("diagnostics").join("operations.jsonl") }
pub fn config_file() -> PathBuf { config_file_for(&dirs::home_dir().unwrap()) }
fn config_file_for(home: &Path) -> PathBuf { home.join(".config").join("lift").join("config.toml") }

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct VirtualWorkspaceSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "yes")]
    pub auto_assign_windows: bool,
    #[serde(default = "yes")]
    pub preserve_focus_per_workspace: bool,
    #[serde(default = "no")]
    pub workspace_auto_back_and_forth: bool,
    #[serde(default)]
    pub reapply_app_rules_on_title_change: bool,
    #[serde(default)]
    pub app_rules: Vec<AppWorkspaceRule>,
    /// Per-display default workspace number assigned at lazy init. Example:
    ///   display_default_workspaces = { "30999A24-..." = 1 }
    /// Means "when this display's space gets its first workspace, give it
    /// number 1". Mid-session ws creation is not constrained by this map.
    #[serde(default)]
    pub display_default_workspaces: HashMap<String, usize>,
    /// Stable display UUIDs ordered from left to right for workspace UI.
    /// Unlisted displays follow their observed physical position.
    #[serde(default)]
    pub display_order: Vec<String>,
    /// Ordered display UUIDs used as receivers when another display is removed.
    #[serde(default)]
    pub display_migration_priority: Vec<String>,
}

// Allow specifying a workspace by numeric index or by name in the config.
// This supports both `workspace = 2` and `workspace = "coding"` in app rules.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Eq)]
#[serde(untagged)]
pub enum WorkspaceSelector {
    Index(usize),
    Name(String),
}

/// Converts the digit-row workspace number (1..=9, then 0) to its global slot.
pub fn workspace_number_to_global_slot(number: usize) -> Option<usize> {
    match number {
        1..=9 => Some(number - 1),
        0 => Some(9),
        _ => None,
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct AppWorkspaceRule {
    /// Application bundle identifier (e.g., "com.apple.Terminal")
    pub app_id: Option<String>,
    /// Initial workspace digit (0 through 9) or workspace name. Existing windows keep their
    /// current workspace, so an explicit move is not overwritten by later observations.
    pub workspace: Option<WorkspaceSelector>,
    /// Whether windows should be floating in this workspace
    #[serde(default)]
    pub floating: bool,
    /// Whether Lift should manage matching windows (defaults to true). `false` makes the
    /// window invisible to Lift (no tiling, floating, or assignments).
    #[serde(default = "yes")]
    pub manage: bool,
    /// Optional: Application name pattern (alternative to app_id)
    pub app_name: Option<String>,
    /// Optional: Regular expression to match window title (applies to window.title)
    ///
    /// If present, this regex will be used when attempting to match a window by
    /// title.
    pub title_regex: Option<String>,
    /// Optional: Substring to search for in window title (applies to window.title)
    ///
    /// If present, Lift will internally treat this as a substring match and will
    /// construct a regex to match titles containing this substring. This allows
    /// people who don't want to write full regexes to match by a simple substring.
    pub title_substring: Option<String>,

    /// Optional: Accessibility role to match (AXRole). If present, it must be a
    /// non-empty string and will be compared against the accessibility role
    /// reported by the AX APIs for a window (exact string match).
    pub ax_role: Option<String>,

    /// Optional: Accessibility subrole to match (AXSubrole). If present, it must be a
    /// non-empty string and will be compared against the accessibility subrole
    /// reported by the AX APIs for a window (exact string match).
    pub ax_subrole: Option<String>,
}

impl Default for VirtualWorkspaceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_assign_windows: true,
            preserve_focus_per_workspace: true,
            workspace_auto_back_and_forth: false,
            reapply_app_rules_on_title_change: false,
            app_rules: Vec::new(),
            display_default_workspaces: HashMap::default(),
            display_order: Vec::new(),
            display_migration_priority: Vec::new(),
        }
    }
}

impl VirtualWorkspaceSettings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        // Reject duplicate display UUIDs and out-of-range workspace numbers.
        let mut seen_uuids = crate::common::collections::HashSet::default();
        for (uuid, number) in &self.display_default_workspaces {
            if uuid.trim().is_empty() {
                issues.push(format!(
                    "display_default_workspaces has entry with empty display UUID (-> {})",
                    number
                ));
            } else if !seen_uuids.insert(uuid.clone()) {
                issues.push(format!(
                    "display_default_workspaces UUID '{}' has multiple defaults",
                    uuid
                ));
            }
            if *number >= MAX_WORKSPACES {
                issues.push(format!(
                    "display_default_workspaces[{}] = {} is outside 0..=9",
                    uuid, number
                ));
            }
        }

        let mut seen_display_order_uuids = crate::common::collections::HashSet::default();
        for uuid in &self.display_order {
            if uuid.trim().is_empty() {
                issues.push("display_order has empty display UUID".to_string());
            } else if !seen_display_order_uuids.insert(uuid.clone()) {
                issues.push(format!("display_order has duplicate display UUID '{}'", uuid));
            }
        }

        let mut seen_migration_uuids = crate::common::collections::HashSet::default();
        for uuid in &self.display_migration_priority {
            if uuid.trim().is_empty() {
                issues.push("display_migration_priority has empty display UUID".to_string());
            } else if !seen_migration_uuids.insert(uuid.clone()) {
                issues.push(format!(
                    "display_migration_priority has duplicate display UUID '{}'",
                    uuid
                ));
            }
        }

        // Validate rules and check duplicates in a single pass
        let mut seen_app_ids = crate::common::collections::HashSet::default();
        let mut seen_app_names = crate::common::collections::HashSet::default();
        let mut seen_title_regexes = crate::common::collections::HashSet::default();
        let mut seen_title_substrings = crate::common::collections::HashSet::default();
        let mut seen_ax_roles = crate::common::collections::HashSet::default();
        let mut seen_ax_subroles = crate::common::collections::HashSet::default();

        for (index, rule) in self.app_rules.iter().enumerate() {
            let app_id_empty = rule.app_id.as_ref().map_or(true, |id| id.is_empty());
            if app_id_empty
                && rule.app_name.is_none()
                && rule.title_regex.is_none()
                && rule.title_substring.is_none()
                && rule.ax_role.is_none()
                && rule.ax_subrole.is_none()
            {
                issues.push(format!(
                    "App rule {} has no app_id, app_name, title_regex, or title_substring specified",
                    index
                ));
            }

            if let Some(ref workspace) = rule.workspace {
                if let WorkspaceSelector::Index(idx) = workspace {
                    if *idx >= MAX_WORKSPACES {
                        issues.push(format!(
                            "App rule {} references workspace {} which is outside 0..=9",
                            index, idx
                        ));
                    }
                }
            }

            if let Some(ref app_id) = rule.app_id {
                if !app_id.is_empty() && !app_id.contains('.') {
                    issues.push(format!(
                        "App rule {} has suspicious app_id '{}' (should be bundle identifier like 'com.example.app')",
                        index, app_id
                    ));
                }

                let has_specific_match = rule.app_name.is_some()
                    || rule.title_regex.is_some()
                    || rule.title_substring.is_some()
                    || rule.ax_role.is_some()
                    || rule.ax_subrole.is_some();
                if !app_id.is_empty() && !has_specific_match && !seen_app_ids.insert(app_id) {
                    issues.push(format!("Duplicate app_id '{}' in rule {}", app_id, index));
                }
            }

            if let Some(ref app_name) = rule.app_name {
                if !seen_app_names.insert(app_name) {
                    issues.push(format!("Duplicate app_name '{}' in rule {}", app_name, index));
                }
            }

            if let Some(ref title_re) = rule.title_regex {
                if title_re.is_empty() {
                    issues.push(format!("App rule {} has empty title_regex", index));
                } else if !seen_title_regexes.insert(title_re) {
                    issues.push(format!("Duplicate title_regex '{}' in rule {}", title_re, index));
                }
            }

            if let Some(ref title_sub) = rule.title_substring {
                if title_sub.is_empty() {
                    issues.push(format!("App rule {} has empty title_substring", index));
                } else if !seen_title_substrings.insert(title_sub) {
                    issues.push(format!(
                        "Duplicate title_substring '{}' in rule {}",
                        title_sub, index
                    ));
                }
            }

            if let Some(ref ax_role) = rule.ax_role {
                if ax_role.is_empty() {
                    issues.push(format!("App rule {} has empty ax_role", index));
                } else if !seen_ax_roles.insert(ax_role) {
                    issues.push(format!("Duplicate ax_role '{}' in rule {}", ax_role, index));
                }
            }

            if let Some(ref ax_sub) = rule.ax_subrole {
                if ax_sub.is_empty() {
                    issues.push(format!("App rule {} has empty ax_subrole", index));
                } else if !seen_ax_subroles.insert(ax_sub) {
                    issues.push(format!("Duplicate ax_subrole '{}' in rule {}", ax_sub, index));
                }
            }
        }

        issues
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    settings: Settings,
    keys: HashMap<String, WmCommand>,
    #[serde(default)]
    virtual_workspaces: VirtualWorkspaceSettings,
    /// Modifier combinations that can be reused in key bindings
    /// e.g., "comb1" = "Alt + Shift" allows using "comb1 + C" in keys
    #[serde(default)]
    modifier_combinations: HashMap<String, String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Config {
    pub settings: Settings,
    pub keys: Vec<(Hotkey, WmCommand)>,
    #[serde(default)]
    pub key_specs: Vec<(String, WmCommand)>,
    pub virtual_workspaces: VirtualWorkspaceSettings,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Config, D::Error>
    where D: serde::Deserializer<'de> {
        #[derive(Deserialize)]
        struct ConfigSerde {
            settings: Settings,
            keys: Vec<(Hotkey, WmCommand)>,
            #[serde(default)]
            key_specs: Vec<(String, WmCommand)>,
            virtual_workspaces: VirtualWorkspaceSettings,
        }

        let config = ConfigSerde::deserialize(deserializer)?;
        let key_specs = if config.key_specs.is_empty() && !config.keys.is_empty() {
            config
                .keys
                .iter()
                .map(|(hotkey, command)| (hotkey.to_string(), command.clone()))
                .collect()
        } else {
            config.key_specs
        };

        Ok(Config {
            settings: config.settings,
            keys: config.keys,
            key_specs,
            virtual_workspaces: config.virtual_workspaces,
        })
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "no")]
    pub animate: bool,
    #[serde(default = "default_animation_duration")]
    pub animation_duration: f64,
    #[serde(default = "default_animation_fps")]
    pub animation_fps: f64,
    #[serde(default)]
    pub animation_easing: AnimationEasing,
    #[serde(default = "yes")]
    pub default_disable: bool,
    #[serde(default = "yes")]
    pub mouse_follows_focus: bool,
    #[serde(default = "yes")]
    pub mouse_hides_on_focus: bool,
    #[serde(default = "yes")]
    pub focus_follows_mouse: bool,
    /// Bounded operation/state log used to reproduce interaction bugs.
    #[serde(default)]
    pub diagnostics: DiagnosticsSettings,
    /// Hotkey that disables focus-follows-mouse while held.
    /// Accepts either a full hotkey (e.g. "Ctrl + A") or a modifier-only spec (e.g. "Ctrl")
    #[serde(default)]
    pub focus_follows_mouse_disable_hotkey: Option<HotkeySpec>,
    /// Apps that should not trigger automatic workspace switching when activated.
    /// List of bundle identifiers (e.g., "com.apple.Spotlight") that often
    /// inappropriately steal focus and shouldn't cause workspace switches.
    #[serde(default)]
    pub auto_focus_blacklist: Vec<String>,
    #[serde(default)]
    pub layout: LayoutSettings,
    #[serde(default)]
    pub ui: UiSettings,
    /// Trackpad gesture settings
    #[serde(default)]
    pub gestures: GestureSettings,

    #[serde(default)]
    pub window_snapping: WindowSnappingSettings,

    /// Commands to run on startup (e.g., for subscribing to events)
    #[serde(default)]
    pub run_on_start: Vec<String>,

    /// Whether to reapply app rules when a window title changes.
    /// Enable hot-reloading of the config file when it changes
    #[serde(default = "yes")]
    pub hot_reload: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_diagnostic_file_size_mb")]
    pub max_file_size_mb: u64,
    #[serde(default = "default_diagnostic_retained_files")]
    pub retained_files: usize,
}

impl Default for DiagnosticsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_size_mb: default_diagnostic_file_size_mb(),
            retained_files: default_diagnostic_retained_files(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AnimationEasing {
    #[default]
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
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct UiSettings {
    #[serde(default)]
    pub menu_bar: MenuBarSettings,
    #[serde(default)]
    pub stack_line: StackLineSettings,
    #[serde(default)]
    pub mission_control: MissionControlSettings,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct GestureSettings {
    /// Enable horizontal swipes to switch virtual workspaces
    #[serde(default = "no")]
    pub enabled: bool,
    /// If true, consume low-level dock swipe events to prevent macOS from also handling them
    #[serde(default = "yes")]
    pub consume_dock_swipe: bool,
    /// Invert horizontal direction (swap next/prev)
    #[serde(default)]
    pub invert_horizontal_swipe: bool,
    /// Maximum absolute Y delta allowed for the gesture to count as horizontal
    #[serde(default = "default_swipe_vertical_tolerance")]
    pub swipe_vertical_tolerance: f64,
    /// If true, attempt to skip empty workspaces on swipe (if supported)
    #[serde(default)]
    pub skip_empty: bool,
    /// Number of fingers required for swipe (default = 3)
    #[serde(default = "default_swipe_fingers")]
    pub fingers: usize,
    /// Normalized horizontal distance (0..1) required to fire a swipe
    #[serde(default = "default_distance_pct")]
    pub distance_pct: f64,
    /// Enable haptic feedback when a swipe commits
    #[serde(default = "yes")]
    pub haptics_enabled: bool,
    /// Haptic feedback pattern (generic | alignment | level_change)
    #[serde(default)]
    pub haptic_pattern: HapticPattern,
}

impl Default for GestureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            consume_dock_swipe: true,
            invert_horizontal_swipe: false,
            swipe_vertical_tolerance: default_swipe_vertical_tolerance(),
            skip_empty: true,
            fingers: default_swipe_fingers(),
            distance_pct: default_distance_pct(),
            haptics_enabled: true,
            haptic_pattern: HapticPattern::LevelChange,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default, Copy)]
#[serde(deny_unknown_fields)]
pub struct WindowSnappingSettings {
    #[serde(default = "default_drag_swap_fraction")]
    pub drag_swap_fraction: f64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum MenuBarDisplayMode {
    #[default]
    All,
    Active,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum ActiveWorkspaceLabel {
    #[default]
    Index,
    Name,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceDisplayStyle {
    #[default]
    Layout,
    Label,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum MenuBarWorkspaceScope {
    #[default]
    PerDisplay,
    Global,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct MenuBarSettings {
    #[serde(default = "no")]
    pub enabled: bool,
    #[serde(default = "no")]
    pub show_empty: bool,
    #[serde(default)]
    pub mode: MenuBarDisplayMode,
    #[serde(default)]
    pub active_label: ActiveWorkspaceLabel,
    #[serde(default)]
    pub display_style: WorkspaceDisplayStyle,
    #[serde(default)]
    pub workspace_scope: MenuBarWorkspaceScope,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct StackLineSettings {
    #[serde(default = "no")]
    pub enabled: bool,
    #[serde(default = "default_stack_line_thickness")]
    pub thickness: f64,
    #[serde(default)]
    pub horiz_placement: HorizontalPlacement,
    #[serde(default)]
    pub vert_placement: VerticalPlacement,
    /// Distance to position the stack line away from the window edge (in points)
    /// This creates spacing between the window and the stack line
    #[serde(default = "default_stack_line_spacing")]
    pub spacing: f64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct MissionControlSettings {
    #[serde(default = "no")]
    pub enabled: bool,
    #[serde(default = "no")]
    pub fade_enabled: bool,
    #[serde(default = "default_mission_control_fade_duration_ms")]
    pub fade_duration_ms: f64,
}

fn default_mission_control_fade_duration_ms() -> f64 { 180.0 }

fn default_drag_swap_fraction() -> f64 { 0.3 }

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum HorizontalPlacement {
    #[default]
    Top,
    Bottom,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerticalPlacement {
    #[default]
    Left,
    Right,
}

impl StackLineSettings {
    pub fn thickness(&self) -> f64 { if self.enabled { self.thickness } else { 0.0 } }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct LayoutSettings {
    /// Gap configuration for window spacing
    #[serde(default)]
    pub gaps: GapSettings,
}

/// Gap configuration for window spacing
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct GapSettings {
    /// Outer gaps (space between windows and screen edges)
    #[serde(default)]
    pub outer: OuterGaps,
    /// Inner gaps (space between windows)
    #[serde(default)]
    pub inner: InnerGaps,
    /// Display-specific gap overrides keyed by display UUID
    #[serde(default)]
    pub per_display: HashMap<String, GapOverride>,
}

/// Outer gap configuration (space between windows and screen edges)
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct OuterGaps {
    /// Gap at the top of the screen
    #[serde(default)]
    pub top: f64,
    /// Gap at the left of the screen
    #[serde(default)]
    pub left: f64,
    /// Gap at the bottom of the screen
    #[serde(default)]
    pub bottom: f64,
    /// Gap at the right of the screen
    #[serde(default)]
    pub right: f64,
}

/// Inner gap configuration (space between windows)
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct InnerGaps {
    /// Horizontal gap between windows
    #[serde(default)]
    pub horizontal: f64,
    /// Vertical gap between windows
    #[serde(default)]
    pub vertical: f64,
}

/// Overrides for gaps on a per-display basis
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct GapOverride {
    /// Override outer gaps completely for the display
    #[serde(default)]
    pub outer: Option<OuterGaps>,
    /// Override inner gaps completely for the display
    #[serde(default)]
    pub inner: Option<InnerGaps>,
}

impl Settings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.animation_duration < 0.0 {
            issues.push(format!(
                "animation_duration must be non-negative, got {}",
                self.animation_duration
            ));
        }

        if self.animation_fps <= 0.0 {
            issues.push(format!(
                "animation_fps must be positive, got {}",
                self.animation_fps
            ));
        }

        if !(1..=100).contains(&self.diagnostics.max_file_size_mb) {
            issues.push(format!(
                "diagnostics.max_file_size_mb must be in 1..=100, got {}",
                self.diagnostics.max_file_size_mb
            ));
        }
        if !(1..=10).contains(&self.diagnostics.retained_files) {
            issues.push(format!(
                "diagnostics.retained_files must be in 1..=10, got {}",
                self.diagnostics.retained_files
            ));
        }

        issues.extend(self.layout.validate());

        if self.gestures.swipe_vertical_tolerance < 0.0 {
            issues.push(format!(
                "gestures.swipe_vertical_tolerance must be non-negative, got {}",
                self.gestures.swipe_vertical_tolerance
            ));
        }

        issues
    }
}

impl LayoutSettings {
    pub fn validate(&self) -> Vec<String> { self.gaps.validate() }
}

impl GapSettings {
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        // Validate outer gaps
        issues.extend(self.outer.validate());

        // Validate inner gaps
        issues.extend(self.inner.validate());

        for (uuid, overrides) in &self.per_display {
            if let Some(outer) = &overrides.outer {
                for issue in outer.validate() {
                    issues.push(format!("per_display[{uuid}] {issue}"));
                }
            }
            if let Some(inner) = &overrides.inner {
                for issue in inner.validate() {
                    issues.push(format!("per_display[{uuid}] {issue}"));
                }
            }
        }

        issues
    }

    pub fn effective_for_display(&self, display_uuid: Option<&str>) -> GapSettings {
        let mut resolved = GapSettings {
            outer: self.outer.clone(),
            inner: self.inner.clone(),
            per_display: HashMap::default(),
        };
        if let Some(uuid) = display_uuid {
            if let Some(overrides) = self.per_display.get(uuid) {
                if let Some(outer_override) = &overrides.outer {
                    resolved.outer = outer_override.clone();
                }
                if let Some(inner_override) = &overrides.inner {
                    resolved.inner = inner_override.clone();
                }
            }
        }
        resolved
    }
}

impl OuterGaps {
    /// Validates outer gap configuration values and returns a list of issues found.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.top < 0.0 {
            issues.push(format!("outer.top gap must be non-negative, got {}", self.top));
        }

        if self.left < 0.0 {
            issues.push(format!("outer.left gap must be non-negative, got {}", self.left));
        }

        if self.bottom < 0.0 {
            issues.push(format!(
                "outer.bottom gap must be non-negative, got {}",
                self.bottom
            ));
        }

        if self.right < 0.0 {
            issues.push(format!(
                "outer.right gap must be non-negative, got {}",
                self.right
            ));
        }

        issues
    }
}

impl InnerGaps {
    /// Validates inner gap configuration values and returns a list of issues found.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.horizontal < 0.0 {
            issues.push(format!(
                "inner.horizontal gap must be non-negative, got {}",
                self.horizontal
            ));
        }

        if self.vertical < 0.0 {
            issues.push(format!(
                "inner.vertical gap must be non-negative, got {}",
                self.vertical
            ));
        }

        issues
    }
}

fn yes() -> bool { true }

fn default_animation_duration() -> f64 { 0.2 }

fn default_animation_fps() -> f64 { 60.0 }

fn default_diagnostic_file_size_mb() -> u64 { 4 }

fn default_diagnostic_retained_files() -> usize { 3 }

#[allow(dead_code)]
fn no() -> bool { false }

// Interpreted as normalized fraction when <= 1.0. If > 1.0 and <= 100.0,
// it is treated as a percentage (e.g. 40.0 -> 0.40).
fn default_swipe_vertical_tolerance() -> f64 { 0.4 }
fn default_swipe_fingers() -> usize { 3 }
fn default_distance_pct() -> f64 { 0.08 }
fn default_stack_line_spacing() -> f64 { 1.0 }
fn default_stack_line_thickness() -> f64 { 20.0 }

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum HapticPattern {
    Generic,
    Alignment,
    #[default]
    LevelChange,
}

impl Config {
    pub fn read(path: &Path) -> anyhow::Result<Config> {
        let buf = std::fs::read_to_string(path)?;
        Self::parse(&buf)
    }

    pub fn default() -> Config { Self::parse(include_str!("../../lift.default.toml")).unwrap() }

    /// Save the current config to a file
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let config_file = ConfigFile {
            settings: self.settings.clone(),
            keys: self
                .key_specs
                .iter()
                .map(|(hotkey, command)| (hotkey.clone(), command.clone()))
                .collect(),
            virtual_workspaces: self.virtual_workspaces.clone(),
            modifier_combinations: HashMap::default(),
        };

        let toml_string = toml::to_string_pretty(&config_file)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, toml_string.as_bytes())?;

        Ok(())
    }

    /// Validates the entire configuration and returns a list of issues found.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        // Validate settings
        issues.extend(self.settings.validate());

        // Validate virtual workspace settings
        issues.extend(self.virtual_workspaces.validate());

        issues
    }

    fn normalize_hotkey_string(key: &str) -> String {
        let mut out = String::with_capacity(key.len());
        let mut word = String::new();

        for ch in key.chars() {
            if ch.is_alphabetic() {
                word.push(ch);
            } else {
                if !word.is_empty() {
                    let token = if word.len() == 1 {
                        word.to_ascii_uppercase()
                    } else {
                        match word.to_lowercase().as_str() {
                            "up" => "ArrowUp".to_string(),
                            "down" => "ArrowDown".to_string(),
                            "left" => "ArrowLeft".to_string(),
                            "right" => "ArrowRight".to_string(),
                            _ => word.clone(),
                        }
                    };
                    out.push_str(&token);
                    word.clear();
                }
                out.push(ch);
            }
        }

        if !word.is_empty() {
            let token = if word.len() == 1 {
                word.to_ascii_uppercase()
            } else {
                match word.to_lowercase().as_str() {
                    "up" => "ArrowUp".to_string(),
                    "down" => "ArrowDown".to_string(),
                    "left" => "ArrowLeft".to_string(),
                    "right" => "ArrowRight".to_string(),
                    _ => word.clone(),
                }
            };
            out.push_str(&token);
        }

        out
    }

    fn expand_modifier_combinations(key: &str, combinations: &HashMap<String, String>) -> String {
        if let Some(plus_pos) = key.find(" + ") {
            let potential_combo = &key[..plus_pos];
            if let Some(combo_value) = combinations.get(potential_combo) {
                let rest = &key[plus_pos + 3..];
                return format!("{} + {}", combo_value, rest);
            }
        }
        key.to_string()
    }

    /// no need to pull in a dep for just this
    fn levenshtein(a: &str, b: &str) -> usize {
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();
        let mut d = vec![vec![0usize; b_chars.len() + 1]; a_chars.len() + 1];
        for i in 0..=a_chars.len() {
            d[i][0] = i;
        }
        for j in 0..=b_chars.len() {
            d[0][j] = j;
        }
        for i in 1..=a_chars.len() {
            for j in 1..=b_chars.len() {
                let cost = if a_chars[i - 1] == b_chars[j - 1] {
                    0
                } else {
                    1
                };
                d[i][j] = std::cmp::min(
                    std::cmp::min(d[i - 1][j] + 1, d[i][j - 1] + 1),
                    d[i - 1][j - 1] + cost,
                );
            }
        }
        d[a_chars.len()][b_chars.len()]
    }

    // Extracts an "unknown variant `...`" token from serde error string when present.
    // Additionally, if serde's error message contains an "expected" list (backtick-delimited),
    // embed those expected tokens alongside the unknown token using the separator "||".
    // The resulting returned string may therefore be:
    //   - "unknown_token" (no expected candidates found)
    //   - "unknown_token||cand1,cand2,..." (candidates appended)
    fn extract_unknown_variant(err: &str) -> Option<String> {
        let needle = "unknown variant `";
        if let Some(start) = err.find(needle) {
            let rest = &err[start + needle.len()..];
            if let Some(end) = rest.find('`') {
                let unknown = rest[..end].to_string();

                // Collect all backtick-enclosed tokens in the error message and
                // treat them as candidate variants (excluding the unknown itself).
                let mut variants: Vec<String> = Vec::new();
                let mut i = 0usize;
                while let Some(open) = err[i..].find('`') {
                    let open_abs = i + open + 1;
                    if let Some(close_off) = err[open_abs..].find('`') {
                        let close_abs = open_abs + close_off;
                        let token = &err[open_abs..close_abs];
                        if token != unknown {
                            variants.push(token.to_string());
                        }
                        i = close_abs + 1;
                    } else {
                        break;
                    }
                }

                if !variants.is_empty() {
                    // dedupe while preserving order
                    let mut seen = std::collections::HashSet::new();
                    let mut deduped = Vec::new();
                    for v in variants {
                        if seen.insert(v.clone()) {
                            deduped.push(v);
                        }
                    }
                    return Some(format!("{}||{}", unknown, deduped.join(",")));
                }

                return Some(unknown);
            }
        }

        if let Some(unknown_pos) = err.find("unknown") {
            if let Some(backtick_pos) = err[unknown_pos..].find('`') {
                let rest = &err[unknown_pos + backtick_pos + 1..];
                if let Some(end) = rest.find('`') {
                    return Some(rest[..end].to_string());
                }
            }
        }
        None
    }

    // Provide suggestion by comparing the unknown token to a list of known commands.
    // If the `unknown` string was produced by `extract_unknown_variant` and contains
    // an embedded serde candidate list (format: "token||cand1,cand2"), prefer those
    // candidates when computing the best suggestion. Otherwise fall back to the
    // conservative builtin list.
    //
    // Returns the best candidate if its distance is within a reasonable threshold.
    fn suggest_similar_command(unknown: &str) -> Option<(String, Option<String>)> {
        // Detect if `unknown` was augmented with serde-provided expected variants.
        let (unknown_token, serde_candidates): (String, Option<Vec<String>>) =
            if let Some(idx) = unknown.find("||") {
                let (u, rest) = unknown.split_at(idx);
                let rest = &rest[2..];
                let candidates: Vec<String> = rest
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                (u.to_lowercase(), Some(candidates))
            } else {
                (unknown.to_lowercase(), None)
            };

        // Choose candidate set: prefer serde-provided ones when available.
        let mut best: Option<(String, usize)> = None;

        if let Some(cands) = serde_candidates {
            for cand in cands.iter() {
                let cand_norm = cand.to_lowercase();
                let dist = Self::levenshtein(&unknown_token, &cand_norm);
                if best.is_none() || dist < best.as_ref().unwrap().1 {
                    best = Some((cand.clone(), dist));
                }
            }
        } else {
            // Use dynamically generated builtin candidates.
            let builtin_candidates = crate::actor::wm_controller::WmCommand::builtin_candidates();
            for cand in builtin_candidates.iter() {
                let dist = Self::levenshtein(&unknown_token, &cand.to_lowercase());
                if best.is_none() || dist < best.as_ref().unwrap().1 {
                    best = Some((cand.to_string(), dist));
                }
            }
        }

        if let Some((best_cand, dist)) = best {
            // Heuristic threshold: allow suggestions if distance is <= half the length (or <=3).
            let threshold = std::cmp::max(3usize, best_cand.len() / 2);
            if dist <= threshold {
                // If the best candidate is in deprecated map, return the non-deprecated suggestion.
                let mut replacement = None;
                for &(dep, repl) in DEPRECATED_MAP.iter() {
                    if dep == best_cand {
                        replacement = Some(repl.to_string());
                        break;
                    }
                }
                return Some((best_cand.to_string(), replacement));
            }
        }

        // Also check if the unknown token itself matched a deprecated name exactly
        for &(dep, repl) in DEPRECATED_MAP.iter() {
            if dep == unknown_token {
                return Some((repl.to_string(), None)); // recommend replacement
            }
        }

        None
    }

    fn parse(buf: &str) -> anyhow::Result<Config> {
        // Attempt to deserialize. If it fails, and the error indicates an unknown enum
        // variant, attempt to provide a helpful suggestion.
        match toml::from_str::<ConfigFile>(&buf) {
            Ok(c) => {
                let mut keys = Vec::new();
                let mut key_specs = Vec::new();
                for (key, cmd) in c.keys {
                    let expanded_key =
                        Self::expand_modifier_combinations(&key, &c.modifier_combinations);
                    let normalized_key = Self::normalize_hotkey_string(&expanded_key);
                    let Ok(hotkey) = Hotkey::from_str(&normalized_key) else {
                        bail!("Could not parse hotkey: {key}");
                    };
                    keys.push((hotkey, cmd.clone()));
                    key_specs.push((normalized_key, cmd));
                }
                Ok(Config {
                    settings: c.settings,
                    keys,
                    key_specs,
                    virtual_workspaces: c.virtual_workspaces,
                })
            }
            Err(e) => {
                let msg = e.to_string();
                if let Some(unknown_token) = Self::extract_unknown_variant(&msg) {
                    if let Some((suggestion, deprecated_replacement)) =
                        Self::suggest_similar_command(&unknown_token)
                    {
                        if let Some(repl) = deprecated_replacement {
                            bail!(
                                "{msg}\nDid you mean `{}`? Note: `{}` is deprecated; use `{}` instead.",
                                suggestion,
                                suggestion,
                                repl
                            );
                        } else {
                            bail!("{msg}\nDid you mean `{}`?", suggestion);
                        }
                    } else {
                        bail!("{msg}");
                    }
                } else {
                    bail!("{msg}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lift_paths_use_lift_directories() {
        let home = Path::new("/Users/example");
        assert_eq!(data_dir_for(home), PathBuf::from("/Users/example/.lift"));
        assert_eq!(
            config_file_for(home),
            PathBuf::from("/Users/example/.config/lift/config.toml")
        );
        assert!(diagnostics_file().ends_with(".lift/diagnostics/operations.jsonl"));
    }

    #[test]
    fn diagnostics_are_bounded_and_enabled_by_default() {
        let settings = DiagnosticsSettings::default();
        assert!(settings.enabled);
        assert_eq!(settings.max_file_size_mb, 4);
        assert_eq!(settings.retained_files, 3);

        let mut config = Config::default();
        config.settings.diagnostics.max_file_size_mb = 0;
        config.settings.diagnostics.retained_files = 11;
        let issues = config.validate();
        assert!(issues.iter().any(|issue| issue.contains("max_file_size_mb")));
        assert!(issues.iter().any(|issue| issue.contains("retained_files")));
    }

    #[test]
    fn digit_row_workspace_numbers_map_to_global_slots() {
        assert_eq!(workspace_number_to_global_slot(1), Some(0));
        assert_eq!(workspace_number_to_global_slot(9), Some(8));
        assert_eq!(workspace_number_to_global_slot(0), Some(9));
        assert_eq!(workspace_number_to_global_slot(10), None);
        assert_eq!(workspace_number_to_global_slot(11), None);
    }

    #[test]
    fn workspace_zero_is_valid_and_ten_is_rejected_in_configuration() {
        let valid: VirtualWorkspaceSettings = toml::from_str(
            r#"
                display_default_workspaces = { main = 0 }
                app_rules = [{ app_id = "com.example.app", workspace = 0 }]
            "#,
        )
        .unwrap();
        assert!(valid.validate().is_empty());

        let invalid: VirtualWorkspaceSettings = toml::from_str(
            r#"
                display_default_workspaces = { main = 10 }
                app_rules = [{ app_id = "com.example.app", workspace = 10 }]
            "#,
        )
        .unwrap();
        let issues = invalid.validate();
        assert_eq!(
            issues.iter().filter(|issue| issue.contains("outside 0..=9")).count(),
            2
        );
    }

    #[test]
    fn display_migration_priority_defaults_to_empty() {
        assert!(VirtualWorkspaceSettings::default().display_migration_priority.is_empty());
    }

    #[test]
    fn menu_bar_workspace_scope_defaults_to_per_display() {
        assert_eq!(
            MenuBarSettings::default().workspace_scope,
            MenuBarWorkspaceScope::PerDisplay
        );
        let global: MenuBarSettings = toml::from_str("workspace_scope = \"global\"").unwrap();
        assert_eq!(global.workspace_scope, MenuBarWorkspaceScope::Global);
    }

    #[test]
    fn display_order_preserves_configured_order_and_rejects_invalid_uuids() {
        let settings: VirtualWorkspaceSettings =
            toml::from_str(r#"display_order = ["display-left", "display-right"]"#).unwrap();
        assert_eq!(settings.display_order, vec![
            "display-left".to_string(),
            "display-right".to_string()
        ]);

        let mut invalid = VirtualWorkspaceSettings::default();
        invalid.display_order = vec!["display-left".into(), "".into(), "display-left".into()];
        let issues = invalid.validate();
        assert!(issues.iter().any(|issue| issue.contains("display_order has empty")));
        assert!(issues.iter().any(|issue| issue.contains("display_order has duplicate")));
    }

    #[test]
    fn display_migration_priority_preserves_configured_order() {
        let settings: VirtualWorkspaceSettings =
            toml::from_str(r#"display_migration_priority = ["display-b", "display-a"]"#).unwrap();
        assert_eq!(settings.display_migration_priority, vec![
            "display-b".to_string(),
            "display-a".to_string()
        ]);
    }

    #[test]
    fn display_migration_priority_rejects_empty_and_duplicate_uuids() {
        let mut settings = VirtualWorkspaceSettings::default();
        settings.display_migration_priority =
            vec!["display-a".into(), "".into(), "display-a".into()];
        let issues = settings.validate();
        assert!(issues.iter().any(|issue| issue.contains("empty display UUID")));
        assert!(issues.iter().any(|issue| issue.contains("duplicate display UUID")));
    }

    #[test]
    fn layout_config_rejects_removed_modes_and_fields() {
        assert!(toml::from_str::<LayoutSettings>("mode = \"bsp\"").is_err());
        assert!(toml::from_str::<LayoutSettings>("stack = {}").is_err());
        assert!(toml::from_str::<LayoutSettings>("scrolling = {}").is_err());
        assert!(toml::from_str::<LayoutSettings>("master_stack = {}").is_err());
    }

    #[test]
    fn test_normalize_hotkey_string() {
        assert_eq!(
            Config::normalize_hotkey_string("Alt + Shift + Down"),
            "Alt + Shift + ArrowDown"
        );
        assert_eq!(Config::normalize_hotkey_string("Ctrl + Up"), "Ctrl + ArrowUp");
        assert_eq!(
            Config::normalize_hotkey_string("Shift + Left"),
            "Shift + ArrowLeft"
        );
        assert_eq!(
            Config::normalize_hotkey_string("Meta + Right"),
            "Meta + ArrowRight"
        );
    }

    #[test]
    fn test_modifier_combinations_in_config() {
        let toml = r#"
            [settings]
            animate = false

            [modifier_combinations]
            comb1 = "Alt + Shift"
            leader = "Ctrl + Alt"

            [keys]
            "comb1 + C" = "toggle_space_activated"
            "leader + Tab" = "next_workspace"
            "Alt + H" = { move_focus = "left" }
        "#;

        let cfg = Config::parse(toml).unwrap();
        // We expect keys to be parsed into hotkeys
        assert!(!cfg.keys.is_empty());
    }

    #[test]
    fn serde_round_trip_preserves_key_specs() {
        let cfg = Config::default();
        assert!(!cfg.key_specs.is_empty());

        let json = serde_json::to_string(&cfg).unwrap();
        let round_tripped: Config = serde_json::from_str(&json).unwrap();

        assert_eq!(round_tripped.key_specs, cfg.key_specs);
    }

    #[test]
    fn serde_without_key_specs_reconstructs_from_keys() {
        let cfg = Config::default();
        let mut json = serde_json::to_value(&cfg).unwrap();
        json.as_object_mut().unwrap().remove("key_specs");

        let round_tripped: Config = serde_json::from_value(json).unwrap();

        assert_eq!(round_tripped.key_specs.len(), round_tripped.keys.len());
        assert!(!round_tripped.key_specs.is_empty());
    }

    #[test]
    fn test_levenshtein_suggests() {
        let err =
            "unknown variant `toggle_stak`, expected one of `toggle_stack`, `toggle_orientation`";
        let token = Config::extract_unknown_variant(err).unwrap();
        assert_eq!(token, "toggle_stak||toggle_stack,toggle_orientation");
        let suggestion = Config::suggest_similar_command(&token);
        assert!(suggestion.is_some());
        let (s, _maybe_dep) = suggestion.unwrap();
        assert_eq!(s, "toggle_stack");
    }
}
