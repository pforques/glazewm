use wm_common::WmEvent;

use super::flatten_split_container;
use crate::{
  traits::{CommonGetters, TilingDirectionGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

/// Applies `window_behavior.fixed_tiling_direction` to all active
/// workspaces.
///
/// Sets each workspace to the fixed tiling direction and flattens all of
/// its split containers, so that its windows are laid out in a single
/// row/column. No-op if no fixed tiling direction is configured.
pub fn apply_fixed_tiling_direction(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let Some(fixed_direction) = config.fixed_tiling_direction() else {
    return Ok(());
  };

  for workspace in state.workspaces() {
    let mut has_changed = false;

    // Flatten split containers until none are left. Nested split
    // containers are hoisted into their parent on each pass.
    while let Some(split_container) = workspace
      .descendants()
      .find_map(|descendant| descendant.as_split().cloned())
    {
      flatten_split_container(split_container)?;
      has_changed = true;
    }

    if workspace.tiling_direction() != fixed_direction {
      workspace.set_tiling_direction(fixed_direction.clone());
      has_changed = true;

      state.emit_event(WmEvent::TilingDirectionChanged {
        direction_container: workspace.to_dto()?,
        new_tiling_direction: fixed_direction.clone(),
      });
    }

    if has_changed {
      state
        .pending_sync
        .queue_containers_to_redraw(workspace.tiling_children());
    }
  }

  Ok(())
}
