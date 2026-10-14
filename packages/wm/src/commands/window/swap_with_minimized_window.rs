use anyhow::Context;
use tracing::info;
use wm_common::WindowState;
use wm_platform::Direction;

use super::update_window_state;
use crate::{
  commands::container::set_focused_descendant,
  models::{TilingWindow, WindowContainer},
  traits::{CommonGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

/// Swaps the given tiling window with one of the minimized windows in its
/// workspace, if it is the only visible tiling window there.
///
/// `Direction::Right` restores the next minimized window (in tree order)
/// and `Direction::Left` the previous one, wrapping around. The given
/// window gets minimized and the restored window gets focus.
///
/// Returns `false` (without changing anything) if the swap isn't
/// applicable, so that the caller can fall back to regular focus.
pub fn swap_with_minimized_window(
  window: &TilingWindow,
  direction: &Direction,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<bool> {
  if !matches!(direction, Direction::Left | Direction::Right) {
    return Ok(false);
  }

  let workspace = window.workspace().context("No workspace.")?;

  let windows = workspace
    .descendants()
    .filter_map(|descendant| descendant.as_window_container().ok())
    .collect::<Vec<_>>();

  let slots = windows
    .iter()
    .map(|window| match window.state() {
      WindowState::Tiling => WindowSlot::Tiling,
      WindowState::Minimized => WindowSlot::Minimized,
      _ => WindowSlot::Other,
    })
    .collect::<Vec<_>>();

  let Some(focused_index) =
    windows.iter().position(|other| other.id() == window.id())
  else {
    return Ok(false);
  };

  let Some(target_index) = minimized_swap_target(
    &slots,
    focused_index,
    *direction == Direction::Right,
  ) else {
    return Ok(false);
  };

  let target = windows[target_index].clone();
  info!("Swapping window with minimized window: {target}");

  // Convert the current window to minimized right away, rather than
  // waiting for the OS minimize event, so that the restored window
  // directly takes up the full workspace. The window is natively
  // minimized on the next platform sync.
  let window: WindowContainer = window.clone().into();
  window.update_native_properties(|properties| {
    properties.is_minimized = true;
  });
  update_window_state(window, WindowState::Minimized, state, config)?;

  let target_state = target.toggled_state(WindowState::Minimized, config);
  let target = update_window_state(target, target_state, state, config)?;

  set_focused_descendant(&target.into(), None);

  // Override stray OS focus events caused by the native minimize.
  state.unmanaged_or_minimized_timestamp = Some(std::time::Instant::now());
  state.pending_sync.queue_focus_change().queue_cursor_jump();

  Ok(true)
}

/// Kind of window in a workspace, for swap target selection.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WindowSlot {
  Tiling,
  Minimized,
  Other,
}

/// Gets the index of the minimized window to swap with, given the windows
/// of a workspace in tree order.
///
/// Returns `None` unless the focused window is the only tiling window and
/// at least one window is minimized.
fn minimized_swap_target(
  slots: &[WindowSlot],
  focused_index: usize,
  is_forward: bool,
) -> Option<usize> {
  let tiling_count = slots
    .iter()
    .filter(|slot| **slot == WindowSlot::Tiling)
    .count();

  if tiling_count != 1 || slots.get(focused_index) != Some(&WindowSlot::Tiling)
  {
    return None;
  }

  let minimized = slots
    .iter()
    .enumerate()
    .filter(|(_, slot)| **slot == WindowSlot::Minimized)
    .map(|(index, _)| index)
    .collect::<Vec<_>>();

  if is_forward {
    minimized
      .iter()
      .find(|index| **index > focused_index)
      .or_else(|| minimized.first())
      .copied()
  } else {
    minimized
      .iter()
      .rev()
      .find(|index| **index < focused_index)
      .or_else(|| minimized.last())
      .copied()
  }
}

#[cfg(test)]
mod tests {
  use super::{
    minimized_swap_target,
    WindowSlot::{Minimized, Other, Tiling},
  };

  #[test]
  fn swaps_with_next_and_previous_minimized() {
    let slots = [Minimized, Tiling, Minimized, Other, Minimized];

    assert_eq!(minimized_swap_target(&slots, 1, true), Some(2));
    assert_eq!(minimized_swap_target(&slots, 1, false), Some(0));
  }

  #[test]
  fn wraps_around() {
    let slots = [Minimized, Minimized, Tiling];

    assert_eq!(minimized_swap_target(&slots, 2, true), Some(0));
    assert_eq!(minimized_swap_target(&slots, 2, false), Some(1));

    let slots = [Tiling, Minimized, Minimized];
    assert_eq!(minimized_swap_target(&slots, 0, false), Some(2));
  }

  #[test]
  fn not_applicable() {
    // Several visible tiling windows.
    assert_eq!(
      minimized_swap_target(&[Tiling, Tiling, Minimized], 0, true),
      None
    );
    // Nothing minimized.
    assert_eq!(minimized_swap_target(&[Tiling, Other], 0, true), None);
    // Focused window isn't tiling.
    assert_eq!(
      minimized_swap_target(&[Other, Tiling, Minimized], 0, true),
      None
    );
  }
}
