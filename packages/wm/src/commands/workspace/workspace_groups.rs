use anyhow::Context;
use tracing::info;

use super::{activate_workspace, focus_workspace};
use crate::{
  commands::{
    container::set_focused_descendant, window::move_window_to_workspace,
  },
  models::{Monitor, WindowContainer, Workspace, WorkspaceTarget},
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

/// Displays the other workspaces in the group of the given workspace on
/// their respective monitors, without changing focus.
///
/// Inactive partners are activated. Partners that are on the same monitor
/// as the given workspace are skipped. No-op if the workspace isn't part
/// of a workspace group.
pub fn display_workspace_partners(
  workspace: &Workspace,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let workspace_name = workspace.config().name;
  let monitor = workspace.monitor().context("Workspace has no monitor.")?;

  for partner_name in config.workspace_partners(&workspace_name) {
    let partner = match state.workspace_by_name(&partner_name) {
      Some(partner) => partner,
      None => {
        activate_workspace(Some(&partner_name), None, state, config)?;
        state
          .workspace_by_name(&partner_name)
          .context("Failed to activate partner workspace.")?
      }
    };

    let partner_monitor =
      partner.monitor().context("Partner workspace has no monitor.")?;

    if partner_monitor.id() == monitor.id() || partner.is_displayed() {
      continue;
    }

    info!("Displaying partner workspace: {partner}");

    let displayed_workspace = partner_monitor.displayed_workspace();

    // Bring the partner to the front of its monitor's focus order without
    // shifting focus away from the current monitor.
    let partner_container = partner.clone().into();
    set_focused_descendant(&partner_container, Some(&partner_container));

    if let Some(displayed_workspace) = displayed_workspace {
      state
        .pending_sync
        .queue_container_to_redraw(displayed_workspace);
    }

    state.pending_sync.queue_container_to_redraw(partner);
  }

  Ok(())
}

/// Ensures that the workspaces displayed on other monitors belong to the
/// same group as the focused workspace.
///
/// Only replaces workspaces that are part of a different group, so that
/// ungrouped workspaces can still be displayed alongside a group.
pub fn sync_workspace_partners(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  if config.value.workspace_groups.is_empty() {
    return Ok(());
  }

  let Some(focused_workspace) =
    state.focused_container().and_then(|focused| focused.workspace())
  else {
    return Ok(());
  };

  let Some(group) =
    config.workspace_group_for_workspace(&focused_workspace.config().name)
  else {
    return Ok(());
  };

  let has_mismatch = state.monitors().iter().any(|monitor| {
    monitor.displayed_workspace().is_some_and(|displayed| {
      let displayed_name = displayed.config().name;

      !group.workspaces.contains(&displayed_name)
        && config
          .workspace_group_for_workspace(&displayed_name)
          .is_some()
    })
  });

  if has_mismatch {
    display_workspace_partners(&focused_workspace, state, config)?;
  }

  Ok(())
}

/// Gets the name of the workspace in the given group that should be used
/// for the given monitor.
///
/// Prefers an active member on the monitor that is bound to it, then any
/// active member on the monitor, then a member bound to the monitor, and
/// lastly the first member of the group.
fn group_member_for_monitor(
  group_name: &str,
  monitor: Option<&Monitor>,
  state: &WmState,
  config: &UserConfig,
) -> anyhow::Result<String> {
  let group = config
    .workspace_group(group_name)
    .with_context(|| format!("Workspace group '{group_name}' not found."))?;

  let members = group
    .workspaces
    .iter()
    .map(|name| {
      let active_monitor_index = state
        .workspace_by_name(name)
        .and_then(|workspace| workspace.monitor())
        .map(|monitor| monitor.index());

      let bound_monitor_index = config
        .value
        .workspaces
        .iter()
        .find(|workspace| workspace.name == *name)
        .and_then(|workspace| workspace.bind_to_monitor)
        .map(|index| index as usize);

      GroupMember {
        name: name.clone(),
        active_monitor_index,
        bound_monitor_index,
      }
    })
    .collect::<Vec<_>>();

  select_group_member(&members, monitor.map(Monitor::index))
    .context("Workspace group has no workspaces.")
}

/// Focuses the workspace of the given group on the currently focused
/// monitor and displays the rest of the group on the other monitors.
pub fn focus_workspace_group(
  group_name: &str,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let focused_workspace = state
    .focused_container()
    .and_then(|focused| focused.workspace())
    .context("No workspace is currently focused.")?;

  let target_name = group_member_for_monitor(
    group_name,
    focused_workspace.monitor().as_ref(),
    state,
    config,
  )?;

  if focused_workspace.config().name == target_name {
    // Already on the group; make sure the partners are displayed.
    display_workspace_partners(&focused_workspace, state, config)?;
    return Ok(());
  }

  // `focus_workspace` displays the partners of the focused workspace.
  focus_workspace(WorkspaceTarget::Name(target_name), state, config)
}

/// Moves the given window to the workspace of the given group that is on
/// the window's current monitor.
pub fn move_window_to_workspace_group(
  window: WindowContainer,
  group_name: &str,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let target_name = group_member_for_monitor(
    group_name,
    window.monitor().as_ref(),
    state,
    config,
  )?;

  if window
    .workspace()
    .is_some_and(|workspace| workspace.config().name == target_name)
  {
    return Ok(());
  }

  move_window_to_workspace(
    window,
    WorkspaceTarget::Name(target_name),
    state,
    config,
  )
}

/// Placement info of a workspace in a group.
#[derive(Debug, Clone)]
struct GroupMember {
  name: String,
  active_monitor_index: Option<usize>,
  bound_monitor_index: Option<usize>,
}

/// Selects which member of a group to use for a monitor index. See
/// `group_member_for_monitor` for the priority order.
fn select_group_member(
  members: &[GroupMember],
  monitor_index: Option<usize>,
) -> Option<String> {
  let is_on_monitor =
    |member: &&GroupMember| member.active_monitor_index == monitor_index;
  let is_bound_to_monitor =
    |member: &&GroupMember| member.bound_monitor_index == monitor_index;

  monitor_index
    .and_then(|_| {
      members
        .iter()
        .find(|member| is_on_monitor(member) && is_bound_to_monitor(member))
        .or_else(|| members.iter().find(is_on_monitor))
        .or_else(|| {
          members
            .iter()
            .find(|member| {
              member.active_monitor_index.is_none()
                && is_bound_to_monitor(member)
            })
        })
    })
    .or_else(|| members.first())
    .map(|member| member.name.clone())
}

#[cfg(test)]
mod tests {
  use super::{select_group_member, GroupMember};

  fn member(
    name: &str,
    active: Option<usize>,
    bound: Option<usize>,
  ) -> GroupMember {
    GroupMember {
      name: name.to_string(),
      active_monitor_index: active,
      bound_monitor_index: bound,
    }
  }

  #[test]
  fn selects_member_on_monitor() {
    // WS1 on the top monitor (index 1), WS2 on the laptop (index 0).
    let members =
      [member("1", Some(1), Some(1)), member("2", Some(0), Some(0))];

    assert_eq!(select_group_member(&members, Some(0)).as_deref(), Some("2"));
    assert_eq!(select_group_member(&members, Some(1)).as_deref(), Some("1"));
  }

  #[test]
  fn prefers_bound_member_when_monitor_is_shared() {
    // Single monitor: both workspaces end up on the laptop.
    let members =
      [member("1", Some(0), Some(1)), member("2", Some(0), Some(0))];

    assert_eq!(select_group_member(&members, Some(0)).as_deref(), Some("2"));
  }

  #[test]
  fn selects_inactive_bound_member() {
    let members = [member("3", None, Some(1)), member("4", None, Some(0))];

    assert_eq!(select_group_member(&members, Some(0)).as_deref(), Some("4"));
  }

  #[test]
  fn falls_back_to_first_member() {
    let members = [member("3", Some(1), None), member("4", Some(1), None)];

    assert_eq!(select_group_member(&members, Some(0)).as_deref(), Some("3"));
    assert_eq!(select_group_member(&members, None).as_deref(), Some("3"));
    assert_eq!(select_group_member(&[], Some(0)), None);
  }
}
