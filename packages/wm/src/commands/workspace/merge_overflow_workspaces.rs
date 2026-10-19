use anyhow::Context;
use tracing::info;

use super::deactivate_workspace;
use crate::{
  commands::{
    container::set_focused_descendant, window::move_window_to_workspace,
  },
  models::{Container, Workspace, WorkspaceTarget},
  traits::CommonGetters,
  user_config::UserConfig,
  wm_state::WmState,
};

/// Merges overflow workspaces into the workspace group member on the same
/// monitor.
///
/// When workspace groups are configured, workspaces outside of any group
/// (e.g. the ones activated for extra monitors) aren't reachable through
/// the group keybindings. If such a workspace ends up sharing a monitor
/// with another workspace (e.g. after its monitor is disconnected), its
/// windows are moved to the monitor's group workspace and the empty
/// workspace is deactivated, so that no window gets stuck on a hidden
/// workspace. No-op if no workspace groups are configured.
pub fn merge_overflow_workspaces(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  if config.value.workspace_groups.is_empty() {
    return Ok(());
  }

  for monitor in state.monitors() {
    let workspaces = monitor
      .child_focus_order()
      .filter_map(|child| child.as_workspace().cloned())
      .collect::<Vec<_>>();

    let entries = workspaces
      .iter()
      .map(|workspace| {
        let name = workspace.config().name;
        let is_grouped =
          config.workspace_group_for_workspace(&name).is_some();

        (name, is_grouped)
      })
      .collect::<Vec<_>>();

    let Some(plan) = overflow_merge_plan(&entries) else {
      continue;
    };

    let target = state
      .workspace_by_name(&plan.target)
      .context("Merge target workspace not found.")?;

    let was_source_displayed = monitor
      .displayed_workspace()
      .is_some_and(|displayed| plan.sources.contains(&displayed.config().name));

    let focused_container = state.focused_container();
    let mut is_focus_in_sources = false;

    for source_name in &plan.sources {
      let source = state
        .workspace_by_name(source_name)
        .context("Overflow workspace not found.")?;

      is_focus_in_sources |= focused_container
        .as_ref()
        .and_then(CommonGetters::workspace)
        .is_some_and(|workspace| workspace.id() == source.id());

      merge_workspace_into(&source, &target, state, config)?;
    }

    if is_focus_in_sources {
      // Restore focus to the previously focused window (now in the target
      // workspace), or to the target workspace itself.
      let focus_target = focused_container
        .filter(|focused| {
          focused
            .workspace()
            .is_some_and(|workspace| workspace.id() == target.id())
        })
        .unwrap_or_else(|| target.clone().into());

      set_focused_descendant(&focus_target, None);
      state.pending_sync.queue_focus_change();
    } else if was_source_displayed {
      // Display the target instead, without changing the focused monitor.
      let target_container: Container = target.clone().into();
      set_focused_descendant(&target_container, Some(&target_container));
    }

    state
      .pending_sync
      .queue_container_to_redraw(target.clone());
  }

  Ok(())
}

/// Moves all windows of the source workspace to the target workspace and
/// deactivates the source workspace.
fn merge_workspace_into(
  source: &Workspace,
  target: &Workspace,
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  let windows = source
    .descendants()
    .filter_map(|descendant| descendant.as_window_container().ok())
    .collect::<Vec<_>>();

  info!(
    target: crate::diagnostics::LOG_TARGET,
    "Merging overflow workspace '{}' ({} windows) into workspace '{}'.",
    source.config().name,
    windows.len(),
    target.config().name
  );

  for window in windows {
    move_window_to_workspace(
      window,
      WorkspaceTarget::Name(target.config().name),
      state,
      config,
    )?;
  }

  deactivate_workspace(source.clone(), state)
}

/// Which workspaces of a monitor to merge, and into which one.
#[derive(Debug, PartialEq, Eq)]
struct OverflowMergePlan {
  target: String,
  sources: Vec<String>,
}

/// Decides which overflow workspaces of a monitor to merge.
///
/// Takes the workspaces of a monitor in focus order (displayed first),
/// each with whether it belongs to a workspace group. The target is the
/// most recently displayed grouped workspace, or the displayed workspace
/// if none is grouped. Every other ungrouped workspace is merged into it.
///
/// Returns `None` if there is nothing to merge.
fn overflow_merge_plan(
  workspaces: &[(String, bool)],
) -> Option<OverflowMergePlan> {
  let target = workspaces
    .iter()
    .find(|(_, is_grouped)| *is_grouped)
    .or_else(|| workspaces.first())
    .map(|(name, _)| name.clone())?;

  let sources = workspaces
    .iter()
    .filter(|(name, is_grouped)| !is_grouped && *name != target)
    .map(|(name, _)| name.clone())
    .collect::<Vec<_>>();

  (!sources.is_empty()).then_some(OverflowMergePlan { target, sources })
}

#[cfg(test)]
mod tests {
  use super::{overflow_merge_plan, OverflowMergePlan};

  fn entries(workspaces: &[(&str, bool)]) -> Vec<(String, bool)> {
    workspaces
      .iter()
      .map(|(name, is_grouped)| ((*name).to_string(), *is_grouped))
      .collect()
  }

  fn plan(target: &str, sources: &[&str]) -> Option<OverflowMergePlan> {
    Some(OverflowMergePlan {
      target: target.to_string(),
      sources: sources.iter().map(|name| (*name).to_string()).collect(),
    })
  }

  #[test]
  fn keeps_overflow_workspace_alone_on_its_monitor() {
    // Third monitor with only WS6.
    assert_eq!(overflow_merge_plan(&entries(&[("6", false)])), None);
    assert_eq!(overflow_merge_plan(&[]), None);
  }

  #[test]
  fn keeps_grouped_workspaces() {
    // Laptop alone with WS1..4.
    let workspaces =
      entries(&[("4", true), ("2", true), ("1", true), ("3", true)]);

    assert_eq!(overflow_merge_plan(&workspaces), None);
  }

  #[test]
  fn merges_hidden_overflow_workspace_on_removed_monitor() {
    // Third monitor removed: its WS6 is moved to the laptop, hidden.
    let workspaces = entries(&[("4", true), ("2", true), ("6", false)]);

    assert_eq!(overflow_merge_plan(&workspaces), plan("4", &["6"]));
  }

  #[test]
  fn merges_displayed_overflow_workspace_into_recent_group_member() {
    // WS6 got displayed on the laptop; WS2 was displayed before it.
    let workspaces = entries(&[("6", false), ("2", true), ("4", true)]);

    assert_eq!(overflow_merge_plan(&workspaces), plan("2", &["6"]));
  }

  #[test]
  fn merges_overflow_workspaces_together_without_group_member() {
    // Third monitor with a displayed WS6 and a hidden WS5.
    let workspaces = entries(&[("6", false), ("5", false)]);

    assert_eq!(overflow_merge_plan(&workspaces), plan("6", &["5"]));
  }
}
