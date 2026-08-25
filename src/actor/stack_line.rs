use std::collections::hash_map::Entry;
use std::sync::Arc;

use arc_swap::ArcSwap;
use objc2::MainThreadMarker;
use objc2_app_kit::NSCursor;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use tracing::instrument;

use crate::actor::app::WindowId;
use crate::actor::reactor::{Command, ReactorCommand};
use crate::actor::{self, reactor};
use crate::common::collections::HashMap;
use crate::common::config::{Config, HorizontalPlacement, VerticalPlacement};
use crate::core::bsp::Axis;
use crate::core::ids::GroupId;
use crate::core::snapshot::CoreSnapshot;
use crate::model::layout::LayoutKind;
use crate::sys::screen::{CoordinateConverter, SpaceId};
use crate::ui::stack_line::{
    GroupDisplayData, GroupIndicatorWindow, GroupKind, IndicatorConfig, point_hits_indicator_frame,
};

/// Shared indicator hit-rect state readable from the event tap callback.
/// Uses Arc<ArcSwap<...>> for lock-free reads from the input thread.
pub type SharedHitRects = Arc<ArcSwap<Vec<CGRect>>>;

pub fn new_shared_hit_rects() -> SharedHitRects { Arc::new(ArcSwap::from_pointee(Vec::new())) }

#[derive(Debug, Clone)]
pub struct GroupInfo {
    pub group_id: GroupId,
    pub space_id: SpaceId,
    pub container_kind: LayoutKind,
    pub frame: CGRect,
    pub total_count: usize,
    pub selected_index: usize,
    pub window_ids: Vec<WindowId>,
}

fn project_display_groups(
    snapshot: &CoreSnapshot,
    display: &crate::core::snapshot::DisplaySnapshot,
) -> Option<(SpaceId, Vec<GroupInfo>, bool)> {
    let space = SpaceId::new(display.space?.0);
    let workspace_id = display.active_workspace?;
    let workspace = snapshot.workspaces.iter().find(|workspace| workspace.id == workspace_id)?;
    let workspace_has_fullscreen = workspace
        .groups
        .iter()
        .flat_map(|group| group.windows.iter())
        .chain(workspace.floating_windows.iter())
        .any(|window| {
            snapshot
                .windows
                .iter()
                .find(|candidate| candidate.id == *window)
                .is_some_and(|candidate| candidate.fullscreen)
        });
    let groups = workspace
        .groups
        .iter()
        .filter(|group| group.windows.len() > 1)
        .filter_map(|group| {
            let selected = group.windows.get(group.selected).copied()?;
            let frame = workspace.layout_frames.get(&selected).copied().or_else(|| {
                snapshot
                    .windows
                    .iter()
                    .find(|window| window.id == selected)
                    .map(|window| window.frame)
            })?;
            Some(GroupInfo {
                group_id: group.id,
                space_id: space,
                container_kind: match group.axis {
                    Axis::Horizontal => LayoutKind::HorizontalStack,
                    Axis::Vertical => LayoutKind::VerticalStack,
                },
                frame: CGRect::new(
                    CGPoint::new(frame.origin.x, frame.origin.y),
                    CGSize::new(frame.size.width, frame.size.height),
                ),
                total_count: group.windows.len(),
                selected_index: group.selected,
                window_ids: group
                    .windows
                    .iter()
                    .map(|window| WindowId {
                        pid: window.application.0,
                        idx: window.index,
                    })
                    .collect(),
            })
        })
        .collect();
    Some((space, groups, workspace_has_fullscreen))
}

#[derive(Debug)]
pub enum Event {
    GroupsUpdated {
        active_space_ids: Vec<SpaceId>,
        space_id: SpaceId,
        groups: Vec<GroupInfo>,
        active_workspace_for_space_has_fullscreen: bool,
    },
    ScreenParametersChanged(CoordinateConverter),
    ConfigUpdated(Config),
    SnapshotUpdated(Arc<CoreSnapshot>),
    /// A click that the event tap already confirmed lands on a visible,
    /// non-occluded stack-line indicator.
    MouseDown(CGPoint),
    /// Cursor moved; `hits_indicator` is `true` when the event tap's
    /// hit-test (geometry + occlusion) determined the point is over an
    /// indicator.
    MouseMoved {
        point: CGPoint,
        hits_indicator: bool,
    },
}

