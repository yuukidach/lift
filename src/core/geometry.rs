use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Result<Self, GeometryError> {
        if ![x, y, width, height].into_iter().all(f64::is_finite) {
            return Err(GeometryError::NonFinite);
        }
        if width < 0.0 || height < 0.0 {
            return Err(GeometryError::InvalidSize);
        }
        Ok(Self {
            origin: Point { x, y },
            size: Size { width, height },
        })
    }

    pub fn intersection_area(self, other: Self) -> f64 {
        let left = self.origin.x.max(other.origin.x);
        let top = self.origin.y.max(other.origin.y);
        let right = (self.origin.x + self.size.width).min(other.origin.x + other.size.width);
        let bottom = (self.origin.y + self.size.height).min(other.origin.y + other.size.height);
        (right - left).max(0.0) * (bottom - top).max(0.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GeometryError {
    #[error("geometry contains a non-finite component")]
    NonFinite,
    #[error("geometry size cannot be negative")]
    InvalidSize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_rejects_negative_or_non_finite_dimensions() {
        assert_eq!(Rect::new(0.0, 0.0, -1.0, 20.0), Err(GeometryError::InvalidSize));
        assert_eq!(
            Rect::new(0.0, 0.0, f64::NAN, 20.0),
            Err(GeometryError::NonFinite)
        );
    }

    #[test]
    fn intersection_area_handles_arbitrary_display_origins() {
        let left = Rect::new(-1920.0, 0.0, 1920.0, 1080.0).unwrap();
        let window = Rect::new(-100.0, 100.0, 300.0, 200.0).unwrap();
        assert_eq!(left.intersection_area(window), 20_000.0);
    }
}
