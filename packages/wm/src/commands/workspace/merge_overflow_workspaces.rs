use anyhow::Context;
use tracing::info;
use wm_common::WorkspaceGroupConfig;

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

/// Merges overflow workspaces into a reachable workspace on the same
/// monitor.
///
/// When workspace groups are configured, workspaces outside of any group
/// (e.g. the ones activated for extra monitors) aren't reachable through
/// the group keybindings, and neither are the workspaces of
/// `multi_monitor_only` groups while groups are inactive (single monitor).
/// If such a workspace ends up sharing a monitor with another workspace
/// (e.g. after its monitor is disconnected), its windows are moved to the
/// monitor's most recently displayed reachable workspace and the empty
/// workspace is deactivated, so that no window gets stuck on a hidden
/// workspace. No-op if no workspace groups are configured.
pub fn merge_overflow_workspaces(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  if config.value.workspace_groups.is_empty() {
    return Ok(());
  }

  let groups_active = state.workspace_groups_active(config);

  for monitor in state.monitors() {
    let workspaces = monitor
      .child_focus_order()
      .filter_map(|child| child.as_workspace().cloned())
      .collect::<Vec<_>>();

    let entries = workspaces
      .iter()
      .map(|workspace| {
        let name = workspace.config().name;
        let is_reachable = is_reachable_workspace(
          config.workspace_group_for_workspace(&name),
          groups_active,
        );

        (name, is_reachable)
      })
      .collect::<Vec<_>>();

    let Some(plan) = overflow_merge_plan(&entries) else {
      continue;
    };

    let target = state
      .workspace_by_name(&plan.target)
      .context("Merge target workspace not found.")?;

    let was_source_displayed =
      monitor.displayed_workspace().is_some_and(|displayed| {
        plan.sources.contains(&displayed.config().name)
      });

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

    state.pending_sync.queue_container_to_redraw(target.clone());
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

/// Whether a workspace in the given group (`None` if ungrouped) can be
/// reached through the group keybindings.
///
/// Ungrouped workspaces never are, and the workspaces of a
/// `multi_monitor_only` group only are while groups are active.
fn is_reachable_workspace(
  group: Option<&WorkspaceGroupConfig>,
  groups_active: bool,
) -> bool {
  group.is_some_and(|group| groups_active || !group.multi_monitor_only)
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
/// each with whether it is reachable (see `is_reachable_workspace`). The
/// target is the most recently displayed reachable workspace, or the
/// displayed workspace if none is reachable. Every other unreachable
/// workspace is merged into it.
///
/// Returns `None` if there is nothing to merge.
fn overflow_merge_plan(
  workspaces: &[(String, bool)],
) -> Option<OverflowMergePlan> {
  let target = workspaces
    .iter()
    .find(|(_, is_reachable)| *is_reachable)
    .or_else(|| workspaces.first())
    .map(|(name, _)| name.clone())?;

  let sources = workspaces
    .iter()
    .filter(|(name, is_reachable)| !is_reachable && *name != target)
    .map(|(name, _)| name.clone())
    .collect::<Vec<_>>();

  (!sources.is_empty()).then_some(OverflowMergePlan { target, sources })
}

#[cfg(test)]
mod tests {
  use wm_common::WorkspaceGroupConfig;

  use super::{
    is_reachable_workspace, overflow_merge_plan, OverflowMergePlan,
  };

  fn entries(workspaces: &[(&str, bool)]) -> Vec<(String, bool)> {
    workspaces
      .iter()
      .map(|(name, is_reachable)| ((*name).to_string(), *is_reachable))
      .collect()
  }

  fn group(
    workspaces: &[&str],
    multi_monitor_only: bool,
  ) -> WorkspaceGroupConfig {
    WorkspaceGroupConfig {
      name: "group".to_string(),
      display_name: None,
      workspaces: workspaces
        .iter()
        .map(|name| (*name).to_string())
        .collect(),
      multi_monitor_only,
    }
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

  #[test]
  fn multi_monitor_only_group_is_reachable_only_with_groups_active() {
    let main = group(&["1", "2"], false);
    let perso = group(&["5", "6"], true);

    assert!(is_reachable_workspace(Some(&main), true));
    assert!(is_reachable_workspace(Some(&main), false));
    assert!(is_reachable_workspace(Some(&perso), true));
    assert!(!is_reachable_workspace(Some(&perso), false));
    assert!(!is_reachable_workspace(None, true));
    assert!(!is_reachable_workspace(None, false));
  }

  #[test]
  fn merges_multi_monitor_only_desktop_on_single_monitor() {
    // Unplugged while on the third desktop: the laptop displays WS6 and
    // the Samsung's WS5 got moved to it; WS4 was displayed before.
    let workspaces = entries(&[
      ("6", false),
      ("4", true),
      ("2", true),
      ("5", false),
      ("1", true),
      ("3", true),
    ]);

    assert_eq!(overflow_merge_plan(&workspaces), plan("4", &["6", "5"]));
  }
}