pub struct StackLine {
    config: Config,
    rx: Receiver,
    #[allow(dead_code)]
    mtm: MainThreadMarker,
    indicators: HashMap<GroupId, GroupIndicatorWindow>,
    #[allow(dead_code)]
    reactor_tx: reactor::Sender,
    coordinate_converter: CoordinateConverter,
    group_sigs_by_space: HashMap<SpaceId, Vec<GroupSig>>,
    cursor_over_indicator: bool,
    shared_hit_rects: SharedHitRects,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

impl StackLine {
    pub fn new(
        config: Config,
        rx: Receiver,
        mtm: MainThreadMarker,
        reactor_tx: reactor::Sender,
        coordinate_converter: CoordinateConverter,
        shared_hit_rects: SharedHitRects,
    ) -> Self {
        Self {
            config,
            rx,
            mtm,
            indicators: HashMap::default(),
            reactor_tx,
            coordinate_converter,
            group_sigs_by_space: HashMap::default(),
            cursor_over_indicator: false,
            shared_hit_rects,
        }
    }

    pub async fn run(mut self) {
        if !self.is_enabled() {
            tracing::debug!("stack line disabled at start; will listen for config changes");
        }

        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            self.handle_event(event);
        }
    }

    fn is_enabled(&self) -> bool { self.config.settings.ui.stack_line.enabled }

    /// Publish the current indicator frames so the event tap can suppress
    /// clicks that land on a visible, non-occluded indicator.
    fn sync_shared_hit_rects(&self) {
        let mut rects = Vec::new();
        if self.is_enabled() {
            for indicator in self.indicators.values().filter(|i| i.is_visible()) {
                rects.push(indicator.frame());
            }
        }
        self.shared_hit_rects.store(Arc::new(rects));
    }

    #[instrument(name = "stack_line::handle_event", skip(self))]
    fn handle_event(&mut self, event: Event) {
        if !self.is_enabled()
            && !matches!(
                event,
                Event::ConfigUpdated(_)
                    | Event::ScreenParametersChanged(_)
                    | Event::SnapshotUpdated(_)
                    | Event::MouseDown(_)
                    | Event::MouseMoved { .. }
            )
        {
            return;
        }
        match event {
            Event::GroupsUpdated {
                active_space_ids,
                space_id,
                groups,
                active_workspace_for_space_has_fullscreen,
            } => {
                self.handle_groups_updated(
                    active_space_ids,
                    space_id,
                    groups,
                    active_workspace_for_space_has_fullscreen,
                );
                self.sync_shared_hit_rects();
            }
            Event::ScreenParametersChanged(converter) => {
                self.handle_screen_parameters_changed(converter);
            }
            Event::ConfigUpdated(config) => {
                self.handle_config_updated(config);
                self.sync_shared_hit_rects();
            }
            Event::SnapshotUpdated(snapshot) => {
                self.handle_snapshot_updated(&snapshot);
                self.sync_shared_hit_rects();
            }
            Event::MouseDown(point) => {
                self.handle_mouse_down(point);
            }
            Event::MouseMoved { point, hits_indicator } => {
                self.handle_mouse_moved(point, hits_indicator);
            }
        }
    }

    fn handle_snapshot_updated(&mut self, snapshot: &CoreSnapshot) {
        let active_space_ids = snapshot
            .displays
            .iter()
            .filter_map(|display| display.space.map(|space| SpaceId::new(space.0)))
            .collect::<Vec<_>>();
        if active_space_ids.is_empty() {
            for indicator in self.indicators.values() {
                let _ = indicator.clear();
            }
            self.indicators.clear();
            self.group_sigs_by_space.clear();
            return;
        }

        for display in &snapshot.displays {
            let Some((space, groups, workspace_has_fullscreen)) =
                project_display_groups(snapshot, display)
            else {
                continue;
            };
            self.handle_groups_updated(
                active_space_ids.clone(),
                space,
                groups,
                workspace_has_fullscreen,
            );
        }
    }

