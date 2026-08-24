use std::collections::BTreeMap;

use crate::core::geometry::{Point, Rect, Size};
use crate::core::ids::{DisplayId, WindowId};
use crate::core::snapshot::CoreSnapshot;

const MIN_ANCHOR_AREA: f64 = 1.0;

pub fn frames_for_display(
    snapshot: &CoreSnapshot,
    display_id: &DisplayId,
) -> Vec<(WindowId, Rect)> {
    let Some(display) = snapshot.displays.iter().find(|display| &display.id == display_id) else {
        return Vec::new();
    };
    let all_displays = snapshot.displays.iter().map(|display| display.frame).collect::<Vec<_>>();
    let windows = snapshot.windows.iter().map(|window| (window.id, window)).collect::<BTreeMap<_, _>>();
    let bundles = snapshot
        .applications
        .iter()
        .map(|application| (application.id, application.bundle_id.as_deref()))
        .collect::<BTreeMap<_, _>>();
    let mut frames = BTreeMap::new();

    for workspace in snapshot.workspaces.iter().filter(|workspace| &workspace.display == display_id) {
        let tiled = workspace
            .groups
            .iter()
            .flat_map(|group| group.windows.iter().copied());
        let members = tiled.chain(workspace.floating_windows.iter().copied());
        if display.active_workspace == Some(workspace.id) {
            for window in members {
                let Some(observed) = windows.get(&window) else { continue };
                if observed.platform_id.is_none() { continue; }
                let frame = workspace.layout_frames.get(&window).copied().unwrap_or_else(|| {
                    if intersection_area(display.frame, observed.frame) > 9.0 {
                        observed.frame
                    } else {
                        centered(display.frame, observed.frame.size)
                    }
                });
                frames.insert(window, frame);
            }
        } else {
            for window in members {
                let Some(observed) = windows.get(&window) else { continue };
                if observed.platform_id.is_none() { continue; }
                frames.insert(
                    window,
                    hidden_frame(
                        display.frame,
                        observed.frame.size,
                        bundles.get(&window.application).copied().flatten(),
                        &all_displays,
                    ),
                );
            }
        }
    }
    frames.into_iter().collect()
}

pub fn hidden_frame(
    display: Rect,
    size: Size,
    bundle_id: Option<&str>,
    all_displays: &[Rect],
) -> Rect {
    let other_displays = all_displays
        .iter()
        .copied()
        .filter(|candidate| *candidate != display)
        .collect::<Vec<_>>();
    let primary = corner_frame(display, size, false, bundle_id);
    let fallback = corner_frame(display, size, true, bundle_id);
    let bottom_y = primary.origin.y;
    let mut breakpoints = Vec::new();
    for frame in std::iter::once(&display).chain(&other_displays) {
        breakpoints.extend([
            frame.origin.x - size.width,
            frame.origin.x,
            frame.origin.x + frame.size.width - size.width,
            frame.origin.x + frame.size.width,
        ]);
    }
    breakpoints.sort_by(f64::total_cmp);
    breakpoints.dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);
    let at = |x| Rect::new(x, bottom_y, size.width, size.height).unwrap();
    let mut xs = vec![
        display.origin.x,
        display.origin.x + display.size.width - size.width,
        display.origin.x + (display.size.width - size.width) / 2.0,
    ];
    xs.extend(breakpoints.iter().copied());

    for interval in breakpoints.windows(2) {
        let (x0, x1) = (interval[0], interval[1]);
        if x1 - x0 <= f64::EPSILON { continue; }
        let anchor0 = intersection_area(display, at(x0));
        let anchor1 = intersection_area(display, at(x1));
        let delta = anchor1 - anchor0;
        if delta.abs() > f64::EPSILON {
            let x = x0 + (MIN_ANCHOR_AREA - anchor0) * (x1 - x0) / delta;
            if (x0..=x1).contains(&x) { xs.push(x); }
        }
        for (index, first) in other_displays.iter().enumerate() {
            let first0 = intersection_area(*first, at(x0));
            let first1 = intersection_area(*first, at(x1));
            for second in &other_displays[index + 1..] {
                let diff0 = first0 - intersection_area(*second, at(x0));
                let diff1 = first1 - intersection_area(*second, at(x1));
                let diff_delta = diff1 - diff0;
                if diff_delta.abs() <= f64::EPSILON { continue; }
                let x = x0 - diff0 * (x1 - x0) / diff_delta;
                if (x0..=x1).contains(&x) { xs.push(x); }
            }
        }
    }

    std::iter::once(primary)
        .chain(std::iter::once(fallback))
        .chain(xs.into_iter().map(at))
        .enumerate()
        .min_by(|(left_preference, left), (right_preference, right)| {
            hidden_score(display, *left, &other_displays, *left_preference)
                .cmp(&hidden_score(display, *right, &other_displays, *right_preference))
        })
        .map(|(_, frame)| frame)
        .expect("hidden frame candidates are never empty")
}

