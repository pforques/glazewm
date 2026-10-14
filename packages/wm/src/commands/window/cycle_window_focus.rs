use anyhow::Context;
use tracing::info;
use wm_common::{TilingDirection, WindowState};
use wm_platform::Direction;

use super::update_window_state;
use crate::{
  commands::container::set_focused_descendant,
  models::{Container, WindowContainer, Workspace},
  traits::{CommonGetters, TilingDirectionGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

/// Cycles focus through all windows of the origin container's workspace,
/// including minimized ones, in workspace order (see `windows_in_order`).
///
/// Right/down moves to the next window and left/up to the previous one,
/// wrapping around. A minimized target gets restored in place; if the
/// origin is a tiling window, it gets minimized in exchange, so that the
/// number of visible windows stays the same.
///
/// Returns `false` without changing anything if cycling isn't applicable
/// (direction perpendicular to the workspace's tiling direction, or no
/// other window to cycle to), so that the caller can fall back to regular
/// focus.
pub fn cycle_window_focus(
  origin: &Container,
  direction: &Direction,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<bool> {
  let workspace = match origin {
    Container::Workspace(workspace) => workspace.clone(),
    _ => origin.workspace().context("No workspace.")?,
  };

  if TilingDirection::from_direction(direction)
    != workspace.tiling_direction()
  {
    return Ok(false);
  }

  let windows = windows_in_order(&workspace);

  let slots = windows
    .iter()
    .map(|window| match window.state() {
      WindowState::Tiling => WindowSlot::Tiling,
      WindowState::Minimized => WindowSlot::Minimized,
      _ => WindowSlot::Visible,
    })
    .collect::<Vec<_>>();

  let origin_index =
    windows.iter().position(|window| window.id() == origin.id());

  let is_forward = matches!(direction, Direction::Right | Direction::Down);

  let Some(action) = cycle_action(&slots, origin_index, is_forward) else {
    return Ok(false);
  };

  let target = match action {
    CycleAction::Focus(index) => windows[index].clone(),
    CycleAction::Restore(index) => {
      info!("Cycling focus to minimized window: {}", windows[index]);
      restore_window(windows[index].clone(), state, config)?
    }
    CycleAction::Swap(index) => {
      info!("Swapping window with minimized window: {}", windows[index]);

      // Minimize the origin first so that it keeps its position in the
      // workspace. Its state is converted right away, rather than waiting
      // for the OS minimize event, so that the restored window directly
      // takes up its space. The window is natively minimized on the next
      // platform sync.
      let origin = origin
        .as_window_container()
        .context("Origin is not a window.")?;

      origin.update_native_properties(|properties| {
        properties.is_minimized = true;
      });

      update_window_state(origin, WindowState::Minimized, state, config)?;
      restore_window(windows[index].clone(), state, config)?
    }
  };

  set_focused_descendant(&target.into(), None);

  if !matches!(action, CycleAction::Focus(_)) {
    // Override stray OS focus events caused by the native (un)minimize.
    state.unmanaged_or_minimized_timestamp =
      Some(std::time::Instant::now());
  }

  state.pending_sync.queue_focus_change().queue_cursor_jump();

  Ok(true)
}

/// Gets all windows of a workspace in workspace order, i.e. depth-first
/// tree order. Minimized windows keep their position in this order (see
/// `update_window_state`), so it is stable across minimize/restore and
/// matches the order of the workspace's children in IPC queries.
pub fn windows_in_order(workspace: &Workspace) -> Vec<WindowContainer> {
  let mut stack =
    workspace.children().into_iter().rev().collect::<Vec<_>>();
  let mut windows = Vec::new();

  while let Some(container) = stack.pop() {
    match container.as_window_container() {
      Ok(window) => windows.push(window),
      Err(_) => stack.extend(container.children().into_iter().rev()),
    }
  }

  windows
}

/// Restores a minimized window to its previous state.
fn restore_window(
  window: WindowContainer,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<WindowContainer> {
  let target_state = window.toggled_state(WindowState::Minimized, config);
  update_window_state(window, target_state, state, config)
}

/// Kind of window in a workspace, for cycle target selection.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WindowSlot {
  Tiling,
  Minimized,
  /// Floating or fullscreen window.
  Visible,
}

/// Action to perform when cycling focus.
#[derive(Clone, Copy, Debug, PartialEq)]
enum CycleAction {
  /// Focus the (already visible) window at this index.
  Focus(usize),
  /// Restore the minimized window at this index.
  Restore(usize),
  /// Restore the minimized window at this index and minimize the origin.
  Swap(usize),
}

/// Gets the cycle action, given the windows of a workspace in workspace
/// order and the index of the origin window (`None` if the origin is the
/// workspace itself).
fn cycle_action(
  slots: &[WindowSlot],
  origin_index: Option<usize>,
  is_forward: bool,
) -> Option<CycleAction> {
  let count = slots.len();

  let target_index = match origin_index {
    Some(_) if count < 2 => return None,
    Some(index) if is_forward => (index + 1) % count,
    Some(index) => (index + count - 1) % count,
    None if count == 0 => return None,
    None if is_forward => 0,
    None => count - 1,
  };

  let is_origin_tiling =
    origin_index.is_some_and(|index| slots[index] == WindowSlot::Tiling);

  Some(match slots[target_index] {
    WindowSlot::Minimized if is_origin_tiling => {
      CycleAction::Swap(target_index)
    }
    WindowSlot::Minimized => CycleAction::Restore(target_index),
    _ => CycleAction::Focus(target_index),
  })
}

#[cfg(test)]
mod tests {
  use super::{
    cycle_action, CycleAction,
    WindowSlot::{Minimized, Tiling, Visible},
  };

  #[test]
  fn focuses_next_and_previous_visible_window() {
    let slots = [Tiling, Tiling, Visible];

    assert_eq!(
      cycle_action(&slots, Some(0), true),
      Some(CycleAction::Focus(1))
    );
    assert_eq!(
      cycle_action(&slots, Some(1), true),
      Some(CycleAction::Focus(2))
    );
    assert_eq!(
      cycle_action(&slots, Some(1), false),
      Some(CycleAction::Focus(0))
    );
  }

  #[test]
  fn wraps_around() {
    let slots = [Tiling, Tiling, Tiling];

    assert_eq!(
      cycle_action(&slots, Some(2), true),
      Some(CycleAction::Focus(0))
    );
    assert_eq!(
      cycle_action(&slots, Some(0), false),
      Some(CycleAction::Focus(2))
    );
  }

  #[test]
  fn swaps_tiling_origin_with_minimized_target() {
    let slots = [Minimized, Tiling, Minimized];

    assert_eq!(
      cycle_action(&slots, Some(1), true),
      Some(CycleAction::Swap(2))
    );
    assert_eq!(
      cycle_action(&slots, Some(1), false),
      Some(CycleAction::Swap(0))
    );
  }

  #[test]
  fn restores_without_swap_from_non_tiling_origin() {
    // From a floating window.
    assert_eq!(
      cycle_action(&[Visible, Minimized], Some(0), true),
      Some(CycleAction::Restore(1))
    );
    // From a minimized window (e.g. everything is minimized).
    assert_eq!(
      cycle_action(&[Minimized, Minimized], Some(0), true),
      Some(CycleAction::Restore(1))
    );
  }

  #[test]
  fn starts_from_either_end_when_workspace_is_focused() {
    let slots = [Minimized, Tiling];

    assert_eq!(
      cycle_action(&slots, None, true),
      Some(CycleAction::Restore(0))
    );
    assert_eq!(
      cycle_action(&slots, None, false),
      Some(CycleAction::Focus(1))
    );
  }

  #[test]
  fn not_applicable_without_other_window() {
    assert_eq!(cycle_action(&[Tiling], Some(0), true), None);
    assert_eq!(cycle_action(&[], None, true), None);
  }
}
