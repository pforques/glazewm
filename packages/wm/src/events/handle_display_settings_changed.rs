use std::time::Instant;

use anyhow::Context;
use wm_common::try_warn;

use crate::{
  commands::{
    monitor::{
      add_monitor, move_bounded_workspaces_to_new_monitor, remove_monitor,
      sort_monitors, update_monitor,
    },
    workspace::merge_overflow_workspaces,
  },
  models::{Monitor, NativeMonitorProperties},
  traits::{CommonGetters, PositionGetters, WindowGetters},
  user_config::UserConfig,
  wm_state::WmState,
};

pub fn handle_display_settings_changed(
  state: &mut WmState,
  config: &UserConfig,
) -> anyhow::Result<()> {
  tracing::info!("Display settings changed.");

  // Milestones are logged so that a stall on this event can be located.
  let start = Instant::now();
  log_milestone(start, "started");

  let prev_monitor_count = state.monitors().len();
  let prev_groups_active = state.workspace_groups_active(config);

  // Ignore the event if retrieval of the displays or their properties
  // fails (can happen transiently during sleep/wake).
  let displays = try_warn!(state
    .dispatcher
    .sorted_displays()
    .map_err(anyhow::Error::from)
    .and_then(|displays| {
      displays
        .into_iter()
        .map(|display| {
          let properties = NativeMonitorProperties::try_from(&display)?;
          Ok((display, properties))
        })
        .try_collect::<Vec<_>>()
    }));

  log_milestone(start, &format!("queried {} displays", displays.len()));

  let mut pending_monitors = state.monitors();
  let mut unmatched_displays = Vec::new();

  // Match each display to an existing monitor and update it.
  for (display, properties) in displays {
    match find_matching_monitor(&pending_monitors, &properties) {
      Some((monitor, index)) => {
        update_monitor(monitor, &display, properties, state)?;
        pending_monitors.remove(index);
      }
      None => unmatched_displays.push((display, properties)),
    }
  }

  let mut new_monitors: Vec<Monitor> = Vec::new();

  // Pair unmatched displays with unmatched monitors, or add new ones.
  for (display, properties) in unmatched_displays {
    if pending_monitors.is_empty() {
      let monitor = add_monitor(display, properties, state)?;
      new_monitors.push(monitor);
    } else {
      let monitor = pending_monitors.remove(0);
      update_monitor(&monitor, &display, properties, state)?;
    }
  }

  // Remove monitors that no longer have a corresponding display and move
  // their workspaces to other monitors.
  //
  // Prevent removal of the last monitor (i.e. for when all monitors are
  // disconnected). This will cause the WM's monitors to temporarily
  // mismatch the OS monitor state, however, it'll be updated correctly
  // when a new monitor is connected again.
  for monitor in pending_monitors {
    if state.monitors().len() > 1 {
      remove_monitor(monitor, state, config)?;
    }
  }

  // Sort monitors by position.
  sort_monitors(&state.root_container)?;

  log_milestone(start, "updated monitors");

  // When the number of monitors changes, monitor indices can shift and
  // monitors can get paired with a different display, so move every bound
  // workspace back to its monitor (not only the ones of new monitors).
  let monitors_to_bind = if state.monitors().len() == prev_monitor_count {
    new_monitors
  } else {
    state.monitors()
  };

  for monitor in monitors_to_bind {
    move_bounded_workspaces_to_new_monitor(&monitor, state, config)?;
  }

  log_milestone(start, "rebound workspaces");

  // Never leave windows on an overflow workspace that shares a monitor
  // with other workspaces (e.g. after its monitor got disconnected).
  merge_overflow_workspaces(state, config)?;

  log_milestone(start, "merged overflow workspaces");

  for window in state.windows() {
    // Display setting changes can spread windows out sporadically, so mark
    // all windows as needing a DPI adjustment (just in case).
    window.set_has_pending_dpi_adjustment(true);

    // Need to update floating position of moved windows when a monitor is
    // disconnected or if the primary display is changed. The primary
    // display dictates the position of 0,0.
    let workspace = window.workspace().context("No workspace.")?;

    let should_recenter = if window.has_custom_floating_placement() {
      let workspace_rect = workspace.to_rect()?;

      // Keep the placement if it still intersects the workspace, since
      // `PlatformEvent::DisplaySettingsChanged` can be triggered by
      // non-monitor changes (e.g. unplugging a USB device).
      window
        .floating_placement()
        .intersection_area(&workspace_rect)
        == 0
    } else {
      true
    };

    if should_recenter {
      window.set_floating_placement(
        window
          .floating_placement()
          .translate_to_center(&workspace.to_rect()?),
      );
    }
  }

  // Redraw full container tree.
  state
    .pending_sync
    .queue_container_to_redraw(state.root_container.clone());

  let monitor_count = state.monitors().len();

  if monitor_count != prev_monitor_count {
    state.mark_monitor_count_changed();

    // The OS focused window may have been moved to a hidden workspace, so
    // make sure focus is synced to the WM's focused container.
    state.pending_sync.queue_focus_change();

    let groups_active = state.workspace_groups_active(config);

    tracing::info!(
      target: crate::diagnostics::LOG_TARGET,
      "Monitor count changed: {prev_monitor_count} -> {monitor_count} \
       (workspace groups active: {prev_groups_active} -> {groups_active})."
    );
  }

  // Anything slow after this point happens in the platform sync (e.g.
  // repositioning windows).
  log_milestone(start, "handled, syncing windows");

  Ok(())
}

/// Logs a milestone of the display settings change handling, with the
/// time elapsed since `start`.
fn log_milestone(start: Instant, milestone: &str) {
  tracing::info!(
    target: crate::diagnostics::LOG_TARGET,
    "Display settings changed: {milestone} ({:?}).",
    start.elapsed()
  );
}

/// Finds the monitor matching the given display properties.
///
/// Returns the monitor and its index within the list of monitors.
fn find_matching_monitor<'a>(
  monitors: &'a [Monitor],
  properties: &NativeMonitorProperties,
) -> Option<(&'a Monitor, usize)> {
  monitors.iter().enumerate().find_map(|(index, monitor)| {
    let existing = monitor.native_properties();

    let is_match = {
      #[cfg(target_os = "macos")]
      {
        existing.device_uuid == properties.device_uuid
      }

      // On Windows, match the monitor by:
      // 1. Its handle
      // 2. Its device path
      // 3. Its hardware ID (if unique)
      //
      // Monitor handles and device paths are unique, but can change over
      // time. The hardware ID is not guaranteed to be unique, so we
      // match against that last.
      #[cfg(target_os = "windows")]
      {
        existing.handle == properties.handle
          || existing.device_path.as_deref().is_some_and(|device_path| {
            properties.device_path.as_deref() == Some(device_path)
          })
          || existing.hardware_id.as_deref().is_some_and(|hardware_id| {
            let is_unique = monitors
              .iter()
              .filter(|other_monitor| {
                other_monitor.native_properties().hardware_id.as_deref()
                  == Some(hardware_id)
              })
              .count()
              == 1;

            is_unique
              && properties.hardware_id.as_deref() == Some(hardware_id)
          })
      }
    };

    is_match.then_some((monitor, index))
  })
}
