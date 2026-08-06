use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use slotmap::{SlotMap, new_key_type};
use tracing::{error, warn};

use crate::actor::app::WindowId;
use crate::common::collections::{HashMap, HashSet};
use crate::common::config::{
    AppWorkspaceRule, LayoutMode, LayoutSettings, VirtualWorkspaceSettings, WorkspaceSelector,
};
use crate::common::log::trace_misc;
use crate::layout_engine::Direction;
use crate::layout_engine::systems::{BspLayoutSystem, LayoutSystemKind};
use crate::model::{WindowRegistryHandle, WindowWorkspaceInfo};
use crate::sys::app::pid_t;
use crate::sys::geometry::CGRectDef;
use crate::sys::screen::SpaceId;

new_key_type! {
    pub struct VirtualWorkspaceId;
}

/// Global workspace identifier exposed to the user (the digit on the hotkey).
/// 0..=9 today; the type is `usize` for indexing convenience.
pub type WorkspaceNumber = usize;

/// Number of global workspace slots reachable by the digit-row hotkeys
/// (1, 2, …, 9, 0). Kept independent of any per-display config because the
/// global pool is shared across all displays.
pub const GLOBAL_WORKSPACE_SLOTS: usize = 10;

/// Prefix used to synthesize placeholder display UUIDs for spaces whose real
/// display UUID hasn't been registered yet (typically `set_active_workspace`
/// firing before `set_space_display`). The prefix is collision-proof against
/// real macOS UUIDs, which are 36-char hex-with-dashes.
const SYNTHETIC_DISPLAY_UUID_PREFIX: &str = "__space_";

fn synthetic_display_uuid(space: SpaceId) -> String {
    format!("{}{}", SYNTHETIC_DISPLAY_UUID_PREFIX, space.get())
}

impl std::fmt::Display for VirtualWorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dbg = format!("{:?}", self);
        let digits: String = dbg.chars().filter(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u64>() {
            write!(f, "{:08}", n)
        } else {
            write!(f, "{}", dbg)
        }
    }
}

/// Result of resolving a digit-row hotkey through `resolve_workspace`.
/// Bundles the per-space coordinates (so the caller can dispatch a regular
/// per-space SwitchToWorkspace) along with the chosen display UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotTarget {
    pub space: SpaceId,
    pub workspace_id: VirtualWorkspaceId,
    pub per_space_index: usize,
    pub display_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceError {
    NoWorkspacesAvailable,
    AssignmentFailed,
    InvalidWorkspaceId(VirtualWorkspaceId),
    InvalidWorkspaceIndex(usize),
    InconsistentState(String),
}

/// Details about an app rule assignment when Rift will manage the window.
#[derive(Debug, Clone, Copy)]
pub struct AppRuleAssignment {
    pub workspace_id: VirtualWorkspaceId,
    pub floating: bool,
    pub prev_rule_decision: bool,
}

/// Result of evaluating app rules for a window.
#[derive(Debug, Clone, Copy)]
pub enum AppRuleResult {
    Managed(AppRuleAssignment),
    Unmanaged,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VirtualWorkspace {
    /// The user-visible workspace number (the digit on the hotkey, or any
    /// future explicit number). Source of truth for `workspace_by_number`
    /// lookups. `#[serde(default)]` so that pre-Phase-3 saved state without
    /// this field deserializes (the next mirror rebuild will write the
    /// correct number based on display state).
    #[serde(default)]
    pub number: WorkspaceNumber,
    pub name: String,
    pub space: SpaceId,
    windows: HashSet<WindowId>,
    last_focused: Option<WindowId>,
    #[serde(default = "default_layout_system_kind")]
    pub layout_system: LayoutSystemKind,
    #[serde(default, deserialize_with = "deserialize_runtime_layout_mode")]
    pub layout_mode: LayoutMode,
}

fn default_layout_system_kind() -> LayoutSystemKind {
    VirtualWorkspace::create_layout_system()
}

fn normalize_layout_mode(_mode: LayoutMode) -> LayoutMode {
    _mode.runtime()
}

fn deserialize_runtime_layout_mode<'de, D>(deserializer: D) -> Result<LayoutMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    LayoutMode::deserialize(deserializer).map(LayoutMode::runtime)
}

impl VirtualWorkspace {
    fn new(
        number: WorkspaceNumber,
        name: String,
        space: SpaceId,
        mode: LayoutMode,
        _settings: &LayoutSettings,
    ) -> Self {
        let mode = normalize_layout_mode(mode);
        let layout_system = Self::create_layout_system();
        Self {
            number,
            name,
            space,
            windows: HashSet::default(),
            last_focused: None,
            layout_system,
            layout_mode: mode,
        }
    }

    pub fn tree(&self) -> &LayoutSystemKind {
        &self.layout_system
    }

    pub fn tree_mut(&mut self) -> &mut LayoutSystemKind {
        &mut self.layout_system
    }

    pub fn layout_mode(&self) -> LayoutMode {
        self.layout_mode
    }

    pub fn create_layout_system() -> LayoutSystemKind {
        LayoutSystemKind::Bsp(BspLayoutSystem::default())
    }

    pub fn contains_window(&self, window_id: WindowId) -> bool {
        self.windows.contains(&window_id)
    }

    pub fn windows(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.windows.iter().copied()
    }

    pub fn add_window(&mut self, window_id: WindowId) {
        self.windows.insert(window_id);
    }

    pub fn remove_window(&mut self, window_id: WindowId) -> bool {
        if self.last_focused == Some(window_id) {
            self.last_focused = None;
        }
        self.windows.remove(&window_id)
    }

    pub fn set_last_focused(&mut self, window_id: Option<WindowId>) {
        self.last_focused = window_id;
    }

