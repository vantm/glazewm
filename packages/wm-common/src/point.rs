/// Represents an x-y coordinate.
#[derive(Debug, Clone)]
pub struct Point {
  pub x: i32,
  pub y: i32,
}

impl Point {
  #[must_use]
  pub fn from_xy(x: i32, y: i32) -> Self {
    Self { x, y }
  }

  #[must_use]
  pub fn min() -> Self {
    Self {
      x: i32::MIN,
      y: i32::MIN,
    }
  }
}
