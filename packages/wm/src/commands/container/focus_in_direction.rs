use anyhow::Context;
use wm_common::{Direction, Point, TilingDirection, WindowState};

use super::set_focused_descendant;
use crate::{
  models::{Container, TilingContainer},
  traits::{
    CommonGetters, PositionGetters, TilingDirectionGetters, WindowGetters,
  },
  wm_state::WmState,
};

pub fn focus_in_direction(
  origin_container: &Container,
  direction: &Direction,
  state: &mut WmState,
) -> anyhow::Result<()> {
  let focus_target = match origin_container {
    Container::TilingWindow(_) => {
      // If a suitable focus target isn't found in the current workspace,
      // attempt to find a workspace in the given direction.
      tiling_focus_target(origin_container, direction)?.map_or_else(
        || workspace_focus_target(origin_container, direction, state),
        |container| Ok(Some(container)),
      )?
    }
    Container::NonTilingWindow(ref non_tiling_window) => {
      match non_tiling_window.state() {
        WindowState::Floating(_) => {
          match floating_focus_target(origin_container, direction) {
            Some(focus_target) => Some(focus_target),
            None => {
              workspace_focus_target(origin_container, direction, state)?
            }
          }
        }
        WindowState::Fullscreen(_) => {
          workspace_focus_target(origin_container, direction, state)?
        }
        _ => None,
      }
    }
    Container::Workspace(_) => {
      workspace_focus_target(origin_container, direction, state)?
    }
    _ => None,
  };

  // Set focus to the target container.
  if let Some(focus_target) = focus_target {
    set_focused_descendant(&focus_target, None);
    state.pending_sync.queue_focus_change().queue_cursor_jump();
  }

  Ok(())
}

fn floating_focus_target(
  origin_container: &Container,
  direction: &Direction,
) -> Option<Container> {
  let is_floating = |sibling: &Container| {
    sibling.as_non_tiling_window().is_some_and(|window| {
      matches!(window.state(), WindowState::Floating(_))
    })
  };

  // let mut floating_siblings =
  //   origin_container.siblings().filter(is_floating);

  let get_pos = |c: &Container| {
    c.to_rect()
      .map(|r| Point::from_xy(r.x(), r.y()))
      .unwrap_or(Point::min())
  };

  let origin_position = get_pos(origin_container);

  let mut floating_siblings_with_position: Vec<_> = origin_container
    .self_and_siblings()
    .filter(|s| s.id() != origin_container.id())
    .filter(is_floating)
    .map(|s| (s.clone(), get_pos(&s.clone())))
    .collect();

  match direction {
    Direction::Left | Direction::Right => {
      floating_siblings_with_position.sort_by(|a, b| a.1.x.cmp(&b.1.x));
    }
    Direction::Up | Direction::Down => {
      floating_siblings_with_position.sort_by(|a, b| a.1.y.cmp(&b.1.y));
    }
  }

  // Wrap if next/previous floating window is not found.
  match direction {
    Direction::Left => floating_siblings_with_position
      .into_iter()
      .filter(|(_, p)| p.x < origin_position.x)
      .map(|(s, _)| s.clone())
      .last(),
    Direction::Right => floating_siblings_with_position
      .into_iter()
      .find(|(_, p)| p.x > origin_position.x)
      .map(|(s, _)| s.clone()),
    Direction::Up => floating_siblings_with_position
      .into_iter()
      .filter(|(_, p)| p.y < origin_position.y)
      .map(|(s, _)| s.clone())
      .last(),
    Direction::Down => floating_siblings_with_position
      .into_iter()
      .find(|(_, p)| p.y > origin_position.y)
      .map(|(s, _)| s.clone()),
  }
}

/// Gets a focus target within the current workspace. Traverse upwards from
/// the origin container to find an adjacent container that can be focused.
fn tiling_focus_target(
  origin_container: &Container,
  direction: &Direction,
) -> anyhow::Result<Option<Container>> {
  let tiling_direction = TilingDirection::from_direction(direction);
  let mut origin_or_ancestor = origin_container.clone();

  // Traverse upwards from the focused container. Stop searching when a
  // workspace is encountered.
  while !origin_or_ancestor.is_workspace() {
    let parent = origin_or_ancestor
      .parent()
      .and_then(|parent| parent.as_direction_container().ok())
      .context("No direction container.")?;

    // Skip if the tiling direction doesn't match.
    if parent.tiling_direction() != tiling_direction {
      origin_or_ancestor = parent.into();
      continue;
    }

    // Get the next/prev tiling sibling depending on the tiling direction.
    let focus_target = match direction {
      Direction::Up | Direction::Left => origin_or_ancestor
        .prev_siblings()
        .find_map(|c| c.as_tiling_container().ok()),
      _ => origin_or_ancestor
        .next_siblings()
        .find_map(|c| c.as_tiling_container().ok()),
    };

    match focus_target {
      Some(target) => {
        // Return once a suitable focus target is found.
        return Ok(match target {
          TilingContainer::TilingWindow(_) => Some(target.into()),
          TilingContainer::Split(split) => split
            .descendant_in_direction(&direction.inverse())
            .map(Into::into),
        });
      }
      None => origin_or_ancestor = parent.into(),
    }
  }

  Ok(None)
}

/// Gets a focus target outside of the current workspace in the given
/// direction.
///
/// This will descend into the workspace in the given direction, and will
/// always return a tiling container. This makes it different from the
/// `focus_workspace` command with `FocusWorkspaceTarget::Direction`.
fn workspace_focus_target(
  origin_container: &Container,
  direction: &Direction,
  state: &WmState,
) -> anyhow::Result<Option<Container>> {
  let monitor = origin_container.monitor().context("No monitor.")?;

  let target_workspace = state
    .monitor_in_direction(&monitor, direction)?
    .and_then(|monitor| monitor.displayed_workspace());

  let focused_fullscreen = target_workspace
    .as_ref()
    .and_then(|workspace| workspace.descendant_focus_order().next())
    .filter(|focused| match focused {
      Container::NonTilingWindow(window) => {
        matches!(window.state(), WindowState::Fullscreen(_))
      }
      _ => false,
    });

  let focus_target = focused_fullscreen
    .or_else(|| {
      target_workspace.as_ref().and_then(|w| {
        w.child_focus_order()
          .next()
          .and_then(|c| c.as_non_tiling_window().cloned())
          .map(Into::into)
      })
    })
    .or_else(|| {
      target_workspace.as_ref().and_then(|workspace| {
        workspace
          .descendant_in_direction(&direction.inverse())
          .map(Into::into)
      })
    })
    .or(target_workspace.map(Into::into));

  Ok(focus_target)
}