    pub fn last_focused(&self) -> Option<WindowId> {
        self.last_focused
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HideCorner {
    BottomLeft,
    #[default]
    BottomRight,
}

impl HideCorner {
    pub fn opposite(self) -> Self {
        match self {
            HideCorner::BottomLeft => HideCorner::BottomRight,
            HideCorner::BottomRight => HideCorner::BottomLeft,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VirtualWorkspaceManager {
    pub(crate) workspaces: SlotMap<VirtualWorkspaceId, VirtualWorkspace>,
    /// Mirror of the engine's space → display UUID map. Session-only.
    #[serde(skip)]
    display_uuid_for_space: HashMap<SpaceId, String>,
    /// New model: workspace number → ws id. Presence of the key means the
    /// workspace exists. Source of truth for global slot lookups.
    #[serde(skip)]
    workspace_by_number: HashMap<WorkspaceNumber, VirtualWorkspaceId>,
    /// New model: ws id → bound display UUID. Immutable during ws lifetime
    /// (under stable display config).
    #[serde(skip)]
    display_for_workspace: HashMap<VirtualWorkspaceId, String>,
    /// New model: per-display active ws number. Updated directly by
    /// `set_active_workspace` and cleaned up during destruction/unplug.
    #[serde(skip)]
    active_workspace_per_display: HashMap<String, WorkspaceNumber>,
    /// New model: per-display previous ws number for switch_to_last.
    #[serde(skip)]
    last_workspace_per_display: HashMap<String, WorkspaceNumber>,
    /// Cached copy of `VirtualWorkspaceSettings::display_default_workspaces`.
    /// Per-display pinned default workspace number, used at lazy init.
    #[serde(skip)]
    display_default_workspaces: HashMap<String, WorkspaceNumber>,
    floating_positions: HashMap<(SpaceId, VirtualWorkspaceId), FloatingWindowPositions>,
    workspace_counter: usize,
    #[serde(skip)]
    app_rules: Vec<AppWorkspaceRule>,
    #[serde(skip)]
    app_rule_regex_cache: Vec<Option<regex::Regex>>,
    #[serde(skip)]
    max_workspaces: usize,
    #[serde(skip)]
    pub workspace_auto_back_and_forth: bool,
    #[serde(skip)]
    pub workspace_rules: Vec<crate::common::config::WorkspaceLayoutRule>,
    #[serde(skip)]
    pub default_layout_mode: LayoutMode,
    #[serde(skip)]
    pub layout_settings: LayoutSettings,
    // skipping serializiation but proper layout restores will need window registry to be saved
    #[serde(skip, default = "WindowRegistryHandle::new")]
    window_registry: WindowRegistryHandle,
    #[serde(skip, default)]
    #[allow(dead_code)]
    owned_window_registry: Box<crate::model::WindowRegistry>,
}

impl Default for VirtualWorkspaceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualWorkspaceManager {
    pub fn new() -> Self {
        Self::new_with_config(&VirtualWorkspaceSettings::default(), &LayoutSettings::default())
    }

    pub fn new_with_rules(
        app_rules: Vec<AppWorkspaceRule>,
        layout_settings: LayoutSettings,
    ) -> Self {
        let mut cfg = VirtualWorkspaceSettings::default();
        cfg.app_rules = app_rules;
        Self::new_with_config(&cfg, &layout_settings)
    }

    pub fn new_with_config(
        config: &VirtualWorkspaceSettings,
        layout_settings: &LayoutSettings,
    ) -> Self {
        let max_workspaces = 32;

        let mut owned_window_registry: Box<crate::model::WindowRegistry> = Box::default();
        let mut window_registry = WindowRegistryHandle::new();
        window_registry.attach(owned_window_registry.as_mut());

        let mut manager = Self {
            workspaces: SlotMap::default(),
            display_uuid_for_space: HashMap::default(),
            floating_positions: HashMap::default(),
            workspace_counter: 1,
            app_rules: config.app_rules.clone(),
            app_rule_regex_cache: Vec::new(),
            max_workspaces,
            workspace_auto_back_and_forth: config.workspace_auto_back_and_forth,
            workspace_rules: config.workspace_rules.clone(),
            default_layout_mode: normalize_layout_mode(layout_settings.mode),
            layout_settings: layout_settings.clone(),
            workspace_by_number: HashMap::default(),
            display_for_workspace: HashMap::default(),
            active_workspace_per_display: HashMap::default(),
            last_workspace_per_display: HashMap::default(),
            display_default_workspaces: config.display_default_workspaces.clone(),
            window_registry,
            owned_window_registry,
        };

        manager.rebuild_app_rule_regex_cache();
        manager
    }

    pub fn window_registry(&self) -> WindowRegistryHandle {
        self.window_registry.clone()
    }

    pub fn attach_window_registry(&mut self, registry: &mut crate::model::WindowRegistry) {
        self.window_registry.attach(registry);
    }

    pub fn update_settings(
        &mut self,
        config: &VirtualWorkspaceSettings,
        layout_settings: &LayoutSettings,
    ) {
        self.app_rules = config.app_rules.clone();
        self.workspace_rules = config.workspace_rules.clone();
        self.default_layout_mode = normalize_layout_mode(layout_settings.mode);
        self.layout_settings = layout_settings.clone();
        self.workspace_auto_back_and_forth = config.workspace_auto_back_and_forth;
        self.display_default_workspaces = config.display_default_workspaces.clone();
        self.rebuild_app_rule_regex_cache();
    }

    fn rebuild_app_rule_regex_cache(&mut self) {
        self.app_rule_regex_cache = self
            .app_rules
            .iter()
            .map(|rule| {
                rule.title_regex.as_ref().and_then(|rule_re| {
                    if rule_re.is_empty() {
                        return None;
                    }
                    match regex::RegexBuilder::new(rule_re).case_insensitive(true).build() {
                        Ok(regex) => Some(regex),
                        Err(e) => {
                            warn!("Invalid title_regex '{}' in app rule: {}", rule_re, e);
                            None
                        }
                    }
                })
            })
            .collect();
    }

    fn ensure_space_initialized(&mut self, space: SpaceId) {
        // Phase 4: presence of a workspace bound to this space is the gating
        // condition; we no longer maintain a parallel `workspaces_by_space`
        // table. Walk live workspaces — cheap (≤ a few dozen) and avoids
        // depending on whether the display UUID is known yet.
        let already_initialized = self.workspaces.iter().any(|(_, ws)| ws.space == space);
        if already_initialized {
            return;
        }

        // Each space gets exactly ONE workspace at lazy init. Number is
        // determined by the per-display pin (if any) or the smallest unused
        // number across the whole manager. Activation lands via
        // `set_active_workspace`, which writes the per-display active table.
        let id = self.create_default_workspace_for_space(space);
        let _ = self.set_active_workspace(space, id);
    }

    /// Resolves the workspace number for a freshly-initialized space:
    ///   1. The pinned default in `display_default_workspaces` for this
    ///      display, if both the space's uuid is known AND a pin exists.
    ///   2. Otherwise the smallest unused number across the manager.
    ///
    /// CAVEAT — pin race: if the space's display UUID isn't yet populated
    /// in `display_uuid_for_space` when this fires (which happens when
    /// `recompute_and_set_active_spaces` triggers `ensure_space_initialized`
    /// before `reconcile_spaces_with_display_history` records the UUID),
    /// the pin is silently skipped and the smallest-unused fallback wins.
    /// The workspace is NOT renumbered later when the UUID is learned.
    /// In practice this only bites users who have configured
    /// `display_default_workspaces`. Tracked as follow-up to Task 3.4
    /// (display lifecycle), which is where the ordering can be tightened.
    fn pick_default_workspace_number(&self, space: SpaceId) -> WorkspaceNumber {
        if let Some(uuid) = self.display_uuid_for_space.get(&space) {
            if let Some(pinned) = self.display_default_workspaces.get(uuid).copied() {
                // Walk live workspaces directly — see comment on
                // `smallest_unused_workspace_number` for why mirror state
                // isn't reliable here.
                let already_taken = self.workspaces.iter().any(|(_, ws)| ws.number == pinned);
                if !already_taken {
                    return pinned;
                }
            }
        }
        self.smallest_unused_workspace_number()
    }

    fn smallest_unused_workspace_number(&self) -> WorkspaceNumber {
        // Iterate every live workspace, not just `workspace_by_number`, because
        // the new-model mirror only tracks workspaces whose space has a known
        // display UUID. Lazy init can fire before display registration (e.g.
        // during recompute_and_set_active_spaces, which exposes spaces a
        // moment before reconcile_spaces_with_display_history records their
        // UUIDs); at that point `workspace_by_number` is still empty even
        // though some workspaces already exist. Walking the SlotMap keeps
        // numbers globally unique regardless of mirror state.
        let mut used: crate::common::collections::BTreeSet<WorkspaceNumber> =
            self.workspaces.iter().map(|(_, ws)| ws.number).collect();
        let mut n: WorkspaceNumber = 0;
        while used.contains(&n) {
            used.remove(&n);
            n += 1;
        }
        n
    }

    /// Phase 4: create exactly one workspace for `space` using the per-display
    /// rule. Writes the new-model tables (`workspace_by_number`,
    /// `display_for_workspace`) directly when the space's display UUID is
    /// known; otherwise the workspace exists in the SlotMap only and the
    /// tables pick it up the next time a display UUID lands via
    /// `set_space_display`. Does NOT touch
    /// `active_workspace_per_display` — caller activates if desired.
    fn create_default_workspace_for_space(&mut self, space: SpaceId) -> VirtualWorkspaceId {
        let number = self.pick_default_workspace_number(space);
        let name = number.to_string();
        let mode = self.resolve_layout_mode_for_workspace(number, &name);
        let ws = VirtualWorkspace::new(number, name, space, mode, &self.layout_settings);
        let id = self.workspaces.insert(ws);
        if let Some(uuid) = self.display_uuid_for_space.get(&space).cloned() {
            self.bind_workspace_to_display(id, number, uuid);
        }
        id
    }

    fn resolve_layout_mode_for_workspace(&self, index: usize, name: &str) -> LayoutMode {
        // Check workspace_rules (last matching rule wins, like app_rules)
        for rule in self.workspace_rules.iter().rev() {
            match &rule.workspace {
                WorkspaceSelector::Index(idx) if *idx == index => {
                    return normalize_layout_mode(rule.layout);
                }
                WorkspaceSelector::Name(n) if n == name => {
                    return normalize_layout_mode(rule.layout);
                }
                _ => continue,
            }
        }
        // Fall back to global default
        normalize_layout_mode(self.default_layout_mode)
    }

    pub fn initialized_spaces(&self) -> Vec<SpaceId> {
        // Derive from live workspaces — `ws.space` is the ground truth.
        // Spaces with no workspaces (post-prune) drop out automatically.
        let mut spaces: HashSet<SpaceId> = HashSet::default();
        for (_, ws) in &self.workspaces {
            spaces.insert(ws.space);
        }
        spaces.into_iter().collect()
    }

    /// Returns true iff at least one workspace is bound to `display_uuid` in
    /// the new-model `display_for_workspace` table. O(N_workspaces_in_mirror)
    /// scan; use this in preference to peeking into the legacy per-space
    /// pool when answering "does this display already have a workspace?".
    pub fn has_workspace_for_display(&self, display_uuid: &str) -> bool {
        self.display_for_workspace.values().any(|uuid| uuid == display_uuid)
    }

    /// Ensures at least one workspace is registered in the new model for
    /// `display_uuid` (which must already be paired with `space` via
    /// `set_space_display`). If the new-model mirror already lists a
    /// workspace for the display, returns its id; otherwise creates the
    /// per-display default and returns its id. `debug_assert!`s that
    /// `display_uuid` agrees with the stored mapping for `space` — same
    /// invariant `create_workspace_with_number` enforces — so a caller
    /// passing mismatched arguments fails loudly in debug builds instead
    /// of silently creating a workspace on the wrong display.
    pub fn ensure_default_workspace_for_display(
        &mut self,
        display_uuid: &str,
        space: SpaceId,
    ) -> VirtualWorkspaceId {
        if let Some(existing) = self.display_uuid_for_space.get(&space) {
            debug_assert_eq!(
                existing.as_str(),
                display_uuid,
                "ensure_default_workspace_for_display: display_uuid {} disagrees with stored {} for space {:?}",
                display_uuid,
                existing,
                space,
            );
        }
        if let Some((ws_id, _)) = self
            .display_for_workspace
            .iter()
            .find(|(_, uuid)| uuid.as_str() == display_uuid)
        {
            return *ws_id;
        }
        let id = self.create_default_workspace_for_space(space);
        // Activate iff the display has no active ws yet. Use the
        // direct-write set_active_workspace path so the per-display
        // active table picks it up.
        if !self.active_workspace_per_display.contains_key(display_uuid) {
            let _ = self.set_active_workspace(space, id);
        }
        id
    }

    /// Returns the workspaces visible on `space`, ordered by user-facing
    /// workspace number. Read-only; the public `list_workspaces` wraps this
    /// with a lazy-init step.
    ///
    /// When the space has a known display UUID (the steady-state case after
    /// `set_space_display`), the result is sourced from the new-model
    /// `display_for_workspace` filtered by the uuid AND by `ws.space == space`.
    /// When the UUID is not yet known (test code that never set a display, or
    /// a brief race between `recompute_and_set_active_spaces` and
    /// `reconcile_spaces_with_display_history`), falls back to walking the
    /// SlotMap by `space`. Both branches sort by ws number, so the
    /// per-display ordering is deterministic.
    fn workspaces_for_space(&self, space: SpaceId) -> Vec<(VirtualWorkspaceId, String)> {
        let mut entries: Vec<(WorkspaceNumber, VirtualWorkspaceId, String)> =
            if let Some(uuid) = self.display_uuid_for_space.get(&space) {
                self.display_for_workspace
                    .iter()
                    .filter_map(|(ws_id, ws_uuid)| {
                        if ws_uuid != uuid {
                            return None;
                        }
                        let ws = self.workspaces.get(*ws_id)?;
                        if ws.space != space {
                            return None;
                        }
                        Some((ws.number, *ws_id, ws.name.clone()))
                    })
                    .collect()
            } else {
                self.workspaces
                    .iter()
                    .filter_map(|(id, ws)| {
                        if ws.space != space {
                            return None;
                        }
                        Some((ws.number, id, ws.name.clone()))
                    })
                    .collect()
            };
        entries.sort_by_key(|(n, _, _)| *n);
        entries.into_iter().map(|(_, id, name)| (id, name)).collect()
    }

    // ----- workspace resolution ------------------------------------------------
    //
    // The new model holds two HashMap-keyed tables that together describe
    // global workspace layout:
    //
    //   workspace_by_number: number → workspace_id
    //   display_for_workspace: workspace_id → display_uuid
    //
    // Plus two per-display tables for active/last state:
    //
    //   active_workspace_per_display: display_uuid → number
    //   last_workspace_per_display:   display_uuid → number
    //
    // All resolver helpers (`resolve_workspace`, `space_for_display`,
    // `slot_workspace`, etc.) are HashMap lookups over these tables.

    /// New-model resolver: returns the (workspace_id, display_uuid, space_id)
    /// for workspace number `n`, if it exists. Single HashMap lookup.
    pub fn resolve_workspace(&self, n: WorkspaceNumber) -> Option<SlotTarget> {
        let workspace_id = *self.workspace_by_number.get(&n)?;
        let display_uuid = self.display_for_workspace.get(&workspace_id)?.clone();
        let space = self.workspaces.get(workspace_id)?.space;
        // per_space_index is the position in the per-display list for `space`.
        // Compute on demand from `workspaces_for_space`.
        let per_space_index = self
            .workspaces_for_space(space)
            .into_iter()
            .position(|(id, _)| id == workspace_id)?;
        Some(SlotTarget {
            space,
            workspace_id,
            per_space_index,
            display_uuid,
        })
    }

    pub fn workspace_space(&self, ws_id: VirtualWorkspaceId) -> Option<SpaceId> {
        self.workspaces.get(ws_id).map(|ws| ws.space)
    }

    /// Last-known display UUID for `space`, mirrored from the layout engine.
    pub fn space_display(&self, space: SpaceId) -> Option<&str> {
        self.display_uuid_for_space.get(&space).map(String::as_str)
    }

    /// Update the space → display UUID mirror used by `resolve_workspace`.
    /// Promotes workspaces that were created before their display UUID was
    /// known into the new-model tables (`workspace_by_number`,
    /// `display_for_workspace`).
    pub fn set_space_display(&mut self, space: SpaceId, display_uuid: Option<String>) {
        match display_uuid {
            Some(uuid) => {
                let prior = self.display_uuid_for_space.insert(space, uuid.clone());
                // If we had a synthetic placeholder, migrate active/last and
                // any workspace→uuid mirrors keyed under the synthetic uuid.
                if let Some(old_uuid) = prior {
                    if old_uuid != uuid {
                        if let Some(n) = self.active_workspace_per_display.remove(&old_uuid) {
                            self.active_workspace_per_display.insert(uuid.clone(), n);
                        }
                        if let Some(n) = self.last_workspace_per_display.remove(&old_uuid) {
                            self.last_workspace_per_display.insert(uuid.clone(), n);
                        }
                        for entry in self.display_for_workspace.values_mut() {
                            if *entry == old_uuid {
                                *entry = uuid.clone();
                            }
                        }
                    }
                }
                // Promote any workspaces on this space that aren't yet bound
                // to a display in the new-model tables. Per-display pin wins
                // even over an existing assignment; otherwise first-come.
                let mut entries: Vec<(VirtualWorkspaceId, WorkspaceNumber)> = self
                    .workspaces
                    .iter()
                    .filter(|(_, ws)| ws.space == space)
                    .map(|(id, ws)| (id, ws.number))
                    .collect();
                entries.sort_by_key(|(_, n)| *n);
                for (ws_id, number) in entries {
                    self.bind_workspace_to_display(ws_id, number, uuid.clone());
                }
            }
            None => {
                // Defensive scrub: removing this space's display binding
                // leaves entries keyed by the prior UUID dangling unless the
                // caller already scrubbed them (current call sites do, via
                // `migrate_workspaces_off_display`, but that's an unenforced
                // precondition). Walk the per-display tables and prune any
                // entries that would otherwise become orphans.
                let Some(prior_uuid) = self.display_uuid_for_space.remove(&space) else {
                    return;
                };
                // The `display_for_workspace` mirror keys by ws_id (not
                // space), so walk the workspaces that lived on this space
                // and unbind any whose entry still points at `prior_uuid`.
                // Workspaces that survive (still in the SlotMap) become
                // unrouted — a later `set_space_display(space, Some(...))`
                // will re-promote them through the existing "first-come"
                // path. We do NOT touch `workspace_by_number`: the
                // workspaces are still alive in the SlotMap; clearing the
                // number mapping would orphan the slotmap entry from the
                // global-slot resolver. Callers that wanted the workspaces
                // destroyed should use `migrate_workspaces_off_display` /
                // `destroy_workspace_*` first.
                let space_workspaces: Vec<VirtualWorkspaceId> = self
                    .workspaces
                    .iter()
                    .filter(|(_, ws)| ws.space == space)
                    .map(|(id, _)| id)
                    .collect();
                for ws_id in space_workspaces {
                    if self.display_for_workspace.get(&ws_id).map(String::as_str)
                        == Some(prior_uuid.as_str())
                    {
                        self.display_for_workspace.remove(&ws_id);
                    }
                }
                // If no other space still references `prior_uuid`, the
                // per-display active/last entries keyed by it would persist
                // forever. Scrub them.
                let uuid_still_in_use =
                    self.display_uuid_for_space.values().any(|u| u == &prior_uuid);
                if !uuid_still_in_use {
                    self.active_workspace_per_display.remove(&prior_uuid);
                    self.last_workspace_per_display.remove(&prior_uuid);
                }
            }
        }
    }

    /// Reverse lookup: SpaceId for a display UUID.
    pub fn space_for_display(&self, uuid: &str) -> Option<SpaceId> {
        // Iterate sorted by SpaceId so the choice is deterministic when two
        // spaces somehow share a UUID (shouldn't happen, but HashMap order
        // would otherwise be random).
        let mut entries: Vec<(&SpaceId, &String)> = self.display_uuid_for_space.iter().collect();
        entries.sort_by_key(|(space, _)| **space);
        entries.into_iter().find_map(|(space, u)| (u == uuid).then_some(*space))
    }

    /// Iterate every live workspace number (the new-model truth).
    /// Used by display-unplug migration to find numbers freed when a
    /// display's workspaces are destroyed.
    pub fn all_workspace_numbers(&self) -> impl Iterator<Item = WorkspaceNumber> + '_ {
        self.workspace_by_number.keys().copied()
    }

    /// Explicit ws creation with a chosen number on a chosen display.
    /// Used by SwitchToGlobalSlot create-on-demand and by tests that need to
    /// set up a multi-workspace state explicitly. If the space's display UUID
    /// isn't yet known to the `space → display_uuid` mirror, the
    /// caller-provided `display_uuid` populates it; otherwise we
    /// `debug_assert!` that the parameter agrees with the stored mapping
    /// (callers must not ask us to create a workspace on a display the space
    /// isn't on).
    pub fn create_workspace_with_number(
        &mut self,
        number: WorkspaceNumber,
        display_uuid: &str,
        space: SpaceId,
    ) -> VirtualWorkspaceId {
        match self.display_uuid_for_space.get(&space) {
            Some(existing) => debug_assert_eq!(
                existing.as_str(),
                display_uuid,
                "create_workspace_with_number: display_uuid {} disagrees with stored {} for space {:?}",
                display_uuid,
                existing,
                space,
            ),
            None => {
                self.display_uuid_for_space.insert(space, display_uuid.to_string());
            }
        }
        let name = number.to_string();
        let mode = self.resolve_layout_mode_for_workspace(number, &name);
        let ws = VirtualWorkspace::new(number, name, space, mode, &self.layout_settings);
        let id = self.workspaces.insert(ws);
        self.bind_workspace_to_display(id, number, display_uuid.to_string());
        id
    }

    pub fn remap_space(&mut self, old_space: SpaceId, new_space: SpaceId) {
        if old_space == new_space {
            return;
        }
        let has_old = self.workspaces.iter().any(|(_, ws)| ws.space == old_space);
        if !has_old {
            return;
        }

        // Drop any auto-created workspaces sitting on the target space; the
        // migrated state from `old_space` will replace them.
        let new_space_ws_ids: Vec<VirtualWorkspaceId> = self
            .workspaces
            .iter()
            .filter(|(_, ws)| ws.space == new_space)
            .map(|(id, _)| id)
            .collect();
        for ws_id in new_space_ws_ids {
            // Use `destroy_workspace_purge_active` so any stale
            // `active_workspace_per_display` / `last_workspace_per_display`
            // entries pointing at this workspace's number are scrubbed
            // before destruction. Without this scrub, a workspace that was
            // active on the target space's display before the remap would
            // leave a dangling number in the active table; if the same
            // number was later reissued the wrong workspace would silently
            // appear "active on that display".
            let _ = self.destroy_workspace_purge_active(ws_id);
        }

        // Rebind every workspace on `old_space` to `new_space`. The
        // new-model tables key by ws_id and uuid (not by SpaceId), so they
        // survive automatically.
        for (_, ws) in self.workspaces.iter_mut() {
            if ws.space == old_space {
                ws.space = new_space;
            }
        }

        self.window_registry.get_mut().remap_space(old_space, new_space);

        let mut new_positions = HashMap::default();
        for ((space, ws_id), positions) in std::mem::take(&mut self.floating_positions) {
            if space == new_space && old_space != new_space {
                continue;
            }
            let target_space = if space == old_space { new_space } else { space };
            new_positions.insert((target_space, ws_id), positions);
        }
        self.floating_positions = new_positions;

        // Migrate the space → display UUID mirror.
        if let Some(uuid) = self.display_uuid_for_space.remove(&old_space) {
            self.display_uuid_for_space.insert(new_space, uuid);
        }
    }

    pub fn create_workspace(
        &mut self,
        space: SpaceId,
        name: Option<String>,
    ) -> Result<VirtualWorkspaceId, WorkspaceError> {
        self.ensure_space_initialized(space);
        let count = self.workspaces.iter().filter(|(_, ws)| ws.space == space).count();
        if count >= self.max_workspaces {
            return Err(WorkspaceError::InconsistentState(format!(
                "Maximum workspace limit ({}) reached for space {:?}",
                self.max_workspaces, space
            )));
        }

        let name = name.unwrap_or_else(|| {
            let name = format!("Workspace {}", self.workspace_counter);
            self.workspace_counter += 1;
            name
        });

        let number = self.smallest_unused_workspace_number();
        let mode = self.resolve_layout_mode_for_workspace(number, &name);
        let workspace = VirtualWorkspace::new(number, name, space, mode, &self.layout_settings);
        let workspace_id = self.workspaces.insert(workspace);

        if let Some(uuid) = self.display_uuid_for_space.get(&space).cloned() {
            self.bind_workspace_to_display(workspace_id, number, uuid);
        }

        Ok(workspace_id)
    }

    /// Previous-workspace lookup for SwitchToLastWorkspace. Per-display
    /// scoped: the "last" workspace lives in `last_workspace_per_display`
    /// keyed by display uuid, and is updated whenever `set_active_workspace`
    /// observes a change. Returns `None` if the space has no known display
    /// or no recorded previous workspace.
    pub fn last_workspace(&self, space: SpaceId) -> Option<VirtualWorkspaceId> {
        let uuid = self.display_uuid_for_space.get(&space)?;
        let number = *self.last_workspace_per_display.get(uuid)?;
        self.workspace_by_number.get(&number).copied()
    }

    /// Currently active workspace for `space`. Sourced from the per-display
    /// active table — when the space has no recorded display UUID
    /// (pre-set_space_display state) returns None.
    pub fn active_workspace(&self, space: SpaceId) -> Option<VirtualWorkspaceId> {
        let uuid = self.display_uuid_for_space.get(&space)?;
        let number = *self.active_workspace_per_display.get(uuid)?;
        self.workspace_by_number.get(&number).copied()
    }

    pub fn active_workspace_idx(&self, space: SpaceId) -> Option<u64> {
        let active_ws_id = self.active_workspace(space)?;
        self.workspaces_for_space(space)
            .iter()
            .position(|(id, _)| *id == active_ws_id)
            .map(|idx| idx as u64)
    }

    pub fn workspace_auto_back_and_forth(&self) -> bool {
        self.workspace_auto_back_and_forth
    }

    /// Bind workspace `id` (with workspace number `number`) to display
    /// `uuid` in the `workspace_by_number` and `display_for_workspace`
    /// tables, but only when the per-display pin allows it. Rule: bind if
    /// (a) this display is pinned to take this workspace number via
    /// `display_default_workspaces`, or (b) the workspace number is
    /// currently unowned. Otherwise leave the existing binding intact —
    /// first-come ownership wins for unpinned numbers.
    fn bind_workspace_to_display(
        &mut self,
        id: VirtualWorkspaceId,
        number: WorkspaceNumber,
        uuid: String,
    ) {
        let pinned = self.display_default_workspaces.get(&uuid).copied();
        if pinned == Some(number) || !self.workspace_by_number.contains_key(&number) {
            self.workspace_by_number.insert(number, id);
            self.display_for_workspace.insert(id, uuid);
        }
    }

    /// Activate `workspace_id` on `space`. Writes the per-display active
    /// table directly, recording the previous active number in
    /// `last_workspace_per_display` when it changes. Returns `false` (and
    /// logs an error) if `workspace_id` doesn't exist, doesn't belong to
    /// `space`, or `space` has no known display UUID.
    pub fn set_active_workspace(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
    ) -> bool {
        trace_misc("set_active_workspace", || {
            // Synthesize a stable per-space UUID when set_space_display
            // hasn't been called yet — production paths always call
            // set_space_display first, but tests and pre-discovery code
            // paths still expect set_active_workspace to work. The
            // synthetic key is obviously distinct from any macOS UUID
            // (which is the standard 36-char hex-with-dashes form).
            let uuid = self
                .display_uuid_for_space
                .entry(space)
                .or_insert_with(|| synthetic_display_uuid(space))
                .clone();
            let Some(number) = self
                .workspaces
                .get(workspace_id)
                .filter(|ws| ws.space == space)
                .map(|ws| ws.number)
            else {
                error!(
                    "Attempted to set non-existent or foreign workspace {:?} as active for {:?}",
                    workspace_id, space
                );
                return false;
            };
            // Make sure the (number, ws_id, uuid) triple is registered in the
            // new-model tables even if the workspace was created before its
            // display UUID was known.
            self.workspace_by_number.entry(number).or_insert(workspace_id);
            self.display_for_workspace.entry(workspace_id).or_insert_with(|| uuid.clone());

            let prev = self.active_workspace_per_display.insert(uuid.clone(), number);
            if let Some(p) = prev {
                if p != number {
                    self.last_workspace_per_display.insert(uuid, p);
                }
            }
            true
        })
    }

    fn filtered_workspace_ids(
        &self,
        space: SpaceId,
        skip_empty: Option<bool>,
    ) -> Vec<VirtualWorkspaceId> {
        let require_non_empty = skip_empty == Some(true);
        self.workspaces_for_space(space)
            .into_iter()
            .filter_map(|(id, _)| {
                let ws = self.workspaces.get(id)?;
                if require_non_empty && ws.windows.is_empty() {
                    None
                } else {
                    Some(id)
                }
            })
            .collect()
    }

    fn step_workspace(
        &self,
        space: SpaceId,
        current: VirtualWorkspaceId,
        skip_empty: Option<bool>,
        dir: Direction,
    ) -> Option<VirtualWorkspaceId> {
        let base_ids: Vec<VirtualWorkspaceId> = if skip_empty == Some(true) {
            self.filtered_workspace_ids(space, Some(true))
        } else {
            self.workspaces_for_space(space).into_iter().map(|(id, _)| id).collect()
        };

        if base_ids.is_empty() {
            return None;
        }

        if let Some(pos) = base_ids.iter().position(|&id| id == current) {
            let i = dir.step(pos, base_ids.len());
            return Some(base_ids[i]);
        }

        let fallback_ids = self.filtered_workspace_ids(space, Some(false));
        if fallback_ids.is_empty() {
            return None;
        }
        let start = fallback_ids.iter().position(|&id| id == current)?;
        let require_non_empty = skip_empty == Some(true);

        let mut i = dir.step(start, fallback_ids.len());
        if !require_non_empty {
            return Some(fallback_ids[i]);
        }

        for _ in 0..fallback_ids.len() {
            let id = fallback_ids[i];
            if self.workspaces.get(id).map_or(false, |ws| !ws.windows.is_empty()) {
                return Some(id);
            }
            i = dir.step(i, fallback_ids.len());
        }
        None
    }

    pub fn next_workspace(
        &self,
        space: SpaceId,
        current: VirtualWorkspaceId,
        skip_empty: Option<bool>,
    ) -> Option<VirtualWorkspaceId> {
        self.step_workspace(space, current, skip_empty, Direction::Right)
    }

    pub fn prev_workspace(
        &self,
        space: SpaceId,
        current: VirtualWorkspaceId,
        skip_empty: Option<bool>,
    ) -> Option<VirtualWorkspaceId> {
        self.step_workspace(space, current, skip_empty, Direction::Left)
    }

    /// Assigns `window_id` to `workspace_id` on `space`. If the window was
    /// previously mapped to a different workspace and that workspace becomes
    /// empty + non-active as a result, that workspace is destroyed via the
    /// ephemeral guard.
    ///
    /// Returns `(success, destroyed_workspaces)`. The `destroyed_workspaces`
    /// list MUST be propagated back to the layout engine so it can drop the
    /// matching `workspace_layouts` entries — leaving them stale would cause
    /// `rebalance_all_layouts` to dereference a dead workspace id and panic.
    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn assign_window_to_workspace(
        &mut self,
        space: SpaceId,
        window_id: WindowId,
        workspace_id: VirtualWorkspaceId,
    ) -> (bool, Vec<(SpaceId, VirtualWorkspaceId)>) {
        trace_misc("assign_window_to_workspace", || {
            if !self.workspaces.contains_key(workspace_id)
                || self.workspaces.get(workspace_id).map(|w| w.space) != Some(space)
            {
                error!(
                    "Attempted to assign window to non-existent/foreign workspace {:?} for space {:?}",
                    workspace_id, space
                );
                return (false, Vec::new());
            }

            let existing_mapping = self.window_registry.get().workspace_info_for_window(window_id);
            let mut destroy_old: Option<VirtualWorkspaceId> = None;
            if let Some(WindowWorkspaceInfo {
                workspace_id: old_workspace_id, ..
            }) = existing_mapping
            {
                if let Some(old_workspace) = self.workspaces.get_mut(old_workspace_id) {
                    old_workspace.remove_window(window_id);
                }
                if old_workspace_id != workspace_id {
                    destroy_old = Some(old_workspace_id);
                }
            }

            let inserted = if let Some(workspace) = self.workspaces.get_mut(workspace_id) {
                workspace.add_window(window_id);
                self.window_registry.get_mut().assign_window_to_workspace(
                    window_id,
                    WindowWorkspaceInfo { space, workspace_id },
                );
                true
            } else {
                error!(
                    "Failed to get workspace {:?} for window assignment",
                    workspace_id
                );
                false
            };

            // Run the destroy AFTER the new mapping lands so a same-ws-id
            // assignment never tears down the destination (the `old !=
            // workspace_id` guard above already covers the explicit case;
            // post-insert ordering covers the implicit one where the only
            // remaining window of `old` is the one we just re-mapped).
            let destroyed = if let Some(old_id) = destroy_old {
                self.destroy_workspace_if_ephemeral(old_id)
                    .map(|pair| vec![pair])
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            (inserted, destroyed)
        })
    }

    pub fn workspace_for_window(&self, window_id: WindowId) -> Option<VirtualWorkspaceId> {
        self.window_registry
            .get()
            .workspace_info_for_window(window_id)
            .map(|info| info.workspace_id)
    }

    pub fn workspace_for_window_in_space(
        &self,
        space: SpaceId,
        window_id: WindowId,
    ) -> Option<VirtualWorkspaceId> {
        self.window_registry.get().workspace_for_window(space, window_id)
    }

    pub fn workspaces_for_window(&self, window_id: WindowId) -> Vec<VirtualWorkspaceId> {
        self.window_registry.get().workspaces_for_window(window_id)
    }

    pub fn set_last_rule_decision(&mut self, space: SpaceId, window_id: WindowId, value: bool) {
        let _ = space;
        self.window_registry.get_mut().set_last_rule_decision(window_id, value);
    }

    /// Removes `window_id` from its assigned workspace, then runs the
    /// ephemeral destruction guard on that workspace.
    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn remove_window(&mut self, window_id: WindowId) -> Vec<(SpaceId, VirtualWorkspaceId)> {
        let mut touched: Vec<VirtualWorkspaceId> = Vec::new();
        if let Some(assignment) = self.window_registry.get_mut().remove_window_assignment(window_id)
        {
            if let Some(workspace) = self.workspaces.get_mut(assignment.workspace_id) {
                workspace.remove_window(window_id);
            }
            touched.push(assignment.workspace_id);
        }
        self.window_registry.get_mut().clear_rule_metadata(window_id);
        self.destroy_ephemeral_workspaces(touched)
    }

    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn remove_windows_for_app(&mut self, pid: pid_t) -> Vec<(SpaceId, VirtualWorkspaceId)> {
        let windows_to_remove: Vec<_> = self
            .window_registry
            .get()
            .iter_workspace_assignments()
            .map(|(window_id, _)| window_id)
            .filter(|wid| wid.pid == pid)
            .collect();
        let mut touched: Vec<VirtualWorkspaceId> = Vec::new();
        for window_id in windows_to_remove {
            let assignment = self.window_registry.get_mut().remove_window_assignment(window_id);
            if let Some(info) = assignment {
                if let Some(workspace) = self.workspaces.get_mut(info.workspace_id) {
                    workspace.remove_window(window_id);
                }
                touched.push(info.workspace_id);
            }
            self.window_registry.get_mut().clear_rule_metadata(window_id);
        }
        self.destroy_ephemeral_workspaces(touched)
    }

    /// Remove any stale workspace-set membership for windows that no longer
    /// exist in the reactor's WindowManager. This is a repair path for
    /// restored or otherwise inconsistent state; normal window destruction
    /// should go through `remove_window` while the registry assignment is
    /// still present.
    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn prune_windows_not_in(
        &mut self,
        live_windows: &HashSet<WindowId>,
    ) -> (Vec<WindowId>, Vec<(SpaceId, VirtualWorkspaceId)>) {
        let mut windows_to_remove: Vec<WindowId> = self
            .workspaces
            .values()
            .flat_map(|workspace| workspace.windows())
            .filter(|wid| !live_windows.contains(wid))
            .collect();
        windows_to_remove.sort_unstable();
        windows_to_remove.dedup();

        if windows_to_remove.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let mut touched: Vec<VirtualWorkspaceId> = Vec::new();
        for window_id in &windows_to_remove {
            let assigned_workspace =
                self.window_registry.get_mut().remove_window_assignment(*window_id);
            for (workspace_id, workspace) in self.workspaces.iter_mut() {
                if workspace.remove_window(*window_id) {
                    touched.push(workspace_id);
                }
            }
            if let Some(info) = assigned_workspace
                && !touched.contains(&info.workspace_id)
            {
                touched.push(info.workspace_id);
            }
            self.window_registry.get_mut().clear_rule_metadata(*window_id);
            self.remove_floating_position(*window_id);
        }

        let destroyed = self.destroy_ephemeral_workspaces(touched);
        (windows_to_remove, destroyed)
    }

    /// Ephemeral lifecycle: destroy `ws_id` if it has no windows, while
    /// preserving the invariant that every display keeps at least one
    /// workspace. If the empty workspace is currently active and another
    /// workspace is available on the same display, active focus is first
    /// moved to that fallback and the empty workspace is destroyed.
    ///
    /// Returns `Some((space, ws_id))` if the workspace was destroyed, `None`
    /// otherwise. Callers (the layout engine) use the return value to clean
    /// up their per-(space, workspace) mirrors so subsequent layout
    /// operations don't dereference the dead id.
    fn destroy_workspace_if_ephemeral(
        &mut self,
        ws_id: VirtualWorkspaceId,
    ) -> Option<(SpaceId, VirtualWorkspaceId)> {
        if !self.prepare_empty_workspace_for_destroy(ws_id) {
            return None;
        }
        let space = self.destroy_workspace_no_rebuild(ws_id)?;
        Some((space, ws_id))
    }

    /// Force-destroy a workspace even when it's marked active somewhere,
    /// scrubbing the active/last per-display tables first so no stale entry
    /// can later resurrect a dead number.
    ///
    /// Use this from call sites that have already decided the workspace must
    /// die regardless of its active-anywhere state — namely, the SpaceId
    /// remap path (`remap_space`), which replaces whatever was on the target
    /// space with the migrated state from the source space. This is in
    /// contrast to `destroy_ephemeral_workspaces`, which RESPECTS the
    /// active-anywhere guard (an active workspace stays alive even when
    /// empty, because the user is looking at it).
    ///
    /// Failing to purge the active table before destruction would leave
    /// `active_workspace_per_display[uuid]` pointing at the dead
    /// `WorkspaceNumber`; if that number is later reissued (e.g. by
    /// `create_workspace_with_number`), the freshly-created workspace would
    /// silently inherit "active on that display" state on the wrong display.
    fn destroy_workspace_purge_active(&mut self, ws_id: VirtualWorkspaceId) -> Option<SpaceId> {
        let dead_number = self.workspaces.get(ws_id)?.number;
        self.active_workspace_per_display.retain(|_, n| *n != dead_number);
        self.last_workspace_per_display.retain(|_, n| *n != dead_number);
        self.destroy_workspace_no_rebuild(ws_id)
    }

    /// Internal helper. Removes `ws_id` from the SlotMap and all derived
    /// tables (`workspace_by_number`, `display_for_workspace`, registry
    /// workspace assignments, `last_workspace_per_display`). Returns the
    /// destroyed workspace's space id, or None when `ws_id` wasn't present.
    /// Caller is responsible for ensuring `ws_id` isn't currently active
    /// anywhere.
    fn destroy_workspace_no_rebuild(&mut self, ws_id: VirtualWorkspaceId) -> Option<SpaceId> {
        let ws = self.workspaces.remove(ws_id)?;
        for window in &ws.windows {
            let still_points_here = self
                .window_registry
                .get()
                .workspace_info_for_window(*window)
                .is_some_and(|assignment| assignment.workspace_id == ws_id);
            if still_points_here {
                self.window_registry.get_mut().remove_window_assignment(*window);
            }
        }
        self.display_for_workspace.remove(&ws_id);

        let dead_number = ws.number;
        let mut clear_dead_number = false;
        if self.workspace_by_number.get(&dead_number) == Some(&ws_id) {
            self.workspace_by_number.remove(&dead_number);
            clear_dead_number = true;
        }
        if clear_dead_number {
            self.last_workspace_per_display.retain(|_, n| *n != dead_number);
        }
        Some(ws.space)
    }

    fn display_uuid_for_workspace_id(&self, ws_id: VirtualWorkspaceId) -> Option<String> {
        self.display_for_workspace.get(&ws_id).cloned().or_else(|| {
            let space = self.workspaces.get(ws_id)?.space;
            self.display_uuid_for_space.get(&space).cloned()
        })
    }

    fn workspace_ids_for_display(&self, display_uuid: &str) -> Vec<VirtualWorkspaceId> {
        let mut ids: Vec<(WorkspaceNumber, VirtualWorkspaceId)> = self
            .workspaces
            .iter()
            .filter_map(|(id, ws)| {
                let uuid = self
                    .display_for_workspace
                    .get(&id)
                    .or_else(|| self.display_uuid_for_space.get(&ws.space))?;
                if uuid == display_uuid {
                    Some((ws.number, id))
                } else {
                    None
                }
            })
            .collect();
        ids.sort_by_key(|(number, _)| *number);
        ids.into_iter().map(|(_, id)| id).collect()
    }

    fn fallback_workspace_for_display(
        &self,
        display_uuid: &str,
        excluding: VirtualWorkspaceId,
    ) -> Option<VirtualWorkspaceId> {
        let candidates: Vec<VirtualWorkspaceId> = self
            .workspace_ids_for_display(display_uuid)
            .into_iter()
            .filter(|id| *id != excluding)
            .collect();

        let by_number = |number: WorkspaceNumber| {
            candidates
                .iter()
                .copied()
                .find(|id| self.workspaces.get(*id).map(|ws| ws.number) == Some(number))
        };

        candidates
            .iter()
            .copied()
            .find(|id| self.workspaces.get(*id).is_some_and(|ws| !ws.windows.is_empty()))
            .or_else(|| {
                let number = self.last_workspace_per_display.get(display_uuid).copied()?;
                by_number(number)
            })
            .or_else(|| {
                let number = self.active_workspace_per_display.get(display_uuid).copied()?;
                by_number(number)
            })
            .or_else(|| candidates.into_iter().next())
    }

    fn prepare_empty_workspace_for_destroy(&mut self, ws_id: VirtualWorkspaceId) -> bool {
        let Some(ws) = self.workspaces.get(ws_id) else {
            return false;
        };
        if !ws.windows.is_empty() {
            return false;
        }

        let dead_number = ws.number;
        let Some(display_uuid) = self.display_uuid_for_workspace_id(ws_id) else {
            return !self.is_workspace_active_anywhere(ws_id);
        };
        let fallback = self.fallback_workspace_for_display(&display_uuid, ws_id);

        if self.active_workspace_per_display.get(&display_uuid) == Some(&dead_number) {
            let Some(fallback) = fallback else {
                // This is the display's last workspace. Keep it as the
                // required empty placeholder for that display.
                return false;
            };
            let Some(fallback_number) = self.workspaces.get(fallback).map(|ws| ws.number) else {
                return false;
            };
            self.active_workspace_per_display.insert(display_uuid.clone(), fallback_number);
            self.last_workspace_per_display.retain(|_, n| *n != dead_number);
        } else if fallback.is_none() {
            // The workspace is already inactive but still the display's only
            // workspace; keep one workspace alive per display.
            return false;
        }

        !self.is_workspace_active_anywhere(ws_id)
    }

    /// Eligible-only batch destruction. Walks `candidates` and destroys any
    /// that pass the ephemeral guard, returning the (space, ws_id) pairs
    /// that were destroyed. Callers use the returned list to clean their
    /// per-(space, workspace) mirrors.
    fn destroy_ephemeral_workspaces(
        &mut self,
        candidates: impl IntoIterator<Item = VirtualWorkspaceId>,
    ) -> Vec<(SpaceId, VirtualWorkspaceId)> {
        let mut destroyed: Vec<(SpaceId, VirtualWorkspaceId)> = Vec::new();
        for ws_id in candidates {
            if !self.prepare_empty_workspace_for_destroy(ws_id) {
                continue;
            }
            if let Some(space) = self.destroy_workspace_no_rebuild(ws_id) {
                destroyed.push((space, ws_id));
            }
        }
        destroyed
    }

    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn destroy_empty_workspaces(
        &mut self,
        candidates: impl IntoIterator<Item = VirtualWorkspaceId>,
    ) -> Vec<(SpaceId, VirtualWorkspaceId)> {
        self.destroy_ephemeral_workspaces(candidates)
    }

    /// True iff `ws_id` is the currently active workspace on any display
    /// (per the per-display `active_workspace_per_display` table). Cheap
    /// lookup via the workspace's own number.
    fn is_workspace_active_anywhere(&self, ws_id: VirtualWorkspaceId) -> bool {
        let Some(number) = self.workspaces.get(ws_id).map(|ws| ws.number) else {
            return false;
        };
        self.active_workspace_per_display.values().any(|n| *n == number)
    }

    /// Phase 3.4: when display `dead_uuid` goes offline, move every window
    /// living on its workspaces to the active workspace of `receiver_uuid`,
    /// then destroy the dead workspaces (freeing their numbers for re-use
    /// by Cmd+N create-on-demand).
    ///
    /// Returns the (space, ws_id) pairs that were destroyed; the caller
    /// MUST drain the list into `LayoutEngine::drop_workspace_layout` so
    /// the engine's `workspace_layouts` mirror doesn't keep dangling
    /// entries that `rebalance_all_layouts` would later dereference.
    ///
    /// Returns an empty Vec (no migration performed) when:
    /// - the receiver display isn't known to the manager (its space is
    ///   offline or `set_space_display` was never called for it),
    /// - the receiver's space has no active workspace (lazy init never
    ///   happened, e.g. on a freshly-attached display the user never
    ///   touched), or
    /// - no spaces are bound to `dead_uuid` (the unplug already pruned).
    ///
    /// Must run BEFORE `LayoutEngine::prune_display_state`: that prune
    /// clears `display_uuid_for_space` for the dead spaces, after which
    /// this method can no longer find which workspaces belonged to the
    /// dead display.
    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn migrate_workspaces_off_display(
        &mut self,
        dead_uuid: &str,
        receiver_uuid: &str,
    ) -> Vec<(SpaceId, VirtualWorkspaceId)> {
        // Receiver must be live with an active workspace before we touch
        // anything; without it there's nowhere for the windows to land.
        let Some(receiver_space) = self.space_for_display(receiver_uuid) else {
            return Vec::new();
        };
        let Some(receiver_ws) = self.active_workspace(receiver_space) else {
            warn!(
                dead_uuid = %dead_uuid,
                receiver_uuid = %receiver_uuid,
                "migrate_workspaces_off_display: receiver has no active workspace; \
                 no migration will occur (workspaces on dead display will be pruned without re-homing)"
            );
            return Vec::new();
        };

        // Find every space currently bound to the dead display.
        let dead_spaces: Vec<SpaceId> = self
            .display_uuid_for_space
            .iter()
            .filter(|(_, uuid)| uuid.as_str() == dead_uuid)
            .map(|(space, _)| *space)
            .collect();
        if dead_spaces.is_empty() {
            return Vec::new();
        }

        // Clear the dead display's active/last entries up front so the
        // ephemeral guard in `destroy_workspace_no_rebuild` sees these
        // workspaces as inactive.
        self.active_workspace_per_display.remove(dead_uuid);
        self.last_workspace_per_display.remove(dead_uuid);

        let mut destroyed: Vec<(SpaceId, VirtualWorkspaceId)> = Vec::new();
        for dead_space in &dead_spaces {
            let dead_ws_ids: Vec<VirtualWorkspaceId> = self
                .workspaces
                .iter()
                .filter(|(_, ws)| ws.space == *dead_space)
                .map(|(id, _)| id)
                .collect();
            for dead_id in dead_ws_ids {
                let windows: Vec<WindowId> = self
                    .workspaces
                    .get(dead_id)
                    .map(|ws| ws.windows().collect())
                    .unwrap_or_default();
                for win in windows {
                    if let Some(ws) = self.workspaces.get_mut(receiver_ws) {
                        ws.add_window(win);
                    }
                    self.window_registry.get_mut().assign_window_to_workspace(
                        win,
                        WindowWorkspaceInfo {
                            space: receiver_space,
                            workspace_id: receiver_ws,
                        },
                    );
                    self.window_registry.get_mut().clear_rule_metadata(win);
                }
                self.floating_positions.remove(&(*dead_space, dead_id));
                if let Some(space) = self.destroy_workspace_no_rebuild(dead_id) {
                    destroyed.push((space, dead_id));
                }
            }
        }

        destroyed
    }

    /// Gets all windows in the active virtual workspace for a given native space.
    pub fn windows_in_active_workspace(&self, space: SpaceId) -> Vec<WindowId> {
        if let Some(workspace_id) = self.active_workspace(space) {
            if let Some(workspace) = self.workspaces.get(workspace_id) {
                return workspace.windows().collect();
            }
        }
        Vec::new()
    }

    pub fn is_window_in_active_workspace(&self, space: SpaceId, window_id: WindowId) -> bool {
        if let Some(active_workspace_id) = self.active_workspace(space) {
            if let Some(window_workspace_id) = self.workspace_for_window(window_id) {
                return window_workspace_id == active_workspace_id;
            }
        }
        true
    }

    pub fn windows_in_inactive_workspaces(&self, space: SpaceId) -> Vec<WindowId> {
        let active_workspace_id = self.active_workspace(space);

        self.workspaces
            .iter()
            .filter(|(id, workspace)| workspace.space == space && Some(*id) != active_workspace_id)
            .flat_map(|(_, workspace)| workspace.windows())
            .collect()
    }

    pub fn find_window_by_idx(&self, space: SpaceId, idx: u32) -> Option<WindowId> {
        self.window_registry
            .get()
            .iter_workspace_assignments()
            .find_map(|(wid, info)| (info.space == space && wid.idx.get() == idx).then_some(wid))
    }

    pub fn find_window_by_pid_idx(
        &self,
        space: SpaceId,
        pid: pid_t,
        idx: u32,
    ) -> Option<WindowId> {
        self.window_registry.get().iter_workspace_assignments().find_map(|(wid, info)| {
            (info.space == space && wid.pid == pid && wid.idx.get() == idx).then_some(wid)
        })
    }

    /// Like `find_window_by_idx` but unscoped — scans every registry
    /// workspace assignment. Returns the matching `(SpaceId, WindowId)` so
    /// callers know which space owns the window without rerunning a per-space
    /// lookup; the SpaceId is derived from the owning workspace's `.space`
    /// field. Used by `MoveWindowToWorkspace` after the command-space scoped
    /// lookup misses, so a Cmd+Shift+N from a display other than the window's
    /// display still finds the window AND the correct source space — needed
    /// because `LayoutEngine::space_with_window` only inspects ACTIVE
    /// workspaces and silently misses windows in inactive workspaces.
    /// Mirrors the fallback pattern in
    /// `handle_command_reactor_move_window_to_display`. Returns the first
    /// match in iteration order; idx is window-internal so duplicates across
    /// pids are possible — first-match is fine for the move commands'
    /// purposes.
    pub fn find_window_anywhere_by_idx(&self, idx: u32) -> Option<(SpaceId, WindowId)> {
        self.window_registry.get().iter_workspace_assignments().find_map(|(wid, info)| {
            if wid.idx.get() != idx {
                return None;
            }
            Some((info.space, wid))
        })
    }

    pub fn find_window_anywhere_by_pid_idx(
        &self,
        pid: pid_t,
        idx: u32,
    ) -> Option<(SpaceId, WindowId)> {
        self.window_registry.get().iter_workspace_assignments().find_map(|(wid, info)| {
            if wid.pid != pid || wid.idx.get() != idx {
                return None;
            }
            Some((info.space, wid))
        })
    }

    pub fn find_window_in_workspace_by_idx(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        idx: u32,
    ) -> Option<WindowId> {
        if self.workspaces.get(workspace_id).map(|w| w.space) != Some(space) {
            return None;
        }

        self.workspaces
            .get(workspace_id)
            .and_then(|ws| ws.windows().find(|wid| wid.idx.get() == idx))
    }

    fn hidden_rect_for_corner(
        screen_frame: CGRect,
        original_size: CGSize,
        corner: HideCorner,
        app_bundle_id: Option<&str>,
    ) -> CGRect {
        let one_pixel_offset = if let Some(bundle_id) = app_bundle_id {
            match bundle_id {
                "us.zoom.xos" => CGPoint::new(0.0, 0.0),
                _ => match corner {
                    HideCorner::BottomLeft => CGPoint::new(1.0, -1.0),
                    HideCorner::BottomRight => CGPoint::new(1.0, 1.0),
                },
            }
        } else {
            match corner {
                HideCorner::BottomLeft => CGPoint::new(1.0, -1.0),
                HideCorner::BottomRight => CGPoint::new(1.0, 1.0),
            }
        };

        let hidden_point = match corner {
            HideCorner::BottomLeft => {
                let bottom_left = CGPoint::new(screen_frame.origin.x, screen_frame.max().y);
                CGPoint::new(
                    bottom_left.x + one_pixel_offset.x - original_size.width + 1.0,
                    bottom_left.y + one_pixel_offset.y,
                )
            }
            HideCorner::BottomRight => {
                let bottom_right = CGPoint::new(screen_frame.max().x, screen_frame.max().y);
                CGPoint::new(
                    bottom_right.x - one_pixel_offset.x - 1.0,
                    bottom_right.y - one_pixel_offset.y,
                )
            }
        };

        CGRect::new(hidden_point, original_size)
    }

    fn intersection_area(a: CGRect, b: CGRect) -> f64 {
        let w: f64 = (a.max().x.min(b.max().x) - a.origin.x.max(b.origin.x)).max(0.0);
        let h: f64 = (a.max().y.min(b.max().y) - a.origin.y.max(b.origin.y)).max(0.0);
        w * h
    }

    fn choose_hidden_position(
        &self,
        screen_frame: CGRect,
        original_size: CGSize,
        corner: HideCorner,
        app_bundle_id: Option<&str>,
        other_screens: &[CGRect],
    ) -> CGRect {
        const MIN_ANCHOR_AREA: f64 = 1.0;
        let primary =
            Self::hidden_rect_for_corner(screen_frame, original_size, corner, app_bundle_id);
        let fallback = Self::hidden_rect_for_corner(
            screen_frame,
            original_size,
            corner.opposite(),
            app_bundle_id,
        );

        let primary_anchor = Self::intersection_area(screen_frame, primary);
        let fallback_anchor = Self::intersection_area(screen_frame, fallback);
        let primary_anchored = primary_anchor >= MIN_ANCHOR_AREA;
        let fallback_anchored = fallback_anchor >= MIN_ANCHOR_AREA;

        let mut primary_other_max: f64 = 0.0;
        let mut fallback_other_max: f64 = 0.0;
        for screen in other_screens {
            primary_other_max = primary_other_max.max(Self::intersection_area(*screen, primary));
            fallback_other_max = fallback_other_max.max(Self::intersection_area(*screen, fallback));
        }

        match (primary_anchored, fallback_anchored) {
            (true, false) => primary,
            (false, true) => fallback,
            (true, true) => {
                if (primary_other_max - fallback_other_max).abs() > f64::EPSILON {
                    if primary_other_max < fallback_other_max {
                        primary
                    } else {
                        fallback
                    }
                } else if primary_anchor <= fallback_anchor {
                    primary
                } else {
                    fallback
                }
            }
            (false, false) => {
                if primary_other_max <= fallback_other_max {
                    primary
                } else {
                    fallback
                }
            }
        }
    }

    pub fn calculate_hidden_position(
        &self,
        screen_frame: CGRect,
        original_size: CGSize,
        corner: HideCorner,
        app_bundle_id: Option<&str>,
    ) -> CGRect {
        self.choose_hidden_position(screen_frame, original_size, corner, app_bundle_id, &[])
    }

    pub fn calculate_hidden_position_multi(
        &self,
        screen_frame: CGRect,
        original_size: CGSize,
        corner: HideCorner,
        app_bundle_id: Option<&str>,
        all_screens: &[CGRect],
    ) -> CGRect {
        let other_screens: Vec<CGRect> =
            all_screens.iter().copied().filter(|screen| *screen != screen_frame).collect();
        self.choose_hidden_position(
            screen_frame,
            original_size,
            corner,
            app_bundle_id,
            &other_screens,
        )
    }

    pub fn is_hidden_position(
        &self,
        screen_frame: &CGRect,
        rect: &CGRect,
        app_bundle_id: Option<&str>,
    ) -> bool {
        const VISIBLE_THRESHOLD_PX: f64 = 3.0;
        let hidden_rect = self.choose_hidden_position(
            *screen_frame,
            rect.size,
            HideCorner::BottomRight,
            app_bundle_id,
            &[],
        );
        if rect.origin == hidden_rect.origin && rect.size == hidden_rect.size {
            return true;
        }

        let visible_width = (rect.max().x.min(screen_frame.max().x)
            - rect.origin.x.max(screen_frame.origin.x))
        .max(0.0);
        let visible_height = (rect.max().y.min(screen_frame.max().y)
            - rect.origin.y.max(screen_frame.origin.y))
        .max(0.0);
        visible_width <= VISIBLE_THRESHOLD_PX && visible_height <= VISIBLE_THRESHOLD_PX
    }

    pub fn is_hidden_position_multi(
        &self,
        screen_frame: &CGRect,
        rect: &CGRect,
        app_bundle_id: Option<&str>,
        all_screens: &[CGRect],
    ) -> bool {
        const VISIBLE_THRESHOLD_PX: f64 = 3.0;
        let other_screens: Vec<CGRect> =
            all_screens.iter().copied().filter(|screen| *screen != *screen_frame).collect();
        let hidden_rect = self.choose_hidden_position(
            *screen_frame,
            rect.size,
            HideCorner::BottomRight,
            app_bundle_id,
            &other_screens,
        );
        if rect.origin == hidden_rect.origin && rect.size == hidden_rect.size {
            return true;
        }

        let visible_width = (rect.max().x.min(screen_frame.max().x)
            - rect.origin.x.max(screen_frame.origin.x))
        .max(0.0);
        let visible_height = (rect.max().y.min(screen_frame.max().y)
            - rect.origin.y.max(screen_frame.origin.y))
        .max(0.0);
        visible_width <= VISIBLE_THRESHOLD_PX && visible_height <= VISIBLE_THRESHOLD_PX
    }

    pub fn set_last_focused_window(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        window_id: Option<WindowId>,
    ) {
        if self.workspaces.get(workspace_id).map(|w| w.space) == Some(space) {
            if let Some(workspace) = self.workspaces.get_mut(workspace_id) {
                workspace.set_last_focused(window_id);
            }
        }
    }

    pub fn last_focused_window(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
    ) -> Option<WindowId> {
        if self.workspaces.get(workspace_id).map(|w| w.space) == Some(space) {
            self.workspaces.get(workspace_id)?.last_focused()
        } else {
            None
        }
    }

    pub fn workspace_info(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
    ) -> Option<&VirtualWorkspace> {
        if self.workspaces.get(workspace_id).map(|w| w.space) == Some(space) {
            self.workspaces.get(workspace_id)
        } else {
            None
        }
    }

    pub fn store_floating_position(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        window_id: WindowId,
        position: CGRect,
    ) {
        let key = (space, workspace_id);
        self.floating_positions
            .entry(key)
            .or_default()
            .store_position(window_id, position);
    }

    pub fn store_floating_position_if_absent(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        window_id: WindowId,
        position: CGRect,
    ) {
        let key = (space, workspace_id);
        self.floating_positions
            .entry(key)
            .or_default()
            .store_if_absent(window_id, position);
    }

    pub fn get_floating_position(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        window_id: WindowId,
    ) -> Option<CGRect> {
        let key = (space, workspace_id);
        self.floating_positions.get(&key)?.get_position(window_id)
    }

    pub fn store_current_floating_positions(
        &mut self,
        space: SpaceId,
        floating_windows: &[(WindowId, CGRect)],
    ) {
        if let Some(workspace_id) = self.active_workspace(space) {
            let key = (space, workspace_id);
            let positions = self.floating_positions.entry(key).or_default();

            for &(window_id, position) in floating_windows {
                positions.store_position(window_id, position);
            }
        }
    }

    pub fn get_workspace_floating_positions(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
    ) -> Vec<(WindowId, CGRect)> {
        let key = (space, workspace_id);
        if let Some(positions) = self.floating_positions.get(&key) {
            positions
                .windows()
                .filter_map(|window_id| {
                    positions.get_position(window_id).map(|position| (window_id, position))
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn remove_floating_position(&mut self, window_id: WindowId) {
        for positions in self.floating_positions.values_mut() {
            positions.remove_position(window_id);
        }
    }

    pub fn remove_app_floating_positions(&mut self, pid: pid_t) {
        for positions in self.floating_positions.values_mut() {
            positions.remove_app_windows(pid);
        }
    }

    pub fn list_workspaces(&mut self, space: SpaceId) -> Vec<(VirtualWorkspaceId, String)> {
        self.ensure_space_initialized(space);
        self.workspaces_for_space(space)
    }

    pub fn rename_workspace(
        &mut self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
        new_name: String,
    ) -> bool {
        if self.workspaces.get(workspace_id).map(|w| w.space) != Some(space) {
            return false;
        }
        if let Some(workspace) = self.workspaces.get_mut(workspace_id) {
            workspace.name = new_name;

            true
        } else {
            false
        }
    }

    pub fn workspace_windows(
        &self,
        space: SpaceId,
        workspace_id: VirtualWorkspaceId,
    ) -> Vec<WindowId> {
        if let Some(workspace) = self.workspaces.get(workspace_id) {
            if workspace.space == space {
                let mut windows: Vec<WindowId> = workspace.windows().collect();
                windows.sort_unstable_by_key(|wid| wid.idx.get());
                return windows;
            }
        }
        Vec::new()
    }

    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn auto_assign_window(
        &mut self,
        window_id: WindowId,
        space: SpaceId,
    ) -> Result<(VirtualWorkspaceId, Vec<(SpaceId, VirtualWorkspaceId)>), WorkspaceError> {
        let default_workspace_id = self.get_default_workspace(space)?;
        let (assigned, destroyed) =
            self.assign_window_to_workspace(space, window_id, default_workspace_id);
        if assigned {
            self.window_registry.get_mut().clear_rule_floating(window_id);
            Ok((default_workspace_id, destroyed))
        } else {
            Err(WorkspaceError::AssignmentFailed)
        }
    }

    #[must_use = "destroyed workspaces must be propagated to LayoutEngine::drop_workspace_layout"]
    pub fn assign_window_with_app_info(
        &mut self,
        window_id: WindowId,
        space: SpaceId,
        app_bundle_id: Option<&str>,
        app_name: Option<&str>,
        window_title: Option<&str>,
        ax_role: Option<&str>,
        ax_subrole: Option<&str>,
    ) -> Result<(AppRuleResult, Vec<(SpaceId, VirtualWorkspaceId)>), WorkspaceError> {
        let prev_rule_decision = self.window_registry.get().last_rule_decision(window_id);

        self.ensure_space_initialized(space);
        if !self.workspaces.iter().any(|(_, ws)| ws.space == space) {
            return Err(WorkspaceError::NoWorkspacesAvailable);
        }

        let rule_match = self
            .find_matching_app_rule(app_bundle_id, app_name, window_title, ax_role, ax_subrole)
            .cloned();

        // Existing assignment is only meaningful when the window is already
        // assigned to a workspace on THIS space — assignments on other spaces
        // mean the window is moving to a new space, which goes through the
        // reassignment path below. (Pre-Task-4.3 this was implicit in the
        // `(space, window_id)` key.)
        let existing_assignment = self
            .workspace_for_window(window_id)
            .filter(|ws_id| self.workspaces.get(*ws_id).map(|ws| ws.space) == Some(space));

        if let Some(rule) = rule_match {
            if !rule.manage {
                self.window_registry.get_mut().clear_rule_floating(window_id);
                return Ok((AppRuleResult::Unmanaged, Vec::new()));
            }

            let target_workspace_id = if let Some(ref ws_sel) = rule.workspace {
                // `WorkspaceSelector::Index(N)` in an app_rule is the global
                // `WorkspaceNumber` — same digit the user types for Cmd+N.
                // `Name(name)` still resolves per-space against the source
                // space's workspace list (no global name index exists in the
                // post-Phase-4 model). Lookup-only here: if the workspace
                // does not exist, the engine wrapper is responsible for
                // create-on-demand because layout-tree wiring needs the
                // screen size.
                let resolved_id: Option<VirtualWorkspaceId> = match ws_sel {
                    WorkspaceSelector::Index(i) => {
                        self.resolve_workspace(*i as WorkspaceNumber).map(|t| t.workspace_id)
                    }
                    WorkspaceSelector::Name(name) => {
                        let workspaces = self.list_workspaces(space);
                        match workspaces.iter().find(|(_, n)| n == name) {
                            Some((id, _)) => Some(*id),
                            None => {
                                tracing::warn!(
                                    "App rule references workspace name '{}' which could not be resolved on space {:?}; falling back to default workspace",
                                    name,
                                    space
                                );
                                None
                            }
                        }
                    }
                };

                if let Some(workspace_id) = resolved_id {
                    workspace_id
                } else if let Some(existing_ws) = existing_assignment {
                    existing_ws
                } else {
                    if matches!(ws_sel, WorkspaceSelector::Index(_)) {
                        tracing::warn!(
                            "App rule references workspace number {:?} which has no live workspace; \
                             falling back to default workspace on space {:?}",
                            ws_sel,
                            space
                        );
                    }
                    self.get_default_workspace(space)?
                }
            } else {
                if let Some(existing_ws) = existing_assignment {
                    existing_ws
                } else {
                    self.get_default_workspace(space)?
                }
            };

            if let Some(existing_ws) = existing_assignment {
                self.window_registry.get_mut().set_rule_floating(window_id, rule.floating);
                return Ok((
                    AppRuleResult::Managed(AppRuleAssignment {
                        workspace_id: existing_ws,
                        floating: rule.floating,
                        prev_rule_decision,
                    }),
                    Vec::new(),
                ));
            }

            let (assigned, destroyed) =
                self.assign_window_to_workspace(space, window_id, target_workspace_id);
            if assigned {
                self.window_registry.get_mut().set_rule_floating(window_id, rule.floating);
                return Ok((
                    AppRuleResult::Managed(AppRuleAssignment {
                        workspace_id: target_workspace_id,
                        floating: rule.floating,
                        prev_rule_decision,
                    }),
                    destroyed,
                ));
            } else {
                error!("Failed to assign window to workspace from app rule");
            }
        }

        if let Some(existing_ws) = existing_assignment {
            self.window_registry.get_mut().clear_rule_floating(window_id);
            return Ok((
                AppRuleResult::Managed(AppRuleAssignment {
                    workspace_id: existing_ws,
                    floating: false,
                    prev_rule_decision,
                }),
                Vec::new(),
            ));
        }

        let default_workspace_id = self.get_default_workspace(space)?;
        let (assigned, destroyed) =
            self.assign_window_to_workspace(space, window_id, default_workspace_id);
        if assigned {
            self.window_registry.get_mut().clear_rule_floating(window_id);
            Ok((
                AppRuleResult::Managed(AppRuleAssignment {
                    workspace_id: default_workspace_id,
                    floating: false,
                    prev_rule_decision,
                }),
                destroyed,
            ))
        } else {
            error!("Failed to assign window to default workspace");
            Err(WorkspaceError::AssignmentFailed)
        }
    }

    fn get_default_workspace(
        &mut self,
        space: SpaceId,
    ) -> Result<VirtualWorkspaceId, WorkspaceError> {
        self.ensure_space_initialized(space);
        if let Some(active_workspace_id) = self.active_workspace(space) {
            if self.workspaces.contains_key(active_workspace_id) {
                return Ok(active_workspace_id);
            } else {
                warn!("Active workspace no longer exists, clearing reference");
                if let Some(uuid) = self.display_uuid_for_space.get(&space).cloned() {
                    self.active_workspace_per_display.remove(&uuid);
                }
            }
        }

        let first_id = self
            .workspaces_for_space(space)
            .into_iter()
            .next()
            .map(|(id, _)| id)
            .or_else(|| self.workspaces.iter().find(|(_, ws)| ws.space == space).map(|(id, _)| id))
            .ok_or_else(|| {
                WorkspaceError::InconsistentState("No workspaces for space".to_string())
            })?;

        if self.set_active_workspace(space, first_id) {
            Ok(first_id)
        } else {
            Err(WorkspaceError::InconsistentState(
                "Failed to set default workspace as active".to_string(),
            ))
        }
    }

    fn find_matching_app_rule(
        &self,
        app_bundle_id: Option<&str>,
        app_name: Option<&str>,
        window_title: Option<&str>,
        ax_role: Option<&str>,
        ax_subrole: Option<&str>,
    ) -> Option<&AppWorkspaceRule> {
        let mut matches: Vec<(usize, &AppWorkspaceRule, usize)> = Vec::new();

        for (idx, rule) in self.app_rules.iter().enumerate() {
            if let Some(ref rule_app_id) = rule.app_id {
                match app_bundle_id {
                    Some(bundle_id) if rule_app_id.eq_ignore_ascii_case(bundle_id) => {}
                    _ => continue,
                }
            }

            if let Some(ref rule_name) = rule.app_name {
                match app_name {
                    Some(name) => {
                        let name_l = name.to_lowercase();
                        let rule_name_l = rule_name.to_lowercase();
                        if !(name_l.contains(&rule_name_l) || rule_name_l.contains(&name_l)) {
                            continue;
                        }
                    }
                    None => continue,
                }
            }

            if let Some(ref rule_re) = rule.title_regex {
                if rule_re.is_empty() {
                    continue;
                }
                match window_title {
                    Some(title) => match self.app_rule_regex_cache.get(idx) {
                        Some(Some(re)) => {
                            if !re.is_match(title) {
                                continue;
                            }
                        }
                        _ => continue,
                    },
                    None => continue,
                }
            }

            // Case-insensitive substring matching for title_substring
            if let Some(ref title_sub) = rule.title_substring {
                if title_sub.is_empty() {
                    continue;
                }
                match window_title {
                    Some(title) => {
                        let title_l = title.to_lowercase();
                        let sub_l = title_sub.to_lowercase();
                        if !title_l.contains(&sub_l) {
                            continue;
                        }
                    }
                    None => continue,
                }
            }

            if let Some(ref rule_ax_role) = rule.ax_role {
                if rule_ax_role.is_empty() {
                    continue;
                }
                match ax_role {
                    Some(r) => {
                        if r != rule_ax_role.as_str() {
                            continue;
                        }
                    }
                    None => continue,
                }
            }

            if let Some(ref rule_ax_sub) = rule.ax_subrole {
                if rule_ax_sub.is_empty() {
                    continue;
                }
                match ax_subrole {
                    Some(sr) => {
                        if sr != rule_ax_sub.as_str() {
                            continue;
                        }
                    }
                    None => continue,
                }
            }

            let mut score = 0usize;
            if rule.app_id.as_ref().map_or(false, |s| !s.is_empty()) {
                score += 1;
            }
            if rule.app_name.as_ref().map_or(false, |s| !s.is_empty()) {
                score += 1;
            }
            if rule.title_regex.as_ref().map_or(false, |s| !s.is_empty()) {
                score += 1;
            }
            if rule.title_substring.as_ref().map_or(false, |s| !s.is_empty()) {
                score += 1;
            }
            if rule.ax_role.as_ref().map_or(false, |s| !s.is_empty()) {
                score += 1;
            }
            if rule.ax_subrole.as_ref().map_or(false, |s| !s.is_empty()) {
                score += 1;
            }

            matches.push((idx, rule, score));
        }

        if matches.is_empty() {
            return None;
        }

        if matches.len() == 1 {
            return Some(matches[0].1);
        }

        let mut groups: HashMap<&str, Vec<&(usize, &AppWorkspaceRule, usize)>> = HashMap::default();
        for entry in &matches {
            if let Some(ref app_id) = entry.1.app_id {
                if !app_id.is_empty() {
                    groups.entry(app_id.as_str()).or_default().push(entry);
                }
            }
        }

        if !groups.is_empty() {
            let mut candidate_group_key: Option<&str> = None;
            let mut candidate_group_first_idx: Option<usize> = None;

            for (key, vec_entries) in groups.iter() {
                if vec_entries.len() > 1 {
                    let first_idx = vec_entries.iter().map(|e| e.0).min().unwrap_or(usize::MAX);
                    if candidate_group_key.is_none()
                        || first_idx < candidate_group_first_idx.unwrap()
                    {
                        candidate_group_key = Some(*key);
                        candidate_group_first_idx = Some(first_idx);
                    }
                }
            }

            if let Some(key) = candidate_group_key {
                if let Some(vec_entries) = groups.get(key) {
                    let best = vec_entries.iter().copied().max_by(|a, b| match a.2.cmp(&b.2) {
                        std::cmp::Ordering::Equal => b.0.cmp(&a.0), // prefer earlier-defined rule on tie
                        ord => ord,
                    });
                    if let Some(best_entry) = best {
                        return Some(best_entry.1);
                    }
                }
            }
        }

        let best_overall = matches.iter().max_by(|a, b| match a.2.cmp(&b.2) {
            std::cmp::Ordering::Equal => b.0.cmp(&a.0), // prefer earlier-defined rule on tie
            ord => ord,
        });

        best_overall.map(|(_, rule, _)| *rule)
    }

    /// Engine-facing peek for app_rule-driven global workspace routing:
    /// returns the `WorkspaceNumber` the matching rule's `workspace =`
    /// selector targets, or `None` if no rule matches OR the rule does not
    /// set `workspace`, OR the selector is `Name(_)` (names do not have a
    /// global pool; they resolve per-space inside `assign_window_with_app_info`
    /// itself). The engine uses this to resolve or create the numeric target
    /// before invoking `assign_window_with_app_info` on the correct space.
    pub fn peek_app_rule_workspace_number(
        &self,
        app_bundle_id: Option<&str>,
        app_name: Option<&str>,
        window_title: Option<&str>,
        ax_role: Option<&str>,
        ax_subrole: Option<&str>,
    ) -> Option<WorkspaceNumber> {
        let rule = self.find_matching_app_rule(
            app_bundle_id,
            app_name,
            window_title,
            ax_role,
            ax_subrole,
        )?;
        if !rule.manage {
            return None;
        }
        match rule.workspace.as_ref()? {
            WorkspaceSelector::Index(i) => Some(*i as WorkspaceNumber),
            WorkspaceSelector::Name(_) => None,
        }
    }

    pub fn get_stats(&self) -> WorkspaceStats {
        let mut stats = WorkspaceStats {
            total_workspaces: self.workspaces.len(),
            total_windows: self.window_registry.get().workspace_assignment_count(),
            active_spaces: self.active_workspace_per_display.len(),
            workspace_window_counts: HashMap::default(),
        };

        for (workspace_id, workspace) in &self.workspaces {
            stats.workspace_window_counts.insert(workspace_id, workspace.window_count());
        }

        stats
    }
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FloatingWindowPositions {
    #[serde_as(as = "HashMap<_, CGRectDef>")]
    positions: HashMap<WindowId, CGRect>,
}

impl FloatingWindowPositions {
    fn store_position(&mut self, window_id: WindowId, position: CGRect) {
        self.positions.insert(window_id, position);
    }

    fn store_if_absent(&mut self, window_id: WindowId, position: CGRect) {
        self.positions.entry(window_id).or_insert(position);
    }

    fn get_position(&self, window_id: WindowId) -> Option<CGRect> {
        self.positions.get(&window_id).copied()
    }

    fn remove_position(&mut self, window_id: WindowId) -> Option<CGRect> {
        self.positions.remove(&window_id)
    }

    fn windows(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.positions.keys().copied()
    }

    fn remove_app_windows(&mut self, pid: pid_t) {
        self.positions.retain(|window_id, _| window_id.pid != pid);
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceStats {
    pub total_workspaces: usize,
    pub total_windows: usize,
    pub active_spaces: usize,
    pub workspace_window_counts: HashMap<VirtualWorkspaceId, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::app::WindowId;
    use crate::sys::screen::SpaceId;

    /// Phase 3: each space lazy-inits with exactly ONE default workspace.
    /// Tests written for the old pre-allocation model that index into
    /// `list_workspaces(space).get(N)` for N>0 must explicitly create the
    /// extra slots first via `create_workspace_with_number`. This helper
    /// pads `space` until `list_workspaces(space).len() >= target_count`.
    /// Requires `set_space_display(space, Some(uuid))` to have been called.
    #[allow(dead_code)]
    fn pad_workspaces_to(
        manager: &mut VirtualWorkspaceManager,
        space: SpaceId,
        target_count: usize,
    ) {
        let uuid = manager
            .space_display(space)
            .expect("set_space_display(space, …) before pad_workspaces_to")
            .to_owned();
        // Force lazy init so we don't double-create the default workspace.
        let _ = manager.list_workspaces(space);
        while manager.list_workspaces(space).len() < target_count {
            // Pick a number not already in workspace_by_number; this gives
            // each manually-created slot a unique global number while keeping
            // per-space-index ordering stable.
            let mut n: WorkspaceNumber = 0;
            while manager.workspace_by_number.contains_key(&n) {
                n += 1;
            }
            manager.create_workspace_with_number(n, &uuid, space);
        }
    }

    #[test]
    fn test_virtual_workspace_creation() {
        let mut manager = VirtualWorkspaceManager::new();

        let space = SpaceId::new(1);
        // Pre-creation: list_workspaces lazy-inits a default workspace.
        assert_eq!(manager.list_workspaces(space).len(), 1);

        let ws_id = manager.create_workspace(space, Some("Test Workspace".to_string())).unwrap();
        assert!(
            manager
                .list_workspaces(space)
                .iter()
                .any(|(id, name)| *id == ws_id && name == "Test Workspace")
        );

        let workspace = manager.workspace_info(space, ws_id).unwrap();
        assert_eq!(workspace.name, "Test Workspace");
    }

    #[test]
    fn workspace_layout_mode_deserializes_as_runtime_bsp() {
        let encoded = serde_json::json!({
            "number": 1,
            "name": "legacy",
            "space": 1,
            "windows": [],
            "last_focused": null,
            "layout_system": { "kind": "scrolling" },
            "layout_mode": "scrolling"
        });
        let decoded: VirtualWorkspace = serde_json::from_value(encoded).unwrap();

        assert_eq!(decoded.layout_mode(), LayoutMode::Bsp);
    }

    #[test]
    fn test_window_assignment() {
        let mut manager = VirtualWorkspaceManager::new();
        let space = SpaceId::new(1);
        let ws1_id = manager.create_workspace(space, Some("WS1".to_string())).unwrap();
        let ws2_id = manager.create_workspace(space, Some("WS2".to_string())).unwrap();

        let window1 = WindowId::new(1, 1);
        let window2 = WindowId::new(1, 2);

        assert!(manager.assign_window_to_workspace(space, window1, ws1_id).0);
        assert!(manager.assign_window_to_workspace(space, window2, ws2_id).0);

        assert_eq!(manager.workspace_for_window(window1), Some(ws1_id));
        assert_eq!(manager.workspace_for_window(window2), Some(ws2_id));

        let ws1 = manager.workspace_info(space, ws1_id).unwrap();
        let ws2 = manager.workspace_info(space, ws2_id).unwrap();

        assert!(ws1.contains_window(window1));
        assert!(!ws1.contains_window(window2));
        assert!(ws2.contains_window(window2));
        assert!(!ws2.contains_window(window1));
    }

    #[test]
    fn test_active_workspace_switching() {
        let mut manager = VirtualWorkspaceManager::new();
        let space = SpaceId::new(1);
        let ws1_id = manager.create_workspace(space, Some("WS1".to_string())).unwrap();
        let ws2_id = manager.create_workspace(space, Some("WS2".to_string())).unwrap();

        assert!(manager.set_active_workspace(space, ws1_id));
        assert_eq!(manager.active_workspace(space), Some(ws1_id));

        assert!(manager.set_active_workspace(space, ws2_id));
        assert_eq!(manager.active_workspace(space), Some(ws2_id));
    }

    #[test]
    fn test_window_visibility() {
        fn is_window_visible(
            wm: &VirtualWorkspaceManager,
            window_id: WindowId,
            space: SpaceId,
        ) -> bool {
            let window_workspace = wm.workspace_for_window(window_id);
            let active_workspace = wm.active_workspace(space);

            match (window_workspace, active_workspace) {
                (Some(window_ws), Some(active_ws)) => window_ws == active_ws,
                _ => true,
            }
        }
        let mut manager = VirtualWorkspaceManager::new();
        let space = SpaceId::new(1);
        let ws1_id = manager.create_workspace(space, Some("WS1".to_string())).unwrap();
        let ws2_id = manager.create_workspace(space, Some("WS2".to_string())).unwrap();
        let window1 = WindowId::new(1, 1);
        let window2 = WindowId::new(1, 2);

        manager.set_active_workspace(space, ws1_id);
        let (_, destroyed1) = manager.assign_window_to_workspace(space, window1, ws1_id);
        let (_, destroyed2) = manager.assign_window_to_workspace(space, window2, ws2_id);
        // Both windows are fresh, so existing_mapping is None and no
        // prior workspace can be torn down.
        debug_assert!(destroyed1.is_empty(), "fresh window cannot vacate any workspace");
        debug_assert!(destroyed2.is_empty(), "fresh window cannot vacate any workspace");

        assert!(is_window_visible(&manager, window1, space));
        assert!(!is_window_visible(&manager, window2, space));

        manager.set_active_workspace(space, ws2_id);
        assert!(!is_window_visible(&manager, window1, space));
        assert!(is_window_visible(&manager, window2, space));
    }

    #[test]
    fn test_workspace_navigation() {
        let mut manager = VirtualWorkspaceManager::new();
        let space = SpaceId::new(1);
        let ws1_id = manager.create_workspace(space, Some("WS1".to_string())).unwrap();
        let ws2_id = manager.create_workspace(space, Some("WS2".to_string())).unwrap();
        let ws3_id = manager.create_workspace(space, Some("WS3".to_string())).unwrap();

        assert_eq!(manager.next_workspace(space, ws1_id, None), Some(ws2_id));
        assert_eq!(manager.next_workspace(space, ws2_id, None), Some(ws3_id));

        assert_eq!(manager.prev_workspace(space, ws2_id, None), Some(ws1_id));
        assert_eq!(manager.prev_workspace(space, ws3_id, None), Some(ws2_id));
    }

    /// Phase 4: the new-model resolver uses `workspace_by_number` directly.
    /// Cmd+N picks the workspace with `number == N`, regardless of source
    /// display.
    #[test]
    fn resolve_workspace_returns_global_workspace() {
        let mut manager = VirtualWorkspaceManager::new();
        let space_a = SpaceId::new(1);
        let space_b = SpaceId::new(2);
        manager.set_space_display(space_a, Some("display-A".to_string()));
        manager.set_space_display(space_b, Some("display-B".to_string()));
        // Create a workspace on each display with a chosen number.
        let ws1 = manager.create_workspace_with_number(1, "display-A", space_a);
        let ws2 = manager.create_workspace_with_number(2, "display-B", space_b);

        let target1 = manager.resolve_workspace(1).expect("ws1 resolves");
        assert_eq!(target1.display_uuid, "display-A");
        assert_eq!(target1.space, space_a);
        assert_eq!(target1.workspace_id, ws1);

        let target2 = manager.resolve_workspace(2).expect("ws2 resolves");
        assert_eq!(target2.display_uuid, "display-B");
        assert_eq!(target2.space, space_b);
        assert_eq!(target2.workspace_id, ws2);

        // Unallocated workspace number returns None — the create-on-demand
        // path is the caller's responsibility.
        assert!(manager.resolve_workspace(7).is_none());
    }

    /// `display_default_workspaces` pin: lazy init of a space whose display
    /// uuid has a pinned default uses that workspace number.
    #[test]
    fn display_default_workspaces_pin_overrides_smallest_unused() {
        let mut config = VirtualWorkspaceSettings::default();
        config.display_default_workspaces.insert("display-LEFT".to_string(), 5);
        let mut manager =
            VirtualWorkspaceManager::new_with_config(&config, &LayoutSettings::default());

        let space_left = SpaceId::new(1);
        manager.set_space_display(space_left, Some("display-LEFT".to_string()));
        let _ = manager.list_workspaces(space_left);

        // The pin selects workspace number 5, not the smallest-unused 0.
        let target = manager.resolve_workspace(5).expect("pinned ws resolves");
        assert_eq!(target.display_uuid, "display-LEFT");
        assert_eq!(target.space, space_left);
    }

    /// Phase 4+: `WorkspaceSelector::Index(N)` inside an `app_rule` is the
    /// global `WorkspaceNumber`, matching the digit-row hotkeys
    /// (`SwitchToGlobalSlot` / `MoveWindowToWorkspace`). Pre-fix the lookup
    /// used `list_workspaces(space).get(N)` — a per-space positional index.
    /// `workspaces_for_space` sorts by `WorkspaceNumber` ascending, so once
    /// the space holds more than one workspace, position N stops matching
    /// number N. Setup: create ws#5 first, then ws#1 — `workspaces_for_space`
    /// orders them as `[ws#1, ws#5]`, and pre-fix `get(1)` would have routed
    /// `workspace = 1` to ws#5. The fix uses `resolve_workspace(N)`, keyed
    /// on real `WorkspaceNumber` via `workspace_by_number`.
    #[test]
    fn app_rule_workspace_index_resolves_globally() {
        let rule = AppWorkspaceRule {
            app_id: Some("com.example.foo".to_string()),
            workspace: Some(WorkspaceSelector::Index(1)),
            floating: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
        };
        let mut manager =
            VirtualWorkspaceManager::new_with_rules(vec![rule], LayoutSettings::default());

        let space = SpaceId::new(1);
        manager.set_space_display(space, Some("display-A".to_string()));

        // Create ws#5 first so it occupies position 0; then ws#1 lands at
        // position 1 (workspaces_for_space orders by number ascending, but
        // pre-fix the lookup used insertion order — making position != number).
        let _ws5 = manager.create_workspace_with_number(5, "display-A", space);
        let ws1 = manager.create_workspace_with_number(1, "display-A", space);

        let window = WindowId::new(99, 1);
        let (result, _destroyed) = manager
            .assign_window_with_app_info(
                window,
                space,
                Some("com.example.foo"),
                None,
                None,
                None,
                None,
            )
            .expect("assign succeeds");
        let assignment = match result {
            AppRuleResult::Managed(a) => a,
            AppRuleResult::Unmanaged => panic!("expected managed result"),
        };
        assert_eq!(
            assignment.workspace_id, ws1,
            "app_rule workspace=1 must route to workspace whose number is 1, \
             not to whatever workspace sits at per-space position 1"
        );
    }

    /// Cross-space rendezvous: workspace number 1 lives on display B; a
    /// window matching `workspace = 1` opens on display A. VWM resolves the
    /// global slot correctly but cannot cross-space-assign — `assign_window_
    /// to_workspace` enforces space-locality. VWM therefore falls back to
    /// display A's default workspace. Driving the cross-display routing
    /// (either creating ws#1 on display A or physically moving the window
    /// to display B) is the engine layer's responsibility, not VWM's.
    #[test]
    fn app_rule_cross_space_falls_back_to_source_default() {
        let rule = AppWorkspaceRule {
            app_id: Some("com.example.foo".to_string()),
            workspace: Some(WorkspaceSelector::Index(1)),
            floating: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
        };
        let mut manager =
            VirtualWorkspaceManager::new_with_rules(vec![rule], LayoutSettings::default());

        let space_a = SpaceId::new(1);
        let space_b = SpaceId::new(2);
        manager.set_space_display(space_a, Some("display-A".to_string()));
        manager.set_space_display(space_b, Some("display-B".to_string()));

        // ws#1 lives on display B; display A lazy-inits to ws#0.
        let _ws1_b = manager.create_workspace_with_number(1, "display-B", space_b);
        let default_a = manager.list_workspaces(space_a)[0].0;

        let window = WindowId::new(99, 1);
        let (result, _destroyed) = manager
            .assign_window_with_app_info(
                window,
                space_a,
                Some("com.example.foo"),
                None,
                None,
                None,
                None,
            )
            .expect("assign succeeds");
        let assignment = match result {
            AppRuleResult::Managed(a) => a,
            AppRuleResult::Unmanaged => panic!("expected managed result"),
        };
        assert_eq!(
            assignment.workspace_id, default_a,
            "cross-space app_rule must fall back to source default; engine \
             owns the cross-display routing decision"
        );
    }

    /// When the app_rule references a workspace number that does not exist
    /// anywhere in the global pool, VWM falls back to the source space's
    /// default workspace. (Create-on-demand is the engine's responsibility —
    /// see `resolve_or_create_target_workspace_for_app_rule` — because it
    /// needs the screen size to wire up the layout tree.)
    #[test]
    fn app_rule_unknown_workspace_falls_back_to_source_default() {
        let rule = AppWorkspaceRule {
            app_id: Some("com.example.foo".to_string()),
            workspace: Some(WorkspaceSelector::Index(5)),
            floating: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
        };
        let mut manager =
            VirtualWorkspaceManager::new_with_rules(vec![rule], LayoutSettings::default());

        let space = SpaceId::new(1);
        manager.set_space_display(space, Some("display-A".to_string()));
        // Lazy init creates workspace 0 (the smallest-unused) on display A.
        let default_ws = manager.list_workspaces(space)[0].0;

        let window = WindowId::new(99, 1);
        let (result, _destroyed) = manager
            .assign_window_with_app_info(
                window,
                space,
                Some("com.example.foo"),
                None,
                None,
                None,
                None,
            )
            .expect("assign succeeds");
        let assignment = match result {
            AppRuleResult::Managed(a) => a,
            AppRuleResult::Unmanaged => panic!("expected managed result"),
        };
        assert_eq!(
            assignment.workspace_id, default_ws,
            "ws 5 doesn't exist — VWM must fall back to the source space's default"
        );
    }

    /// `peek_app_rule_workspace_number` returns the `WorkspaceNumber` an
    /// `Index(N)` rule targets, without mutating state. Used by the engine
    /// to drive create-on-demand BEFORE `assign_window_with_app_info`.
    #[test]
    fn peek_app_rule_workspace_number_returns_index_target() {
        let rule = AppWorkspaceRule {
            app_id: Some("com.example.foo".to_string()),
            workspace: Some(WorkspaceSelector::Index(3)),
            floating: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
        };
        let manager =
            VirtualWorkspaceManager::new_with_rules(vec![rule], LayoutSettings::default());

        assert_eq!(
            manager.peek_app_rule_workspace_number(Some("com.example.foo"), None, None, None, None),
            Some(3),
            "matching rule with Index(3) must peek as Some(3)"
        );
        assert_eq!(
            manager.peek_app_rule_workspace_number(
                Some("com.example.other"),
                None,
                None,
                None,
                None
            ),
            None,
            "no rule match → None"
        );
    }

    /// Names do not have a global pool, so the peek returns `None` for
    /// `Name(_)` rules — the engine has nothing to create. `assign_window_
    /// with_app_info` still handles the name lookup per-space internally.
    #[test]
    fn peek_app_rule_workspace_number_returns_none_for_name_selector() {
        let rule = AppWorkspaceRule {
            app_id: Some("com.example.foo".to_string()),
            workspace: Some(WorkspaceSelector::Name("editor".to_string())),
            floating: false,
            manage: true,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
        };
        let manager =
            VirtualWorkspaceManager::new_with_rules(vec![rule], LayoutSettings::default());

        assert_eq!(
            manager.peek_app_rule_workspace_number(Some("com.example.foo"), None, None, None, None),
            None,
            "Name(_) selectors do not target a global slot"
        );
    }

    /// Unmanaged rules (e.g. `floating = true, manage = false`) shouldn't
    /// trigger create-on-demand — they don't end up assigned anyway.
    #[test]
    fn peek_app_rule_workspace_number_returns_none_for_unmanaged_rule() {
        let rule = AppWorkspaceRule {
            app_id: Some("com.example.foo".to_string()),
            workspace: Some(WorkspaceSelector::Index(3)),
            floating: true,
            manage: false,
            app_name: None,
            title_regex: None,
            title_substring: None,
            ax_role: None,
            ax_subrole: None,
        };
        let manager =
            VirtualWorkspaceManager::new_with_rules(vec![rule], LayoutSettings::default());

        assert_eq!(
            manager.peek_app_rule_workspace_number(Some("com.example.foo"), None, None, None, None),
            None,
            "unmanaged rules must not provoke workspace creation"
        );
    }

    /// `set_space_display` round-trips through `space_display`.
    #[test]
    fn set_and_get_space_display() {
        let mut manager = VirtualWorkspaceManager::new();
        let space = SpaceId::new(1);
        assert_eq!(manager.space_display(space), None);
        manager.set_space_display(space, Some("display-A".to_string()));
        assert_eq!(manager.space_display(space), Some("display-A"));
        manager.set_space_display(space, None);
        assert_eq!(manager.space_display(space), None);
    }

    /// `remap_space` keeps the space → display UUID mirror consistent.
    #[test]
    fn remap_space_moves_display_mirror() {
        let mut manager = VirtualWorkspaceManager::new();
        let old_space = SpaceId::new(1);
        let new_space = SpaceId::new(2);

        manager.set_space_display(old_space, Some("display-A".to_string()));
        let _ = manager.list_workspaces(old_space);
        manager.remap_space(old_space, new_space);

        assert_eq!(manager.space_display(old_space), None);
        assert_eq!(manager.space_display(new_space), Some("display-A"));
    }

    /// When `set_active_workspace` runs before `set_space_display`, it
    /// synthesizes a placeholder UUID (`__space_<id>`) and stores the active
    /// entry under that key. A subsequent `set_space_display(space, Some(real))`
    /// must migrate that synthetic entry to the real UUID so
    /// `active_workspace(space)` continues to resolve, and so the synthetic
    /// key doesn't leak in `active_workspace_per_display`.
    #[test]
    fn set_space_display_promotes_synthetic_active_entries() {
        let mut manager = VirtualWorkspaceManager::new();
        let space = SpaceId::new(1);

        // Lazy-init the default workspace WITHOUT calling set_space_display
        // first — this forces `set_active_workspace` (called from
        // `ensure_space_initialized`) onto the synthetic-UUID code path.
        let workspaces = manager.list_workspaces(space);
        assert_eq!(workspaces.len(), 1);
        let ws_id = workspaces[0].0;

        // The synthetic UUID is now the key under which the active workspace
        // is stored.
        let synthetic_uuid = synthetic_display_uuid(space);
        assert!(
            manager.active_workspace_per_display.contains_key(&synthetic_uuid),
            "expected synthetic entry `{}` in active_workspace_per_display, got keys: {:?}",
            synthetic_uuid,
            manager.active_workspace_per_display.keys().collect::<Vec<_>>(),
        );
        assert_eq!(manager.active_workspace(space), Some(ws_id));

        // Now wire the real display UUID. The synthetic entry must migrate.
        let real_uuid = "display-REAL";
        manager.set_space_display(space, Some(real_uuid.to_string()));

        assert!(
            !manager.active_workspace_per_display.contains_key(&synthetic_uuid),
            "synthetic entry `{}` should be gone, got keys: {:?}",
            synthetic_uuid,
            manager.active_workspace_per_display.keys().collect::<Vec<_>>(),
        );
        assert!(
            manager.active_workspace_per_display.contains_key(real_uuid),
            "expected entry under `{}` in active_workspace_per_display, got keys: {:?}",
            real_uuid,
            manager.active_workspace_per_display.keys().collect::<Vec<_>>(),
        );
        // The active workspace must still be resolvable through the public API.
        assert_eq!(manager.active_workspace(space), Some(ws_id));
    }

    /// The final empty workspace on a display must stay alive as that
    /// display's placeholder. Once another same-display workspace exists,
    /// an empty active workspace may be destroyed after active focus moves.
    #[test]
    fn display_keeps_one_empty_workspace_as_placeholder() {
        let mut manager = VirtualWorkspaceManager::new();
        let space1 = SpaceId::new(1);
        let space2 = SpaceId::new(2);

        manager.set_space_display(space1, Some("display-A".to_string()));
        manager.set_space_display(space2, Some("display-B".to_string()));

        // Create a workspace on space1 (display-A) and activate it.
        let ws_a1 = manager.create_workspace_with_number(10, "display-A", space1);
        assert!(manager.set_active_workspace(space1, ws_a1));
        assert_eq!(manager.active_workspace(space1), Some(ws_a1));

        // Also wire up display-B with its own active workspace so the manager
        // has genuine two-display state.
        let ws_b1 = manager.create_workspace_with_number(20, "display-B", space2);
        assert!(manager.set_active_workspace(space2, ws_b1));

        // Try to destroy ws_a1 while it is display-A's only workspace —
        // the per-display placeholder guard must keep it alive.
        let destroyed = manager.destroy_ephemeral_workspaces(vec![ws_a1]);
        assert!(
            destroyed.is_empty(),
            "display placeholder should NOT be destroyed, but got: {:?}",
            destroyed
        );
        assert!(
            manager.workspaces.get(ws_a1).is_some(),
            "ws_a1 should still exist in the SlotMap"
        );

        // Move active focus on display-A to a different workspace.
        let ws_a2 = manager.create_workspace_with_number(11, "display-A", space1);
        assert!(manager.set_active_workspace(space1, ws_a2));
        assert_eq!(manager.active_workspace(space1), Some(ws_a2));

        // Now ws_a1 is no longer active on any display — ephemeral destroy fires.
        let destroyed = manager.destroy_ephemeral_workspaces(vec![ws_a1]);
        assert_eq!(
            destroyed,
            vec![(space1, ws_a1)],
            "now-inactive empty workspace should be destroyed"
        );
        assert!(
            manager.workspaces.get(ws_a1).is_none(),
            "ws_a1 should be gone from the SlotMap"
        );
    }

    #[test]
    fn active_empty_workspace_destroyed_when_display_has_fallback() {
        let mut manager = VirtualWorkspaceManager::new();
        let space = SpaceId::new(1);
        manager.set_space_display(space, Some("display-A".to_string()));

        let ws_default = manager.create_workspace_with_number(1, "display-A", space);
        let ws_empty = manager.create_workspace_with_number(7, "display-A", space);
        assert!(manager.set_active_workspace(space, ws_empty));
        assert_eq!(manager.active_workspace(space), Some(ws_empty));

        let destroyed = manager.destroy_ephemeral_workspaces(vec![ws_empty]);
        assert_eq!(destroyed, vec![(space, ws_empty)]);
        assert!(manager.workspaces.get(ws_empty).is_none());
        assert_eq!(
            manager.active_workspace(space),
            Some(ws_default),
            "active display should fall back before the empty workspace is destroyed"
        );
    }
}
