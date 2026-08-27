use serde::{Deserialize, Serialize};

use super::error::CoreError;
use super::geometry::Rect;

const SMART_WIDTH_RATIO: f64 = 0.80;
const SMART_HEIGHT_RATIO: f64 = 0.93;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FloatingPlacement {
    pub position: Option<FloatingPosition>,
    pub size: Option<FloatingSize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloatingPosition {
    Center,
    Normalized { x: f64, y: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FloatingSize {
    Points {
        width: Option<f64>,
        height: Option<f64>,
    },
    Smart,
}

impl FloatingPlacement {
    pub fn resolve(self, current: Rect, display: Rect) -> Result<Rect, CoreError> {
        let mut width = current.size.width;
        let mut height = current.size.height;
        match self.size {
            Some(FloatingSize::Points {
                width: configured_width,
                height: configured_height,
            }) => {
                if let Some(value) = configured_width {
                    validate_positive(value, "floating width")?;
                    width = value;
                }
                if let Some(value) = configured_height {
                    validate_positive(value, "floating height")?;
                    height = value;
                }
            }
            Some(FloatingSize::Smart) => {
                width = display.size.width * SMART_WIDTH_RATIO;
                height = display.size.height * SMART_HEIGHT_RATIO;
            }
            None => {}
        }

        let mut x = current.origin.x;
        let mut y = current.origin.y;
        if let Some(position) = self.position {
            let (normalized_x, normalized_y) = match position {
                FloatingPosition::Center => (0.5, 0.5),
                FloatingPosition::Normalized { x, y } => {
                    validate_normalized(x, "floating x")?;
                    validate_normalized(y, "floating y")?;
                    (x, y)
                }
            };
            x = display.origin.x + (display.size.width - width).max(0.0) * normalized_x;
            y = display.origin.y + (display.size.height - height).max(0.0) * normalized_y;
        }
        Rect::new(x, y, width, height).map_err(|error| {
            CoreError::InvalidCommand(format!("invalid floating placement: {error}"))
        })
    }
}

fn validate_positive(value: f64, name: &str) -> Result<(), CoreError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(CoreError::InvalidCommand(format!(
            "{name} must be finite and greater than zero"
        )))
    }
}

fn validate_normalized(value: f64, name: &str) -> Result<(), CoreError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(CoreError::InvalidCommand(format!(
            "{name} must be within 0.0..=1.0"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect::new(x, y, width, height).unwrap()
    }

    #[test]
    fn resolves_smart_centered_frame_in_display_coordinates() {
        let resolved = FloatingPlacement {
            position: Some(FloatingPosition::Center),
            size: Some(FloatingSize::Smart),
        }
        .resolve(
            rect(20.0, 30.0, 400.0, 300.0),
            rect(-1200.0, 50.0, 1000.0, 800.0),
        )
        .unwrap();
        assert_eq!(resolved, rect(-1100.0, 78.0, 800.0, 744.0));
    }

    #[test]
    fn partial_size_preserves_unspecified_dimension_and_origin() {
        let resolved = FloatingPlacement {
            position: None,
            size: Some(FloatingSize::Points {
                width: Some(640.0),
                height: None,
            }),
        }
        .resolve(rect(20.0, 30.0, 400.0, 300.0), rect(0.0, 0.0, 1000.0, 800.0))
        .unwrap();
        assert_eq!(resolved, rect(20.0, 30.0, 640.0, 300.0));
    }
}