    fn handle_groups_updated(
        &mut self,
        active_space_ids: Vec<SpaceId>,
        space_id: SpaceId,
        groups: Vec<GroupInfo>,
        space_has_fullscreen: bool,
    ) {
        let active: crate::common::collections::HashSet<SpaceId> =
            active_space_ids.iter().copied().collect();

        self.indicators.retain(|_group_id, indicator| match indicator.space_id() {
            Some(indicator_space_id) if !active.contains(&indicator_space_id) => {
                if let Err(err) = indicator.clear() {
                    tracing::warn!(?err, "failed to clear stack line indicator for inactive space");
                }
                false
            }
            _ => true,
        });
        self.group_sigs_by_space.retain(|sid, _| active.contains(sid));

        let sigs: Vec<GroupSig> = groups.iter().map(GroupSig::from_group_info).collect();

        let groups_unchanged = match self.group_sigs_by_space.entry(space_id) {
            Entry::Occupied(ref prev) => prev.get() == &sigs,
            Entry::Vacant(_) => false,
        };

        if !groups_unchanged {
            let _ = self.group_sigs_by_space.insert(space_id, sigs);

            let group_ids: std::collections::HashSet<GroupId> =
                groups.iter().map(|g| g.group_id).collect();
            self.indicators.retain(|&group_id, indicator| match indicator.space_id() {
                Some(indicator_space_id) if indicator_space_id == space_id => {
                    if group_ids.contains(&group_id) {
                        true
                    } else {
                        if let Err(err) = indicator.clear() {
                            tracing::warn!(?err, "failed to clear stack line indicator");
                        }
                        false
                    }
                }
                _ => true,
            });

            for group in groups {
                self.update_or_create_indicator(group);
            }
        } else {
            let _ = self.group_sigs_by_space.insert(space_id, sigs);
        }

        for indicator in self.indicators.values() {
            if indicator.space_id() == Some(space_id) {
                if let Err(err) = indicator.set_visibility(space_has_fullscreen) {
                    tracing::warn!(?err, "failed to set stack line indicator visibility");
                }
            }
        }
    }

    fn handle_screen_parameters_changed(&mut self, converter: CoordinateConverter) {
        self.coordinate_converter = converter;
        tracing::debug!("Updated coordinate converter for group indicators");
    }

    fn handle_config_updated(&mut self, config: Config) {
        let old_enabled = self.is_enabled();
        self.config = config;
        let new_enabled = self.is_enabled();

        if old_enabled && !new_enabled {
            for indicator in self.indicators.values() {
                if let Err(err) = indicator.clear() {
                    tracing::warn!(
                        ?err,
                        "failed to clear stack line indicator during config update"
                    );
                }
            }
            self.indicators.clear();
            self.group_sigs_by_space.clear();
        } else if new_enabled {
            let new_config = self.indicator_config();
            for (group_id, indicator) in &self.indicators {
                if let Some(group_data) = indicator.group_data() {
                    if let Err(err) = indicator.update(new_config, group_data) {
                        tracing::warn!(
                            ?err,
                            ?group_id,
                            "failed to update stack line indicator with new config"
                        );
                    }
                }
            }
        }

        tracing::debug!("Updated stack line configuration");
    }

    fn handle_mouse_down(&mut self, screen_point: CGPoint) {
        if !self.is_enabled() {
            return;
        }

        // The event tap already verified that this click lands on a visible,
        // non-occluded indicator. We only need to find the matching segment.
        for (&group_id, indicator) in &self.indicators {
            if !indicator.is_visible() {
                continue;
            }

            let frame = indicator.frame();
            if !point_hits_indicator_frame(screen_point, frame) {
                continue;
            }

            let local_point =
                CGPoint::new(screen_point.x - frame.origin.x, screen_point.y - frame.origin.y);
            if let Some(segment_index) = indicator.check_click(local_point) {
                tracing::debug!(
                    ?group_id,
                    segment_index,
                    "Detected click on stack line indicator segment"
                );
                self.handle_indicator_clicked(group_id, segment_index);
                return;
            }
        }
    }

    // The indicator is not backed by NSWindow, so cursor state is tracked explicitly.
    fn handle_mouse_moved(&mut self, _screen_point: CGPoint, hits_indicator: bool) {
        let over_indicator = self.is_enabled() && hits_indicator;

        if over_indicator != self.cursor_over_indicator {
            self.cursor_over_indicator = over_indicator;
            if over_indicator {
                NSCursor::pointingHandCursor().set();
                tracing::trace!("Set pointing hand cursor over indicator");
            } else {
                NSCursor::arrowCursor().set();
                tracing::trace!("Reset to arrow cursor");
            }
        }
    }

