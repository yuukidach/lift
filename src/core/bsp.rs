use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::config::Gaps;
use super::constraints::{AxisConstraints, WindowConstraints, solve_axis_lengths};
use super::geometry::{Point, Rect, Size};
use super::ids::{GroupId, WindowId};
use super::snapshot::GroupSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ratio(f64);

impl Ratio {
    pub const BALANCED: Self = Self(0.5);

    pub fn new(value: f64) -> Result<Self, BspError> {
        if value.is_finite() && (0.05..=0.95).contains(&value) {
            Ok(Self(value))
        } else {
            Err(BspError::InvalidRatio(value))
        }
    }

    pub const fn get(self) -> f64 { self.0 }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct NodeId(u64);

#[derive(Clone, Debug, PartialEq)]
enum BspNode {
    Split {
        axis: Axis,
        ratio: Ratio,
        first: NodeId,
        second: NodeId,
    },
    Group {
        id: GroupId,
        axis: Axis,
        windows: Vec<WindowId>,
        selected: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BspTree {
    root: Option<NodeId>,
    nodes: BTreeMap<NodeId, BspNode>,
    window_nodes: BTreeMap<WindowId, NodeId>,
    next_node_id: u64,
    next_group_id: u64,
    fullscreen: Option<(WindowId, bool)>,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum BspError {
    #[error("window is already tiled: {0:?}")]
    DuplicateWindow(WindowId),
    #[error("window is not tiled: {0:?}")]
    MissingWindow(WindowId),
    #[error("target window is not tiled: {0:?}")]
    MissingTarget(WindowId),
    #[error("BSP ratio must be finite and in 0.05..=0.95, got {0}")]
    InvalidRatio(f64),
    #[error("BSP invariant violated: {0}")]
    InvariantViolation(String),
}

// The reducer owns this tree; platform adapters only observe its snapshots.
#[allow(dead_code)]
impl BspTree {
    pub fn contains(&self, window: WindowId) -> bool { self.window_nodes.contains_key(&window) }

    pub fn is_empty(&self) -> bool { self.root.is_none() }

    pub fn insert_after(
        &mut self,
        after: Option<WindowId>,
        window: WindowId,
    ) -> Result<(), BspError> {
        if self.contains(window) {
            return Err(BspError::DuplicateWindow(window));
        }

        let Some(root) = self.root else {
            let group = self.make_group(vec![window], 0);
            self.root = Some(group);
            return Ok(());
        };

        let target = match after {
            Some(target) => {
                self.window_nodes.get(&target).copied().ok_or(BspError::MissingTarget(target))?
            }
            None => self.last_group(root)?,
        };
        let parent = self.parent_of(target);
        let axis = if self.depth(target).is_multiple_of(2) {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let new_group = self.make_group(vec![window], 0);
        let split = self.allocate_node(BspNode::Split {
            axis,
            ratio: Ratio::BALANCED,
            first: target,
            second: new_group,
        });
        self.replace_child(parent, target, split)?;
        Ok(())
    }

    pub fn remove(&mut self, window: WindowId) -> Result<(), BspError> {
        if self.fullscreen.is_some_and(|(fullscreen, _)| fullscreen == window) {
            self.fullscreen = None;
        }
        let group =
            self.window_nodes.get(&window).copied().ok_or(BspError::MissingWindow(window))?;
        self.remove_from_group(group, window, true)
    }

    pub fn join(&mut self, window: WindowId, target: WindowId) -> Result<bool, BspError> {
        let source_group =
            self.window_nodes.get(&window).copied().ok_or(BspError::MissingWindow(window))?;
        let target_group =
            self.window_nodes.get(&target).copied().ok_or(BspError::MissingTarget(target))?;
        if source_group == target_group {
            return Ok(false);
        }

        self.remove_from_group(source_group, window, true)?;
        let BspNode::Group { windows, selected, .. } = self
            .nodes
            .get_mut(&target_group)
            .ok_or_else(|| BspError::InvariantViolation("target group disappeared".into()))?
        else {
            return Err(BspError::InvariantViolation(
                "window index points to a split".into(),
            ));
        };
        windows.push(window);
        *selected = windows.len() - 1;
        self.window_nodes.insert(window, target_group);
        Ok(true)
    }

    pub fn unjoin(&mut self, window: WindowId) -> Result<bool, BspError> {
        let group =
            self.window_nodes.get(&window).copied().ok_or(BspError::MissingWindow(window))?;
        let member_count = match self.nodes.get(&group) {
            Some(BspNode::Group { windows, .. }) => windows.len(),
            _ => {
                return Err(BspError::InvariantViolation(
                    "window index points to a split".into(),
                ));
            }
        };
        if member_count == 1 {
            return Ok(false);
        }

        self.remove_from_group(group, window, false)?;
        let parent = self.parent_of(group);
        let axis = if self.depth(group).is_multiple_of(2) {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let new_group = self.make_group(vec![window], 0);
        let split = self.allocate_node(BspNode::Split {
            axis,
            ratio: Ratio::BALANCED,
            first: group,
            second: new_group,
        });
        self.replace_child(parent, group, split)?;
        Ok(true)
    }

    pub fn groups(&self) -> Result<Vec<GroupSnapshot>, BspError> {
        let Some(root) = self.root else {
            return Ok(Vec::new());
        };
        let mut groups = Vec::new();
        self.collect_groups(root, &mut groups)?;
        Ok(groups)
    }

    pub fn windows(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.window_nodes.keys().copied()
    }

    pub fn selected_windows(&self) -> Result<Vec<WindowId>, BspError> {
        Ok(self
            .groups()?
            .into_iter()
            .filter_map(|group| group.windows.get(group.selected).copied())
            .collect())
    }

    pub fn select(&mut self, window: WindowId) -> Result<(), BspError> {
        let group =
            self.window_nodes.get(&window).copied().ok_or(BspError::MissingWindow(window))?;
        let Some(BspNode::Group { windows, selected, .. }) = self.nodes.get_mut(&group) else {
            return Err(BspError::InvariantViolation(
                "window index points to a split".into(),
            ));
        };
        *selected = windows
            .iter()
            .position(|candidate| *candidate == window)
            .ok_or_else(|| BspError::InvariantViolation("indexed window is absent".into()))?;
        Ok(())
    }

    pub fn swap(&mut self, first: WindowId, second: WindowId) -> Result<bool, BspError> {
        let first_group =
            self.window_nodes.get(&first).copied().ok_or(BspError::MissingWindow(first))?;
        let second_group =
            self.window_nodes.get(&second).copied().ok_or(BspError::MissingWindow(second))?;
        if first == second {
            return Ok(false);
        }
        let first_position = self.position_in_group(first_group, first)?;
        let second_position = self.position_in_group(second_group, second)?;
        if first_group == second_group {
            let Some(BspNode::Group { windows, .. }) = self.nodes.get_mut(&first_group) else {
                return Err(BspError::InvariantViolation(
                    "window index points to a split".into(),
                ));
            };
            windows.swap(first_position, second_position);
        } else {
            let Some(BspNode::Group { windows, .. }) = self.nodes.get_mut(&first_group) else {
                return Err(BspError::InvariantViolation(
                    "window index points to a split".into(),
                ));
            };
            windows[first_position] = second;
            let Some(BspNode::Group { windows, .. }) = self.nodes.get_mut(&second_group) else {
                return Err(BspError::InvariantViolation(
                    "window index points to a split".into(),
                ));
            };
            windows[second_position] = first;
            self.window_nodes.insert(first, second_group);
            self.window_nodes.insert(second, first_group);
        }
        Ok(true)
    }

    pub fn resize(&mut self, window: WindowId, amount: f64) -> Result<bool, BspError> {
        if !amount.is_finite() {
            return Err(BspError::InvalidRatio(amount));
        }
        let node =
            self.window_nodes.get(&window).copied().ok_or(BspError::MissingWindow(window))?;
        let Some(parent) = self.parent_of(node) else {
            return Ok(false);
        };
        let Some(BspNode::Split { ratio, first, .. }) = self.nodes.get_mut(&parent) else {
            return Err(BspError::InvariantViolation(
                "window parent is not a split".into(),
            ));
        };
        let delta = if *first == node { amount } else { -amount };
        *ratio = Ratio::new((ratio.get() + delta).clamp(0.05, 0.95))?;
        Ok(true)
    }

    pub fn toggle_orientation(&mut self, window: WindowId) -> Result<bool, BspError> {
        let node =
            self.window_nodes.get(&window).copied().ok_or(BspError::MissingWindow(window))?;
        let group_has_stack = matches!(
            self.nodes.get(&node),
            Some(BspNode::Group { windows, .. }) if windows.len() > 1
        );
        let target = if group_has_stack {
            node
        } else if let Some(parent) = self.parent_of(node) {
            parent
        } else {
            return Ok(false);
        };
        let axis = match self.nodes.get_mut(&target) {
            Some(BspNode::Group { axis, .. }) | Some(BspNode::Split { axis, .. }) => axis,
            None => {
                return Err(BspError::InvariantViolation(
                    "orientation target is missing".into(),
                ));
            }
        };
        *axis = match *axis {
            Axis::Horizontal => Axis::Vertical,
            Axis::Vertical => Axis::Horizontal,
        };
        Ok(true)
    }

    pub fn toggle_fullscreen(
        &mut self,
        window: WindowId,
        within_gaps: bool,
    ) -> Result<bool, BspError> {
        if !self.contains(window) {
            return Err(BspError::MissingWindow(window));
        }
        self.fullscreen = if self
            .fullscreen
            .is_some_and(|(current, mode)| current == window && mode == within_gaps)
        {
            None
        } else {
            Some((window, within_gaps))
        };
        Ok(self.fullscreen.is_some())
    }

    pub fn layout(
        &self,
        frame: Rect,
        gaps: Gaps,
        constraints: &BTreeMap<WindowId, WindowConstraints>,
    ) -> Result<BTreeMap<WindowId, Rect>, BspError> {
        let Some(root) = self.root else {
            return Ok(BTreeMap::new());
        };
        if let Some((window, within_gaps)) = self.fullscreen {
            let group =
                self.window_nodes.get(&window).copied().ok_or(BspError::MissingWindow(window))?;
            let frame = if within_gaps {
                inset_frame(frame, gaps)
            } else {
                frame
            };
            let mut frames = BTreeMap::new();
            let Some(BspNode::Group { windows, .. }) = self.nodes.get(&group) else {
                return Err(BspError::InvariantViolation(
                    "fullscreen window index points to a split".into(),
                ));
            };
            for window in windows {
                frames.insert(*window, frame);
            }
            return Ok(frames);
        }
        let area = inset_frame(frame, gaps);
        let mut frames = BTreeMap::new();
        self.layout_node(root, area, gaps, constraints, &mut frames)?;
        Ok(frames)
    }

    pub fn validate(&self) -> Result<(), BspError> {
        let Some(root) = self.root else {
            if self.nodes.is_empty() && self.window_nodes.is_empty() {
                return Ok(());
            }
            return Err(BspError::InvariantViolation(
                "empty tree retains nodes or window indexes".into(),
            ));
        };

        let mut visited_nodes = BTreeSet::new();
        let mut indexed_windows = BTreeMap::new();
        self.validate_node(root, &mut visited_nodes, &mut indexed_windows)?;
        if visited_nodes.len() != self.nodes.len() {
            return Err(BspError::InvariantViolation(
                "tree contains unreachable nodes".into(),
            ));
        }
        if indexed_windows != self.window_nodes {
            return Err(BspError::InvariantViolation(
                "window index disagrees with group membership".into(),
            ));
        }
        if let Some((fullscreen, _)) = self.fullscreen
            && !self.window_nodes.contains_key(&fullscreen)
        {
            return Err(BspError::InvariantViolation(
                "fullscreen state references a missing window".into(),
            ));
        }
        Ok(())
    }

    fn allocate_node(&mut self, node: BspNode) -> NodeId {
        self.next_node_id += 1;
        let id = NodeId(self.next_node_id);
        self.nodes.insert(id, node);
        id
    }

    fn position_in_group(&self, group: NodeId, window: WindowId) -> Result<usize, BspError> {
        let Some(BspNode::Group { windows, .. }) = self.nodes.get(&group) else {
            return Err(BspError::InvariantViolation(
                "window index points to a split".into(),
            ));
        };
        windows
            .iter()
            .position(|candidate| *candidate == window)
            .ok_or_else(|| BspError::InvariantViolation("indexed window is absent".into()))
    }

    fn make_group(&mut self, windows: Vec<WindowId>, selected: usize) -> NodeId {
        self.next_group_id += 1;
        self.make_group_with_id(GroupId(self.next_group_id), Axis::Horizontal, windows, selected)
    }

    fn make_group_with_id(
        &mut self,
        group_id: GroupId,
        axis: Axis,
        windows: Vec<WindowId>,
        selected: usize,
    ) -> NodeId {
        let id = self.allocate_node(BspNode::Group {
            id: group_id,
            axis,
            windows: windows.clone(),
            selected,
        });
        for window in windows {
            self.window_nodes.insert(window, id);
        }
        id
    }

    fn last_group(&self, node: NodeId) -> Result<NodeId, BspError> {
        match self.nodes.get(&node) {
            Some(BspNode::Group { .. }) => Ok(node),
            Some(BspNode::Split { second, .. }) => self.last_group(*second),
            None => Err(BspError::InvariantViolation(
                "tree references a missing node".into(),
            )),
        }
    }

    fn depth(&self, mut node: NodeId) -> usize {
        let mut depth = 0;
        while let Some(parent) = self.parent_of(node) {
            depth += 1;
            node = parent;
        }
        depth
    }

    fn parent_of(&self, child: NodeId) -> Option<NodeId> {
        self.nodes.iter().find_map(|(id, node)| match node {
            BspNode::Split { first, second, .. } if *first == child || *second == child => {
                Some(*id)
            }
            _ => None,
        })
    }

    fn replace_child(
        &mut self,
        parent: Option<NodeId>,
        old: NodeId,
        new: NodeId,
    ) -> Result<(), BspError> {
        let Some(parent) = parent else {
            if self.root != Some(old) {
                return Err(BspError::InvariantViolation(
                    "root replacement does not match".into(),
                ));
            }
            self.root = Some(new);
            return Ok(());
        };
        let Some(BspNode::Split { first, second, .. }) = self.nodes.get_mut(&parent) else {
            return Err(BspError::InvariantViolation(
                "parent is missing or not a split".into(),
            ));
        };
        if *first == old {
            *first = new;
        } else if *second == old {
            *second = new;
        } else {
            return Err(BspError::InvariantViolation(
                "parent does not contain child".into(),
            ));
        }
        Ok(())
    }

    fn remove_from_group(
        &mut self,
        group: NodeId,
        window: WindowId,
        collapse_empty: bool,
    ) -> Result<(), BspError> {
        let BspNode::Group { windows, selected, .. } = self
            .nodes
            .get_mut(&group)
            .ok_or_else(|| BspError::InvariantViolation("window group is missing".into()))?
        else {
            return Err(BspError::InvariantViolation(
                "window index points to a split".into(),
            ));
        };
        let position =
            windows.iter().position(|candidate| *candidate == window).ok_or_else(|| {
                BspError::InvariantViolation("window is absent from indexed group".into())
            })?;
        windows.remove(position);
        self.window_nodes.remove(&window);
        if !windows.is_empty() {
            if position < *selected {
                *selected -= 1;
            } else if *selected >= windows.len() {
                *selected = windows.len() - 1;
            }
            return Ok(());
        }
        if !collapse_empty {
            return Err(BspError::InvariantViolation(
                "operation left an empty group".into(),
            ));
        }
        self.collapse_empty_group(group)
    }

    fn collapse_empty_group(&mut self, group: NodeId) -> Result<(), BspError> {
        let Some(parent) = self.parent_of(group) else {
            if self.root != Some(group) {
                return Err(BspError::InvariantViolation(
                    "orphan group is not the root".into(),
                ));
            }
            self.nodes.remove(&group);
            self.root = None;
            return Ok(());
        };
        let (first, second) = match self.nodes.get(&parent) {
            Some(BspNode::Split { first, second, .. }) => (*first, *second),
            _ => {
                return Err(BspError::InvariantViolation(
                    "group parent is not a split".into(),
                ));
            }
        };
        let sibling = if first == group { second } else { first };
        let grandparent = self.parent_of(parent);
        self.replace_child(grandparent, parent, sibling)?;
        self.nodes.remove(&group);
        self.nodes.remove(&parent);
        Ok(())
    }

    fn collect_groups(
        &self,
        node: NodeId,
        groups: &mut Vec<GroupSnapshot>,
    ) -> Result<(), BspError> {
        match self.nodes.get(&node) {
            Some(BspNode::Split { first, second, .. }) => {
                self.collect_groups(*first, groups)?;
                self.collect_groups(*second, groups)
            }
            Some(BspNode::Group { id, axis, windows, selected }) => {
                groups.push(GroupSnapshot {
                    id: *id,
                    axis: *axis,
                    windows: windows.clone(),
                    selected: *selected,
                });
                Ok(())
            }
            None => Err(BspError::InvariantViolation(
                "tree references a missing node".into(),
            )),
        }
    }

    fn layout_node(
        &self,
        node: NodeId,
        frame: Rect,
        gaps: Gaps,
        constraints: &BTreeMap<WindowId, WindowConstraints>,
        output: &mut BTreeMap<WindowId, Rect>,
    ) -> Result<(), BspError> {
        match self.nodes.get(&node) {
            Some(BspNode::Group { windows, selected, .. }) => {
                let frame = windows
                    .get(*selected)
                    .and_then(|window| constraints.get(window))
                    .map(|constraints| constrain_frame(frame, *constraints))
                    .unwrap_or(frame);
                for window in windows {
                    output.insert(*window, frame);
                }
                Ok(())
            }
            Some(BspNode::Split { axis, ratio, first, second }) => {
                let horizontal = *axis == Axis::Horizontal;
                let gap = if horizontal {
                    gaps.horizontal
                } else {
                    gaps.vertical
                };
                let usable = ((if horizontal {
                    frame.size.width
                } else {
                    frame.size.height
                }) - gap)
                    .max(0.0);
                let first_constraints =
                    self.subtree_axis_constraints(*first, horizontal, constraints, gaps)?;
                let second_constraints =
                    self.subtree_axis_constraints(*second, horizontal, constraints, gaps)?;
                let lengths = solve_axis_lengths(
                    &[
                        AxisConstraints {
                            weight: ratio.get(),
                            ..first_constraints
                        },
                        AxisConstraints {
                            weight: 1.0 - ratio.get(),
                            ..second_constraints
                        },
                    ],
                    usable,
                );
                let first_length = lengths.first().copied().unwrap_or(usable * ratio.get());
                let second_length = lengths.get(1).copied().unwrap_or(0.0);
                let (first_frame, second_frame) =
                    split_frame(frame, *axis, first_length, second_length, gap);
                self.layout_node(*first, first_frame, gaps, constraints, output)?;
                self.layout_node(*second, second_frame, gaps, constraints, output)
            }
            None => Err(BspError::InvariantViolation(
                "layout references a missing node".into(),
            )),
        }
    }

    fn subtree_axis_constraints(
        &self,
        node: NodeId,
        horizontal: bool,
        constraints: &BTreeMap<WindowId, WindowConstraints>,
        gaps: Gaps,
    ) -> Result<AxisConstraints, BspError> {
        match self.nodes.get(&node) {
            Some(BspNode::Group { windows, selected, .. }) => Ok(windows
                .get(*selected)
                .and_then(|window| constraints.get(window))
                .copied()
                .map(|constraints| constraints.for_axis(horizontal))
                .unwrap_or(AxisConstraints {
                    weight: 1.0,
                    can_grow: true,
                    ..Default::default()
                })),
            Some(BspNode::Split { axis, first, second, .. }) => {
                let first = self.subtree_axis_constraints(*first, horizontal, constraints, gaps)?;
                let second =
                    self.subtree_axis_constraints(*second, horizontal, constraints, gaps)?;
                if (*axis == Axis::Horizontal) == horizontal {
                    let gap = if horizontal {
                        gaps.horizontal
                    } else {
                        gaps.vertical
                    };
                    Ok(AxisConstraints {
                        min: first.min + second.min + gap,
                        fixed: first.fixed.zip(second.fixed).map(|(a, b)| a + b + gap),
                        max: first.max.zip(second.max).map(|(a, b)| a + b + gap),
                        weight: 1.0,
                        can_grow: first.can_grow || second.can_grow,
                    })
                } else {
                    Ok(AxisConstraints {
                        min: first.min.max(second.min),
                        fixed: first.fixed.zip(second.fixed).map(|(a, b)| a.max(b)),
                        max: None,
                        weight: 1.0,
                        can_grow: first.can_grow || second.can_grow,
                    })
                }
            }
            None => Err(BspError::InvariantViolation(
                "constraint walk references a missing node".into(),
            )),
        }
    }

    fn validate_node(
        &self,
        node: NodeId,
        visited: &mut BTreeSet<NodeId>,
        windows: &mut BTreeMap<WindowId, NodeId>,
    ) -> Result<(), BspError> {
        if !visited.insert(node) {
            return Err(BspError::InvariantViolation(
                "tree has a cycle or shared child".into(),
            ));
        }
        match self.nodes.get(&node) {
            Some(BspNode::Split { ratio, first, second, .. }) => {
                Ratio::new(ratio.get())?;
                if first == second {
                    return Err(BspError::InvariantViolation(
                        "split children are identical".into(),
                    ));
                }
                self.validate_node(*first, visited, windows)?;
                self.validate_node(*second, visited, windows)
            }
            Some(BspNode::Group { windows: members, selected, .. }) => {
                if members.is_empty() {
                    return Err(BspError::InvariantViolation("group is empty".into()));
                }
                if *selected >= members.len() {
                    return Err(BspError::InvariantViolation(
                        "group selection is out of bounds".into(),
                    ));
                }
                for window in members {
                    if windows.insert(*window, node).is_some() {
                        return Err(BspError::InvariantViolation(
                            "window occurs in more than one group".into(),
                        ));
                    }
                }
                Ok(())
            }
            None => Err(BspError::InvariantViolation(
                "tree references a missing node".into(),
            )),
        }
    }
}

fn inset_frame(frame: Rect, gaps: Gaps) -> Rect {
    Rect {
        origin: Point {
            x: frame.origin.x + gaps.left,
            y: frame.origin.y + gaps.top,
        },
        size: Size {
            width: (frame.size.width - gaps.left - gaps.right).max(0.0),
            height: (frame.size.height - gaps.top - gaps.bottom).max(0.0),
        },
    }
}

fn split_frame(
    frame: Rect,
    axis: Axis,
    first_length: f64,
    second_length: f64,
    gap: f64,
) -> (Rect, Rect) {
    match axis {
        Axis::Horizontal => (
            Rect {
                size: Size {
                    width: first_length,
                    ..frame.size
                },
                ..frame
            },
            Rect {
                origin: Point {
                    x: frame.origin.x + first_length + gap,
                    ..frame.origin
                },
                size: Size {
                    width: second_length,
                    ..frame.size
                },
            },
        ),
        Axis::Vertical => (
            Rect {
                size: Size {
                    height: first_length,
                    ..frame.size
                },
                ..frame
            },
            Rect {
                origin: Point {
                    y: frame.origin.y + first_length + gap,
                    ..frame.origin
                },
                size: Size {
                    height: second_length,
                    ..frame.size
                },
            },
        ),
    }
}

fn constrain_frame(mut frame: Rect, constraints: WindowConstraints) -> Rect {
    let width = constraints.for_axis(true);
    let height = constraints.for_axis(false);
    frame.size.width = constrained_length(frame.size.width, width);
    frame.size.height = constrained_length(frame.size.height, height);
    frame
}

fn constrained_length(available: f64, constraints: AxisConstraints) -> f64 {
    let desired = constraints.fixed.unwrap_or(available).max(constraints.min);
    desired.min(constraints.max.unwrap_or(desired)).min(available).max(0.0)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::core::ids::ApplicationId;

    fn window(index: u32) -> WindowId {
        WindowId::new(ApplicationId(1), NonZeroU32::new(index).unwrap())
    }

    #[test]
    fn join_and_unjoin_keep_each_window_in_exactly_one_group() {
        let mut tree = BspTree::default();
        tree.insert_after(None, window(1)).unwrap();
        tree.insert_after(Some(window(1)), window(2)).unwrap();
        tree.insert_after(Some(window(2)), window(3)).unwrap();

        assert!(tree.join(window(3), window(1)).unwrap());
        tree.validate().unwrap();
        assert_eq!(tree.groups().unwrap(), vec![
            GroupSnapshot {
                id: GroupId(1),
                axis: Axis::Horizontal,
                windows: vec![window(1), window(3)],
                selected: 1,
            },
            GroupSnapshot {
                id: GroupId(2),
                axis: Axis::Horizontal,
                windows: vec![window(2)],
                selected: 0,
            },
        ]);

        assert!(tree.unjoin(window(3)).unwrap());
        tree.validate().unwrap();
        assert_eq!(tree.windows().collect::<Vec<_>>(), vec![
            window(1),
            window(2),
            window(3)
        ]);
    }

    #[test]
    fn interaction_mutations_change_the_pure_tree_without_breaking_invariants() {
        let mut tree = BspTree::default();
        tree.insert_after(None, window(1)).unwrap();
        tree.insert_after(Some(window(1)), window(2)).unwrap();
        let frame = Rect::new(0.0, 0.0, 1000.0, 800.0).unwrap();
        let gaps = Gaps::default();

        let balanced = tree.layout(frame, gaps, &BTreeMap::new()).unwrap();
        tree.resize(window(1), 0.1).unwrap();
        let resized = tree.layout(frame, gaps, &BTreeMap::new()).unwrap();
        assert!(resized[&window(1)].size.width > balanced[&window(1)].size.width);

        tree.toggle_orientation(window(1)).unwrap();
        let vertical = tree.layout(frame, gaps, &BTreeMap::new()).unwrap();
        assert!(vertical[&window(2)].origin.y > vertical[&window(1)].origin.y);

        tree.swap(window(1), window(2)).unwrap();
        tree.validate().unwrap();
        assert_eq!(tree.groups().unwrap()[0].windows, vec![window(2)]);

        tree.toggle_fullscreen(window(2), false).unwrap();
        let fullscreen = tree.layout(frame, gaps, &BTreeMap::new()).unwrap();
        assert_eq!(fullscreen.len(), 1);
        assert_eq!(fullscreen[&window(2)], frame);
    }

    #[test]
    fn removing_the_last_member_collapses_empty_branches() {
        let mut tree = BspTree::default();
        tree.insert_after(None, window(1)).unwrap();
        tree.insert_after(Some(window(1)), window(2)).unwrap();
        tree.remove(window(1)).unwrap();
        tree.validate().unwrap();
        assert_eq!(tree.groups().unwrap()[0].windows, vec![window(2)]);
        tree.remove(window(2)).unwrap();
        tree.validate().unwrap();
        assert!(tree.is_empty());
    }

    #[test]
    fn ratio_rejects_nan_and_degenerate_splits() {
        assert!(matches!(Ratio::new(f64::NAN), Err(BspError::InvalidRatio(_))));
        assert_eq!(Ratio::new(0.0), Err(BspError::InvalidRatio(0.0)));
        assert_eq!(Ratio::new(0.5).unwrap().get(), 0.5);
    }

    #[test]
    fn layout_applies_outer_and_axis_specific_inner_gaps() {
        let mut tree = BspTree::default();
        tree.insert_after(None, window(1)).unwrap();
        tree.insert_after(Some(window(1)), window(2)).unwrap();
        let frame = Rect::new(-100.0, 20.0, 1000.0, 600.0).unwrap();
        let gaps = Gaps {
            top: 10.0,
            left: 20.0,
            bottom: 30.0,
            right: 40.0,
            horizontal: 12.0,
            vertical: 8.0,
        };

        let frames = tree.layout(frame, gaps, &BTreeMap::new()).unwrap();
        assert_eq!(frames[&window(1)], Rect::new(-80.0, 30.0, 464.0, 560.0).unwrap());
        assert_eq!(frames[&window(2)], Rect::new(396.0, 30.0, 464.0, 560.0).unwrap());
    }

    #[test]
    fn layout_reserves_fixed_window_size_before_distributing_space() {
        let mut tree = BspTree::default();
        tree.insert_after(None, window(1)).unwrap();
        tree.insert_after(Some(window(1)), window(2)).unwrap();
        let constraints = BTreeMap::from([(window(1), WindowConstraints {
            resizable: false,
            preferred_size: Size { width: 300.0, height: 200.0 },
            min_size: None,
            max_size: None,
        })]);

        let frames = tree
            .layout(
                Rect::new(0.0, 0.0, 1000.0, 600.0).unwrap(),
                Gaps::default(),
                &constraints,
            )
            .unwrap();
        assert_eq!(frames[&window(1)], Rect::new(0.0, 0.0, 300.0, 200.0).unwrap());
        assert_eq!(frames[&window(2)], Rect::new(300.0, 0.0, 700.0, 600.0).unwrap());
    }
}
