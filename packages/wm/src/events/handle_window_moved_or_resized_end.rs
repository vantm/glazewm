use anyhow::Context;
use tracing::info;
use uuid::Uuid;
use wm_common::{
  try_warn, ActiveDragOperation, LengthValue, Point, Rect,
  TilingDirection, WindowState,
};
use wm_platform::{NativeWindow, Platform};

use crate::{
  commands::{
    container::{move_container_within_tree, wrap_in_split_container},
    monitor::find_monitor_by_anchor_point,
    window::{
      move_window_to_workspace, resize_window, update_window_state,
    },
  },
  models::{
    DirectionContainer, NonTilingWindow, SplitContainer, TilingContainer,
    TilingWindow, WindowContainer,
  },
  traits::{
    CommonGetters, PositionGetters, TilingDirectionGetters, WindowGetters,
  },
  user_config::UserConfig,
  wm_state::WmState,
};

/// Handles the event for when a window is finished being moved or resized
/// by the user (e.g. via the window's drag handles).
///
/// This resizes the window if it's a tiling window and attach a dragged
/// floating window.
pub fn handle_window_moved_or_resized_end(
  native_window: &NativeWindow,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  // Don't update state on resize events if the WM is paused.
  if state.is_paused {
    return Ok(());
  }

  let found_window = state.window_from_native(native_window);

  if let Some(window) = found_window {
    match &window {
      WindowContainer::NonTilingWindow(window) => {
        if let Some(active_drag) = window.active_drag() {
          if active_drag.is_from_tiling
            && active_drag.operation == Some(ActiveDragOperation::Moving)
          {
            // Window is a temporary floating window that should be
            // reverted back to tiling.
            drop_as_tiling_window(window, state, config)?;
          }
        }
      }
      WindowContainer::TilingWindow(window) => {
        let monitor1 = window.monitor().map_or(Uuid::nil(), |m| m.id());
        if let Err(e) = try_arrange_window_between_monitors(
          &window.as_window_container()?,
          state,
          config,
        ) {
          info!("Skipping monitor arrangement due to: {e:#}");
        }
        let monitor2 = window.monitor().map_or(Uuid::nil(), |m| m.id());

        if monitor1 != monitor2 {
          return Ok(());
        }

        let parent = window.parent().context("No parent.")?;

        let old_rect = window.to_rect()?;

        // Snap window to its original position if it's the only window in
        // the workspace.
        if parent.is_workspace() && window.tiling_siblings().count() == 0 {
          state.pending_sync.queue_container_to_redraw(window.clone());
          return Ok(());
        }

        let new_rect = try_warn!(window.native().refresh_frame_position());
        let width_delta = new_rect.width() - old_rect.width();
        let height_delta = new_rect.height() - old_rect.height();

        resize_window(
          &window.clone().into(),
          Some(LengthValue::from_px(width_delta)),
          Some(LengthValue::from_px(height_delta)),
          state,
        )?;
      }
    }

    window.set_active_drag(None);
  }

  Ok(())
}

fn move_window_to_position(
  window: &WindowContainer,
  position: &Point,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let mouse_workspace = state
    .monitor_at_point(position)
    .and_then(|monitor| monitor.displayed_workspace())
    .or_else(|| window.workspace())
    .context("No workspace.")?;

  // Get the workspace, split containers, and other windows under the
  // dragged window.
  let containers_at_pos = state
    .containers_at_point(&mouse_workspace.clone().into(), &position)
    .into_iter()
    .filter(|container| container.id() != window.id());

  // Get the deepest direction container under the dragged window.
  let target_parent: DirectionContainer = containers_at_pos
    .filter_map(|container| container.as_direction_container().ok())
    .fold(mouse_workspace.into(), |acc, container| {
      if container.ancestors().count() > acc.ancestors().count() {
        container
      } else {
        acc
      }
    });

  // If the target parent has no children (i.e. an empty workspace), then
  // just do nothing.
  if target_parent.tiling_children().count() == 0 {
    return Ok(());
  }

  let nearest_container = target_parent
    .children()
    .into_iter()
    .filter_map(|container| container.as_tiling_container().ok())
    .try_fold(None, |acc: Option<TilingContainer>, container| match acc {
      Some(acc) => {
        let is_nearer = acc.to_rect()?.distance_to_point(&position)
          < container.to_rect()?.distance_to_point(&position);

        anyhow::Ok(Some(if is_nearer { acc } else { container }))
      }
      None => Ok(Some(container)),
    })?
    .context("No nearest container.")?;

  let tiling_direction = target_parent.tiling_direction();
  let drop_position =
    drop_position(&position, &nearest_container.to_rect()?);

  let should_split = nearest_container.is_tiling_window()
    && match tiling_direction {
      TilingDirection::Horizontal => {
        drop_position == DropPosition::Top
          || drop_position == DropPosition::Bottom
      }
      TilingDirection::Vertical => {
        drop_position == DropPosition::Left
          || drop_position == DropPosition::Right
      }
    };

  if should_split {
    let split_container = SplitContainer::new(
      tiling_direction.inverse(),
      config.value.gaps.clone(),
    );

    wrap_in_split_container(
      &split_container,
      &target_parent.clone().into(),
      &[nearest_container],
    )?;

    let target_index = match drop_position {
      DropPosition::Top | DropPosition::Left => 0,
      _ => 1,
    };

    move_container_within_tree(
      &window.clone().into(),
      &split_container.into(),
      target_index,
      state,
    )?;
  } else {
    let target_index = match drop_position {
      DropPosition::Top | DropPosition::Left => nearest_container.index(),
      _ => nearest_container.index() + 1,
    };

    move_container_within_tree(
      &window.clone().into(),
      &target_parent.clone().into(),
      target_index,
      state,
    )?;
  }

  state.pending_sync.queue_container_to_redraw(target_parent);

  Ok(())
}