    fn handle_indicator_clicked(&mut self, group_id: GroupId, segment_index: usize) {
        if let Some(indicator) = self.indicators.get(&group_id) {
            let window_ids = indicator.window_ids();
            if let Some(window_id) = window_ids.get(segment_index) {
                tracing::debug!(
                    ?group_id,
                    segment_index,
                    ?window_id,
                    "Group indicator clicked - focusing window"
                );
                let _ = self.reactor_tx.send(reactor::Event::Command(Command::Reactor(
                    ReactorCommand::FocusWindow {
                        window_id: *window_id,
                        window_server_id: None,
                    },
                )));
            } else {
                tracing::debug!(
                    ?group_id,
                    segment_index,
                    "Group indicator clicked with invalid segment index"
                );
            }
        } else {
            tracing::debug!(
                ?group_id,
                segment_index,
                "Group indicator clicked but not found in map"
            );
        }
    }

    fn update_or_create_indicator(&mut self, group: GroupInfo) {
        let group_kind = match group.container_kind {
            LayoutKind::HorizontalStack => GroupKind::Horizontal,
            LayoutKind::VerticalStack => GroupKind::Vertical,
            _ => {
                tracing::warn!(?group.container_kind, "Unexpected container kind for group");
                return;
            }
        };

        let config = self.indicator_config();
        let group_data = GroupDisplayData {
            group_kind,
            total_count: group.total_count,
            selected_index: group.selected_index,
            window_ids: group.window_ids,
        };

        let indicator_frame = Self::calculate_indicator_frame(
            group.frame,
            group_kind,
            config.bar_thickness,
            config.horizontal_placement,
            config.vertical_placement,
            config.spacing,
        );

        let group_id = group.group_id;

        if let Some(indicator) = self.indicators.get_mut(&group_id) {
            if let Err(err) = indicator.set_frame(indicator_frame) {
                tracing::warn!(?err, "failed to set stack line indicator frame");
            }
            indicator.set_space_id(group.space_id);
            if let Err(err) = indicator.update(config, group_data.clone()) {
                tracing::warn!(?err, "failed to update stack line indicator");
            }
        } else {
            match GroupIndicatorWindow::new(indicator_frame, config) {
                Ok(indicator) => {
                    indicator.set_space_id(group.space_id);
                    if let Err(err) = indicator.update(config, group_data.clone()) {
                        tracing::warn!(?err, "failed to initialize stack line indicator");
                    }
                    self.indicators.insert(group_id, indicator);
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to create stack line indicator window");
                    return;
                }
            }
        }

        tracing::debug!(
            ?group.frame,
            ?indicator_frame,
            "Positioned indicator"
        );
    }

    fn calculate_indicator_frame(
        group_frame: CGRect,
        group_kind: GroupKind,
        thickness: f64,
        _horizontal_placement: HorizontalPlacement,
        _vertical_placement: VerticalPlacement,
        spacing: f64,
    ) -> CGRect {
        let min_size = thickness * 2.0;
        let adjusted_width = group_frame.size.width.max(min_size);
        let adjusted_height = group_frame.size.height.max(min_size);

        match group_kind {
            GroupKind::Horizontal => CGRect::new(
                CGPoint::new(group_frame.origin.x, group_frame.origin.y - spacing),
                CGSize::new(adjusted_width, thickness),
            ),
            GroupKind::Vertical => CGRect::new(
                CGPoint::new(group_frame.origin.x - spacing, group_frame.origin.y),
                CGSize::new(thickness, adjusted_height),
            ),
        }
    }

    fn indicator_config(&self) -> IndicatorConfig {
        IndicatorConfig::from(&self.config.settings.ui.stack_line)
    }
}

#[derive(Debug, Clone, PartialEq)]
struct GroupSig {
    group_id: GroupId,
    kind: LayoutKind,
    x_q2: i64,
    y_q2: i64,
    w_q2: i64,
    h_q2: i64,
    total: usize,
    selected_index: usize,
    window_ids: Vec<WindowId>,
}

