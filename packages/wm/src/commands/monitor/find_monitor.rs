use std::ops::ControlFlow;

use wm_common::Point;

use crate::{
  models::{Monitor, RootContainer},
  traits::PositionGetters,
};

// Finds the monitor that contains the given anchor point.
pub fn find_monitor_by_anchor_point(
  root: &RootContainer,
  anchor: &Point,
) -> anyhow::Result<Option<Monitor>> {
  let mut monitor: Option<Monitor> = None;

  let _ = root.monitors().iter().try_for_each(|m| {
    let check_result = m.to_rect().map(|rect| rect.contains_point(anchor));
    if let Ok(true) = check_result {
      monitor = Some(m.clone());
      ControlFlow::Break(())
    } else {
      ControlFlow::Continue(())
    }
  });

  anyhow::Ok(monitor)
}
