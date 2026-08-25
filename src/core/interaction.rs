use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::geometry::Rect;
use super::ids::WindowId;

const STICK_RATIO: f64 = 0.6;
const OVERLAP_WEIGHT: f64 = 0.7;
const CENTER_WEIGHT: f64 = 1.0 - OVERLAP_WEIGHT;
const SWITCH_DELTA: f64 = 0.04;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionControlPhase {
    #[default]
    Inactive,
    ShowAllRequested,
    ShowCurrentRequested,
    Active,
    DismissRequested,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DragCandidate {
    pub window: WindowId,
    pub frame: Rect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DragObservation {
    Updated {
        window: WindowId,
        frame: Rect,
        candidates: Vec<DragCandidate>,
    },
    Committed {
        window: WindowId,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DragSnapshot {
    pub window: Option<WindowId>,
    pub origin_frame: Option<Rect>,
    pub target: Option<WindowId>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DragSwapState {
    window: Option<WindowId>,
    origin_frame: Option<Rect>,
    target: Option<WindowId>,
}

#[derive(Clone, Copy)]
struct CandidateScore {
    window: WindowId,
    overlap: f64,
    score: f64,
}

impl DragSwapState {
    pub fn snapshot(&self) -> DragSnapshot {
        DragSnapshot {
            window: self.window,
            origin_frame: self.origin_frame,
            target: self.target,
        }
    }

    pub fn update(
        &mut self,
        window: WindowId,
        frame: Rect,
        candidates: &[DragCandidate],
        threshold: f64,
    ) -> Option<WindowId> {
        if self.window != Some(window) {
            self.window = Some(window);
            self.origin_frame = Some(frame);
            self.target = None;
        }

        let area = frame.size.width * frame.size.height;
        if area <= 0.0 {
            return self.target;
        }
        let threshold = if threshold > 0.0 {
            threshold.min(1.0)
        } else {
            0.5
        };
        let stick_threshold = threshold * STICK_RATIO;
        let center_x = frame.origin.x + frame.size.width * 0.5;
        let center_y = frame.origin.y + frame.size.height * 0.5;
        let diagonal = f64::hypot(frame.size.width, frame.size.height).max(f64::EPSILON);

        let mut scores = candidates
            .iter()
            .filter(|candidate| candidate.window != window)
            .filter_map(|candidate| {
                let intersection = intersection(frame, candidate.frame)?;
                let intersection_area = intersection.size.width * intersection.size.height;
                let candidate_area = candidate.frame.size.width * candidate.frame.size.height;
                let union = area + candidate_area - intersection_area;
                if union <= 0.0 {
                    return None;
                }
                let overlap = intersection_area / union;
                if overlap < stick_threshold {
                    return None;
                }
                let candidate_x = candidate.frame.origin.x + candidate.frame.size.width * 0.5;
                let candidate_y = candidate.frame.origin.y + candidate.frame.size.height * 0.5;
                let distance = f64::hypot(center_x - candidate_x, center_y - candidate_y);
                let candidate_diagonal =
                    f64::hypot(candidate.frame.size.width, candidate.frame.size.height)
                        .max(f64::EPSILON);
                let proximity = 1.0 - (distance / (diagonal + candidate_diagonal)).clamp(0.0, 1.0);
                Some(CandidateScore {
                    window: candidate.window,
                    overlap,
                    score: overlap * OVERLAP_WEIGHT + proximity * CENTER_WEIGHT,
                })
            })
            .collect::<Vec<_>>();
        scores
            .sort_by(|left, right| right.score.partial_cmp(&left.score).unwrap_or(Ordering::Equal));

        let Some(best) = scores.first().copied() else {
            self.target = None;
            return None;
        };
        if let Some(active) = self
            .target
            .and_then(|target| scores.iter().copied().find(|score| score.window == target))
        {
            if active.window != best.window
                && best.overlap >= threshold
                && best.score >= active.score + SWITCH_DELTA
            {
                self.target = Some(best.window);
            }
            return self.target;
        }
        self.target = (best.overlap >= threshold).then_some(best.window);
        self.target
    }

    pub fn commit(&mut self, window: WindowId) -> Option<(WindowId, WindowId)> {
        let swap = (self.window == Some(window))
            .then(|| self.target.map(|target| (window, target)))
            .flatten();
        self.reset();
        swap
    }

    pub fn reset(&mut self) { *self = Self::default(); }
}

fn intersection(left: Rect, right: Rect) -> Option<Rect> {
    let x = left.origin.x.max(right.origin.x);
    let y = left.origin.y.max(right.origin.y);
    let max_x = (left.origin.x + left.size.width).min(right.origin.x + right.size.width);
    let max_y = (left.origin.y + left.size.height).min(right.origin.y + right.size.height);
    (max_x > x && max_y > y).then(|| Rect::new(x, y, max_x - x, max_y - y).unwrap())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::core::ids::ApplicationId;

    fn window(index: u32) -> WindowId {
        WindowId::new(ApplicationId(1), NonZeroU32::new(index).unwrap())
    }

    fn rect(x: f64, width: f64) -> Rect { Rect::new(x, 0.0, width, 100.0).unwrap() }

    #[test]
    fn drag_candidate_uses_overlap_and_hysteresis() {
        let mut state = DragSwapState::default();
        let dragged = window(1);
        let first = DragCandidate {
            window: window(2),
            frame: rect(0.0, 60.0),
        };
        let second = DragCandidate {
            window: window(3),
            frame: rect(0.0, 40.0),
        };
        assert_eq!(
            state.update(dragged, rect(0.0, 100.0), &[first, second], 0.3),
            Some(first.window)
        );

        let weaker_first = DragCandidate {
            window: first.window,
            frame: rect(20.0, 60.0),
        };
        assert_eq!(
            state.update(dragged, rect(0.0, 100.0), &[weaker_first, second], 0.3),
            Some(first.window)
        );
        assert_eq!(state.commit(dragged), Some((dragged, first.window)));
        assert_eq!(state.snapshot(), DragSnapshot::default());
    }

    #[test]
    fn losing_overlap_clears_the_candidate() {
        let mut state = DragSwapState::default();
        let dragged = window(1);
        let candidate = DragCandidate {
            window: window(2),
            frame: rect(0.0, 60.0),
        };
        state.update(dragged, rect(0.0, 100.0), &[candidate], 0.3);
        assert_eq!(
            state.update(dragged, rect(200.0, 100.0), &[candidate], 0.3),
            None
        );
    }
}