/// Handles transition from temporary floating window to tiling window on
/// drag end.
fn drop_as_tiling_window(
  moved_window: &NonTilingWindow,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  info!(
    "Tiling window drag ended: {}",
    moved_window.as_window_container()?
  );

  let mouse_pos = Platform::mouse_position()?;
  let workspace = moved_window.workspace().context("No workspace.")?;

  // Get the workspace, split containers, and other windows under the
  // dragged window.
  let containers_at_pos = state
    .containers_at_point(&workspace.clone().into(), &mouse_pos)
    .into_iter()
    .filter(|container| container.id() != moved_window.id());

  // Get the deepest direction container under the dragged window.
  let target_parent: DirectionContainer = containers_at_pos
    .filter_map(|container| container.as_direction_container().ok())
    .fold(workspace.into(), |acc, container| {
      if container.ancestors().count() > acc.ancestors().count() {
        container
      } else {
        acc
      }
    });

  // If the target parent has no children (i.e. an empty workspace), then
  // add the window directly.
  if target_parent.tiling_children().count() == 0 {
    update_window_state(
      moved_window.clone().into(),
      WindowState::Tiling,
      state,
      config,
    )?;

    return Ok(());
  }

  let nearest_container = target_parent
    .children()
    .into_iter()
    .filter_map(|container| container.as_tiling_container().ok())
    .try_fold(None, |acc: Option<TilingContainer>, container| match acc {
      Some(acc) => {
        let is_nearer = acc.to_rect()?.distance_to_point(&mouse_pos)
          < container.to_rect()?.distance_to_point(&mouse_pos);

        anyhow::Ok(Some(if is_nearer { acc } else { container }))
      }
      None => Ok(Some(container)),
    })?
    .context("No nearest container.")?;

  let tiling_direction = target_parent.tiling_direction();
  let drop_position =
    drop_position(&mouse_pos, &nearest_container.to_rect()?);

  let moved_window = update_window_state(
    moved_window.clone().into(),
    WindowState::Tiling,
    state,
    config,
  )?;

  let should_split = nearest_container.is_tiling_window()
    && match tiling_direction {
      TilingDirection::Horizontal => {
        drop_position == DropPosition::Top
          || drop_position == DropPosition::Bottom
      }
      TilingDirection::Vertical => {
        drop_position == DropPosition::Left
          || drop_position == DropPosition::Right
      }
    };

  if should_split {
    let split_container = SplitContainer::new(
      tiling_direction.inverse(),
      config.value.gaps.clone(),
    );

    wrap_in_split_container(
      &split_container,
      &target_parent.clone().into(),
      &[nearest_container],
    )?;

    let target_index = match drop_position {
      DropPosition::Top | DropPosition::Left => 0,
      _ => 1,
    };

    move_container_within_tree(
      &moved_window.clone().into(),
      &split_container.into(),
      target_index,
      state,
    )?;
  } else {
    let target_index = match drop_position {
      DropPosition::Top | DropPosition::Left => nearest_container.index(),
      _ => nearest_container.index() + 1,
    };

    move_container_within_tree(
      &moved_window.clone().into(),
      &target_parent.clone().into(),
      target_index,
      state,
    )?;
  }

  state.pending_sync.queue_container_to_redraw(target_parent);

  Ok(())
}

/// Represents where the window was dropped over another.
#[derive(Debug, Clone, PartialEq)]
enum DropPosition {
  Top,
  Bottom,
  Left,
  Right,
}

/// Gets the drop position for a window based on the mouse position.
///
/// This approach divides the window rect into an "X", creating four
/// triangular quadrants, to determine which side the cursor is closest to.
fn drop_position(mouse_pos: &Point, rect: &Rect) -> DropPosition {
  let delta_x = mouse_pos.x - rect.center_point().x;
  let delta_y = mouse_pos.y - rect.center_point().y;

  if delta_x.abs() > delta_y.abs() {
    // Window is in the left or right triangle.
    if delta_x > 0 {
      DropPosition::Right
    } else {
      DropPosition::Left
    }
  } else {
    // Window is in the top or bottom triangle.
    if delta_y > 0 {
      DropPosition::Bottom
    } else {
      DropPosition::Top
    }
  }
}

fn try_arrange_window_between_monitors(
  window: &WindowContainer,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let root = &state.root_container;
  if root.monitors().len() < 2 {
    return Ok(());
  }

  let anchor = Platform::mouse_position()?;

  find_monitor_by_anchor_point(root, &anchor)?
    .and_then(|target| window.monitor().map(|current| (target, current)))
    .and_then(|(target, current)| {
      if target.id() == current.id() {
        None
      } else {
        Some(target)
      }
    })
    .and_then(|target| {
      target
        .workspaces()
        .iter()
        .find(|x| x.is_displayed())
        .cloned()
    })
    .map(|workspace| {
      move_window_to_workspace(
        window.clone(),
        crate::models::WorkspaceTarget::Name(workspace.config().name),
        state,
        config,
      )
      .and_then(|_| {
        Platform::mouse_position().and_then(|mouse_pos| {
          move_window_to_position(window, &mouse_pos, state, config)
        })
      })
    })
    .transpose()
    .map(|_| ())
}