impl GroupSig {
    fn from_group_info(g: &GroupInfo) -> GroupSig {
        let quant = |v: f64| -> i64 { (v * 2.0).round() as i64 };
        GroupSig {
            group_id: g.group_id,
            kind: g.container_kind,
            x_q2: quant(g.frame.origin.x),
            y_q2: quant(g.frame.origin.y),
            w_q2: quant(g.frame.size.width),
            h_q2: quant(g.frame.size.height),
            total: g.total_count,
            selected_index: g.selected_index,
            window_ids: g.window_ids.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::num::NonZeroU32;

    use super::*;
    use crate::core::geometry::Rect;
    use crate::core::ids::{
        ApplicationId, DisplayId, SpaceId as CoreSpaceId, WorkspaceId, WorkspaceNumber,
    };
    use crate::core::snapshot::{
        DisplaySnapshot, GroupSnapshot, WindowSnapshot, WorkspaceSnapshot,
    };

    #[test]
    fn test_group_info_fields() {
        assert_eq!(LayoutKind::VerticalStack.is_group(), true);
        assert_eq!(LayoutKind::HorizontalStack.is_group(), true);
        assert_eq!(LayoutKind::Horizontal.is_group(), false);
    }

    #[test]
    fn snapshot_groups_project_stable_ids_orientation_and_preview_frames() {
        let first = crate::core::ids::WindowId::new(ApplicationId(12), NonZeroU32::new(1).unwrap());
        let second =
            crate::core::ids::WindowId::new(ApplicationId(12), NonZeroU32::new(2).unwrap());
        let group_id = GroupId(55);
        let workspace_id = WorkspaceId(7);
        let display_id = DisplayId("main".into());
        let frame = Rect::new(10.0, 20.0, 600.0, 400.0).unwrap();
        let snapshot = CoreSnapshot {
            displays: vec![DisplaySnapshot {
                id: display_id.clone(),
                frame,
                space: Some(CoreSpaceId(9)),
                is_active_context: true,
                active_workspace: Some(workspace_id),
                last_workspace: None,
            }],
            workspaces: vec![WorkspaceSnapshot {
                id: workspace_id,
                number: Some(WorkspaceNumber::try_from(1).unwrap()),
                name: "Main".into(),
                display: display_id,
                groups: vec![GroupSnapshot {
                    id: group_id,
                    axis: Axis::Vertical,
                    windows: vec![first, second],
                    selected: 1,
                }],
                floating_windows: Vec::new(),
                last_tiled_window: None,
                last_floating_window: None,
                layout_frames: BTreeMap::from([(second, frame)]),
            }],
            windows: vec![WindowSnapshot {
                id: second,
                workspace: Some(workspace_id),
                frame,
                title: "Second".into(),
                application_name: None,
                platform_id: None,
                floating: false,
                minimized: false,
                fullscreen: true,
            }],
            ..CoreSnapshot::default()
        };

        let (space, groups, fullscreen) =
            project_display_groups(&snapshot, &snapshot.displays[0]).unwrap();

        assert_eq!(space, SpaceId::new(9));
        assert!(fullscreen);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, group_id);
        assert_eq!(groups[0].container_kind, LayoutKind::VerticalStack);
        assert_eq!(groups[0].selected_index, 1);
        assert_eq!(groups[0].frame.size.width, 600.0);
    }

    #[test]
    fn test_calculate_indicator_frame() {
        let group_frame = CGRect::new(CGPoint::new(100.0, 200.0), CGSize::new(400.0, 300.0));
        let thickness = 6.0;
        let spacing = 4.0;

        let frame_horizontal = StackLine::calculate_indicator_frame(
            group_frame,
            GroupKind::Horizontal,
            thickness,
            HorizontalPlacement::Top,
            VerticalPlacement::Right,
            spacing,
        );
        assert_eq!(frame_horizontal.origin.x, 100.0);
        assert_eq!(frame_horizontal.origin.y, 200.0 - spacing);
        assert_eq!(frame_horizontal.size.width, 400.0);
        assert_eq!(frame_horizontal.size.height, thickness);

        let frame_vertical = StackLine::calculate_indicator_frame(
            group_frame,
            GroupKind::Vertical,
            thickness,
            HorizontalPlacement::Top,
            VerticalPlacement::Left,
            spacing,
        );
        assert_eq!(frame_vertical.origin.x, 100.0 - spacing);
        assert_eq!(frame_vertical.origin.y, 200.0);
        assert_eq!(frame_vertical.size.width, thickness);
        assert_eq!(frame_vertical.size.height, 300.0);
    }
}