fn hidden_score(
    display: Rect,
    frame: Rect,
    others: &[Rect],
    preference: usize,
) -> (bool, OrderedFloat, OrderedFloat, bool, OrderedFloat, usize) {
    let anchor = intersection_area(display, frame);
    let (other_max, other_total) = others.iter().fold((0.0_f64, 0.0_f64), |(max, total), other| {
        let area = intersection_area(*other, frame);
        (max.max(area), total + area)
    });
    (
        anchor < MIN_ANCHOR_AREA,
        OrderedFloat(other_max),
        OrderedFloat(other_total),
        preference >= 2,
        OrderedFloat(anchor),
        preference,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OrderedFloat(f64);

impl Eq for OrderedFloat {}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering { self.0.total_cmp(&other.0) }
}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

fn corner_frame(display: Rect, size: Size, left: bool, bundle_id: Option<&str>) -> Rect {
    let zoom = bundle_id == Some("us.zoom.xos");
    let offset_x = if zoom { 0.0 } else { 1.0 };
    let offset_y = if zoom { 0.0 } else if left { -1.0 } else { 1.0 };
    let x = if left {
        display.origin.x + offset_x - size.width + 1.0
    } else {
        display.origin.x + display.size.width - offset_x - 1.0
    };
    let y = display.origin.y + display.size.height + offset_y;
    Rect::new(x, y, size.width, size.height).unwrap()
}

fn centered(display: Rect, size: Size) -> Rect {
    Rect {
        origin: Point {
            x: display.origin.x + (display.size.width - size.width) / 2.0,
            y: display.origin.y + (display.size.height - size.height) / 2.0,
        },
        size,
    }
}

fn intersection_area(left: Rect, right: Rect) -> f64 {
    let width = ((left.origin.x + left.size.width).min(right.origin.x + right.size.width)
        - left.origin.x.max(right.origin.x))
    .max(0.0);
    let height = ((left.origin.y + left.size.height).min(right.origin.y + right.size.height)
        - left.origin.y.max(right.origin.y))
    .max(0.0);
    width * height
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_frame_avoids_a_neighboring_portrait_display() {
        let owner = Rect::new(0.0, 0.0, 1440.0, 900.0).unwrap();
        let portrait = Rect::new(1440.0, -500.0, 900.0, 1600.0).unwrap();
        let hidden = hidden_frame(
            owner,
            Size { width: 700.0, height: 500.0 },
            None,
            &[owner, portrait],
        );
        assert!(intersection_area(owner, hidden) >= MIN_ANCHOR_AREA);
        assert_eq!(intersection_area(portrait, hidden), 0.0);
    }

    #[test]
    fn hidden_frame_handles_negative_display_origins() {
        let left = Rect::new(-1920.0, 0.0, 1920.0, 1080.0).unwrap();
        let main = Rect::new(0.0, 0.0, 1728.0, 1117.0).unwrap();
        let hidden = hidden_frame(
            left,
            Size { width: 800.0, height: 600.0 },
            None,
            &[left, main],
        );
        assert!(intersection_area(left, hidden) >= MIN_ANCHOR_AREA);
        assert_eq!(intersection_area(main, hidden), 0.0);
    }
}
