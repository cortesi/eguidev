//! Logical points, vectors, and rectangles.

use egui::{Rect as EguiRect, Vec2 as EguiVec2};
use serde::{Deserialize, Serialize};

fn sanitize_f32(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// A logical point in egui coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Pos2 {
    /// X coordinate in points.
    pub x: f32,
    /// Y coordinate in points.
    pub y: f32,
}

impl From<egui::Pos2> for Pos2 {
    fn from(pos: egui::Pos2) -> Self {
        Self {
            x: sanitize_f32(pos.x),
            y: sanitize_f32(pos.y),
        }
    }
}

impl From<Pos2> for egui::Pos2 {
    fn from(pos: Pos2) -> Self {
        egui::pos2(pos.x, pos.y)
    }
}

/// A 2D vector in egui coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Vec2 {
    /// X component in points.
    pub x: f32,
    /// Y component in points.
    pub y: f32,
}

impl From<EguiVec2> for Vec2 {
    fn from(vec: EguiVec2) -> Self {
        Self {
            x: sanitize_f32(vec.x),
            y: sanitize_f32(vec.y),
        }
    }
}

impl From<Vec2> for EguiVec2 {
    fn from(vec: Vec2) -> Self {
        egui::vec2(vec.x, vec.y)
    }
}

/// Axis-aligned rectangle in egui coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Rect {
    /// Minimum point.
    pub min: Pos2,
    /// Maximum point.
    pub max: Pos2,
}

impl Rect {
    /// Return the center point of the rectangle in egui coordinates.
    pub fn center(self) -> Pos2 {
        Pos2 {
            x: (self.min.x + self.max.x) * 0.5,
            y: (self.min.y + self.max.y) * 0.5,
        }
    }
}

impl From<EguiRect> for Rect {
    fn from(rect: EguiRect) -> Self {
        Self {
            min: Pos2::from(rect.min),
            max: Pos2::from(rect.max),
        }
    }
}

impl From<Rect> for EguiRect {
    fn from(rect: Rect) -> Self {
        Self::from_min_max(rect.min.into(), rect.max.into())
    }
}
