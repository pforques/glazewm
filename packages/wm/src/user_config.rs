use std::{collections::HashMap, env, fs, path::PathBuf};

use anyhow::{Context, Result};
use wm_common::{
  InvokeCommand, KeybindingConfig, MatchType, ParsedConfig,
  TilingDirection, WindowMatchConfig, WindowRuleConfig, WindowRuleEvent,
  WorkspaceConfig, WorkspaceGroupConfig,
};

use crate::{
  models::{Monitor, WindowContainer, Workspace},
  traits::{CommonGetters, WindowGetters},
};

/// Resource string for the sample config file.
const SAMPLE_CONFIG: &str =
  include_str!("../../../resources/assets/sample-config.yaml");

#[derive(Debug)]
pub struct UserConfig {
  /// Path to the user config file.
  pub path: PathBuf,

  /// Parsed user config value.
  pub value: ParsedConfig,

  /// Unparsed user config string.
  pub value_str: String,

  /// Hashmap of window rule event types (e.g. `WindowRuleEvent::Manage`)
  /// and the corresponding window rules of that type.
  window_rules_by_event: HashMap<WindowRuleEvent, Vec<WindowRuleConfig>>,
}

impl UserConfig {
  /// Creates an instance of `UserConfig`. Reads and validates the user
  /// config from the given path.
  ///
  /// Creates a new config file from sample if it doesn't exist.
  pub fn new(config_path: Option<PathBuf>) -> anyhow::Result<Self> {
    let default_config_path = home::home_dir()
      .context("Unable to get home directory.")?
      .join(".glzr/glazewm/config.yaml");

    let config_path = config_path
      .or_else(|| env::var("GLAZEWM_CONFIG_PATH").ok().map(PathBuf::from))
      .unwrap_or(default_config_path);

    let (config_value, config_str) = Self::read(&config_path)?;

    let window_rules_by_event = Self::window_rules_by_event(&config_value);

    Ok(Self {
      path: config_path,
      value: config_value,
      value_str: config_str,
      window_rules_by_event,
    })
  }

  /// Reads and validates the user config from the given path.
  ///
  /// Creates a new config file from sample if it doesn't exist.
  fn read(
    config_path: &PathBuf,
  ) -> anyhow::Result<(ParsedConfig, String)> {
    if !config_path.exists() {
      Self::create_sample(config_path)?;
    }

    let config_str = fs::read_to_string(config_path)
      .context("Unable to read config file.")?;

    // TODO: Improve error formatting of serde_yaml errors. Something
    // similar to https://github.com/AlexanderThaller/format_serde_error
    let config_value = serde_yaml::from_str(&config_str)?;
    validate_workspace_groups(&config_value)?;

    Ok((config_value, config_str))
  }

  /// Initializes a new config file from the sample config resource.
  fn create_sample(config_path: &PathBuf) -> Result<()> {
    let parent_dir =
      config_path.parent().context("Invalid config path.")?;

    fs::create_dir_all(parent_dir).with_context(|| {
      format!("Unable to create directory {}.", &config_path.display())
    })?;

    fs::write(config_path, SAMPLE_CONFIG).with_context(|| {
      format!("Unable to write to {}.", config_path.display())
    })?;

    Ok(())
  }

  pub fn reload(&mut self) -> anyhow::Result<()> {
    let (config_value, config_str) = Self::read(&self.path)?;

    self.window_rules_by_event =
      Self::window_rules_by_event(&config_value);
    self.value = config_value;
    self.value_str = config_str;

    Ok(())
  }

  fn default_window_rules(
    config_value: &ParsedConfig,
  ) -> Vec<WindowRuleConfig> {
    let mut window_rules = Vec::new();

    let floating_defaults =
      &config_value.window_behavior.state_defaults.floating;

    // Default float rules.
    window_rules.push(WindowRuleConfig {
      commands: vec![InvokeCommand::SetFloating {
        centered: Some(floating_defaults.centered),
        shown_on_top: Some(floating_defaults.shown_on_top),
        x_pos: None,
        y_pos: None,
        width: None,
        height: None,
      }],
      match_window: vec![
        WindowMatchConfig {
          window_class: Some(MatchType::Equals { equals:
          // W10/W11 system dialog shown when moving and deleting files.
          "OperationStatusWindow".to_string(),
        }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_class: Some(MatchType::Equals { equals:
          // W10/W11 system dialogs (e.g. File Explorer save/open dialog).
          "#32770".to_string(),
        }),
          ..WindowMatchConfig::default()
        },
      ],
      on: vec![WindowRuleEvent::Manage],
      run_once: true,
    });

    // Default ignore rules.
    window_rules.push(WindowRuleConfig {
      commands: vec![InvokeCommand::Ignore],
      match_window: vec![
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            equals: "SearchApp".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            equals: "SearchHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            equals: "ShellExperienceHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            // W10/11 start menu.
            equals: "StartMenuExperienceHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            // W10/11 screen snipping tool.
            equals: "ScreenClippingHost".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
        WindowMatchConfig {
          window_process: Some(MatchType::Equals {
            // W11 lock screen.
            equals: "LockApp".to_string(),
          }),
          ..WindowMatchConfig::default()
        },
      ],
      on: vec![WindowRuleEvent::Manage],
      run_once: true,
    });

    window_rules
  }

  fn window_rules_by_event(
    config_value: &ParsedConfig,
  ) -> HashMap<WindowRuleEvent, Vec<WindowRuleConfig>> {
    let mut window_rules_by_event = HashMap::new();

    // Combine user-defined window rules with the default ones.
    let default_window_rules = Self::default_window_rules(config_value);
    let all_window_rules = config_value
      .window_rules
      .iter()
      .chain(default_window_rules.iter());

    for window_rule in all_window_rules {
      for event_type in &window_rule.on {
        window_rules_by_event
          .entry(event_type.clone())
          .or_insert_with(Vec::new)
          .push(window_rule.clone());
      }
    }

    window_rules_by_event
  }

  /// Window rules that should be applied to the window when the given
  /// event occurs.
  pub fn pending_window_rules(
    &self,
    window: &WindowContainer,
    event: &WindowRuleEvent,
  ) -> Vec<WindowRuleConfig> {
    let window_title = window.native_properties().title;
    #[cfg(target_os = "windows")]
    let window_class = window.native_properties().class_name;
    let window_process = window.native_properties().process_name;

    let pending_window_rules = self
      .window_rules_by_event
      .get(event)
      .unwrap_or(&Vec::new())
      .iter()
      .filter(|rule| {
        // Skip if window has already ran the rule.
        if window.done_window_rules().contains(rule) {
          return false;
        }

        // Check if the window matches the rule.
        rule.match_window.iter().any(|match_config| {
          let is_process_match = match_config
            .window_process
            .as_ref()
            .is_none_or(|match_type| {
              // TODO: Temp fix for matching Zebar on both platforms with
              // the same process name. Consider using lowercase for every
              // `equals` match type.
              if window_process == "Zebar" {
                match_type.is_match("Zebar")
                  || match_type.is_match("zebar")
              } else {
                match_type.is_match(&window_process)
              }
            });

          let is_class_match = {
            #[cfg(target_os = "windows")]
            {
              match_config.window_class.as_ref().is_none_or(|match_type| {
                match_type.is_match(&window_class)
              })
            }
            #[cfg(not(target_os = "windows"))]
            {
              match_config.window_class.is_none()
            }
          };

          let is_title_match = match_config
            .window_title
            .as_ref()
            .is_none_or(|match_type| match_type.is_match(&window_title));

          is_process_match && is_class_match && is_title_match
        })
      })
      .cloned()
      .collect::<Vec<_>>();

    pending_window_rules
  }

  pub fn inactive_workspace_configs(
    &self,
    active_workspaces: &[Workspace],
  ) -> Vec<&WorkspaceConfig> {
    self
      .value
      .workspaces
      .iter()
      .filter(|config| {
        !active_workspaces
          .iter()
          .any(|workspace| workspace.config().name == config.name)
      })
      .collect()
  }

  pub fn workspace_config_for_monitor(
    &self,
    monitor: &Monitor,
    active_workspaces: &[Workspace],
  ) -> Option<&WorkspaceConfig> {
    let inactive_configs =
      self.inactive_workspace_configs(active_workspaces);

    inactive_configs.into_iter().find(|&config| {
      config
        .bind_to_monitor
        .as_ref()
        .is_some_and(|monitor_index| {
          monitor.index() == *monitor_index as usize
        })
    })
  }

  /// Gets the first inactive workspace config, prioritizing configs that
  /// don't have a monitor binding.
  pub fn next_inactive_workspace_config(
    &self,
    active_workspaces: &[Workspace],
  ) -> Option<&WorkspaceConfig> {
    let inactive_configs =
      self.inactive_workspace_configs(active_workspaces);

    inactive_configs
      .iter()
      .find(|config| config.bind_to_monitor.is_none())
      .or(inactive_configs.first())
      .copied()
  }

  pub fn workspace_config_index(
    &self,
    workspace_name: &str,
  ) -> Option<usize> {
    self
      .value
      .workspaces
      .iter()
      .position(|config| config.name == workspace_name)
  }

  pub fn sort_workspaces(&self, workspaces: &mut [Workspace]) {
    workspaces.sort_by_key(|workspace| {
      self.workspace_config_index(&workspace.config().name)
    });
  }

  /// Gets the workspace group config with the given name.
  pub fn workspace_group(
    &self,
    group_name: &str,
  ) -> Option<&WorkspaceGroupConfig> {
    self
      .value
      .workspace_groups
      .iter()
      .find(|group| group.name == group_name)
  }

  /// Gets the workspace group that contains the given workspace.
  pub fn workspace_group_for_workspace(
    &self,
    workspace_name: &str,
  ) -> Option<&WorkspaceGroupConfig> {
    workspace_group_for_workspace(&self.value, workspace_name)
  }

  /// Gets the names of the other workspaces in the same group as the
  /// given workspace. Empty if the workspace isn't grouped.
  pub fn workspace_partners(&self, workspace_name: &str) -> Vec<String> {
    self
      .workspace_group_for_workspace(workspace_name)
      .map(|group| {
        group
          .workspaces
          .iter()
          .filter(|name| *name != workspace_name)
          .cloned()
          .collect()
      })
      .unwrap_or_default()
  }

  /// Whether workspace groups are in effect with the given number of
  /// connected monitors.
  ///
  /// Groups display their workspaces on separate monitors, so they only
  /// apply when groups are configured and there's more than one monitor.
  /// With a single monitor, every workspace behaves independently.
  pub fn workspace_groups_active(&self, monitor_count: usize) -> bool {
    workspace_groups_active(&self.value, monitor_count)
  }

  /// Tiling direction that all workspaces are forced to use, if any.
  pub fn fixed_tiling_direction(&self) -> Option<TilingDirection> {
    self.value.window_behavior.fixed_tiling_direction.clone()
  }

  /// Keybinding configs that should be active for the current binding
  /// mode, pause state and workspace groups state.
  ///
  /// When paused, only the configs with `InvokeCommand::WmTogglePause` are
  /// returned so that unpausing remains possible. Configs with a `when`
  /// condition are only returned if the condition is met.
  pub fn active_keybinding_configs(
    &self,
    binding_modes: &[wm_common::BindingModeConfig],
    is_paused: bool,
    workspace_groups_active: bool,
  ) -> impl Iterator<Item = KeybindingConfig> {
    let source_configs = if let Some(first_mode) = binding_modes.first() {
      &first_mode.keybindings
    } else {
      &self.value.keybindings
    }
    .clone();

    source_configs.into_iter().filter(move |kb| {
      let is_condition_met = kb
        .when
        .is_none_or(|condition| condition.is_met(workspace_groups_active));

      is_condition_met
        && (!is_paused
          || kb
            .commands
            .contains(&wm_common::InvokeCommand::WmTogglePause))
    })
  }
}

/// Whether workspace groups are in effect with the given number of
/// connected monitors. See `UserConfig::workspace_groups_active`.
fn workspace_groups_active(
  config_value: &ParsedConfig,
  monitor_count: usize,
) -> bool {
  !config_value.workspace_groups.is_empty() && monitor_count > 1
}

/// Gets the workspace group that contains the given workspace.
fn workspace_group_for_workspace<'a>(
  config_value: &'a ParsedConfig,
  workspace_name: &str,
) -> Option<&'a WorkspaceGroupConfig> {
  config_value
    .workspace_groups
    .iter()
    .find(|group| group.workspaces.iter().any(|name| name == workspace_name))
}

/// Validates the `workspace_groups` section of the config.
///
/// Group names must be unique, groups must reference existing workspaces,
/// and a workspace can belong to at most one group.
fn validate_workspace_groups(
  config_value: &ParsedConfig,
) -> anyhow::Result<()> {
  let mut seen_groups = Vec::<&str>::new();
  let mut seen_workspaces = HashMap::<&str, &str>::new();

  for group in &config_value.workspace_groups {
    if seen_groups.contains(&group.name.as_str()) {
      anyhow::bail!("Duplicate workspace group name '{}'.", group.name);
    }

    seen_groups.push(&group.name);

    if group.workspaces.is_empty() {
      anyhow::bail!("Workspace group '{}' has no workspaces.", group.name);
    }

    for workspace_name in &group.workspaces {
      if !config_value
        .workspaces
        .iter()
        .any(|workspace| workspace.name == *workspace_name)
      {
        anyhow::bail!(
          "Workspace group '{}' references unknown workspace '{}'.",
          group.name,
          workspace_name
        );
      }

      if let Some(other_group) =
        seen_workspaces.insert(workspace_name, &group.name)
      {
        anyhow::bail!(
          "Workspace '{}' is in multiple workspace groups ('{}' and '{}').",
          workspace_name,
          other_group,
          group.name
        );
      }
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use wm_common::{
    HiddenWindowFocusBehavior, KeybindingCondition, ParsedConfig,
    TilingDirection,
  };

  use super::{
    validate_workspace_groups, workspace_group_for_workspace,
    workspace_groups_active, SAMPLE_CONFIG,
  };

  const GROUPED_CONFIG: &str = r#"
general:
  hidden_window_focus: "move_to_focused_workspace"
window_behavior:
  fixed_tiling_direction: "horizontal"
  cycle_focus: true
workspaces:
  - name: "1"
    bind_to_monitor: 1
  - name: "2"
    bind_to_monitor: 0
  - name: "3"
    bind_to_monitor: 1
  - name: "4"
    bind_to_monitor: 0
workspace_groups:
  - name: "main"
    display_name: "Main"
    workspaces: ["1", "2"]
  - name: "dev"
    workspaces: ["3", "4"]
"#;

  fn parse(yaml: &str) -> ParsedConfig {
    serde_yaml::from_str(yaml).expect("Config should parse.")
  }

  #[test]
  fn sample_config_is_valid() {
    let config = parse(SAMPLE_CONFIG);

    assert!(validate_workspace_groups(&config).is_ok());
  }

  #[test]
  fn parses_fork_options() {
    let config = parse(GROUPED_CONFIG);

    assert_eq!(
      config.general.hidden_window_focus,
      HiddenWindowFocusBehavior::MoveToFocusedWorkspace
    );
    assert_eq!(
      config.window_behavior.fixed_tiling_direction,
      Some(TilingDirection::Horizontal)
    );
    assert!(config.window_behavior.cycle_focus);
    assert_eq!(config.workspace_groups.len(), 2);
    assert_eq!(
      config.workspace_groups[0].display_name.as_deref(),
      Some("Main")
    );
    assert!(validate_workspace_groups(&config).is_ok());
  }

  #[test]
  fn fork_options_default_to_upstream_behavior() {
    let config = parse("workspaces: [{ name: '1' }]");

    assert_eq!(
      config.general.hidden_window_focus,
      HiddenWindowFocusBehavior::SwitchWorkspace
    );
    assert_eq!(config.window_behavior.fixed_tiling_direction, None);
    assert!(!config.window_behavior.cycle_focus);
    assert!(config.workspace_groups.is_empty());
  }

  #[test]
  fn finds_group_of_workspace() {
    let config = parse(GROUPED_CONFIG);

    let group = workspace_group_for_workspace(&config, "4");
    assert_eq!(group.map(|group| group.name.as_str()), Some("dev"));
    assert!(workspace_group_for_workspace(&config, "9").is_none());
  }

  #[test]
  fn rejects_invalid_groups() {
    let unknown_workspace = parse(
      "workspaces: [{ name: '1' }]
workspace_groups: [{ name: 'a', workspaces: ['1', '2'] }]",
    );
    assert!(validate_workspace_groups(&unknown_workspace).is_err());

    let shared_workspace = parse(
      "workspaces: [{ name: '1' }, { name: '2' }]
workspace_groups:
  - { name: 'a', workspaces: ['1', '2'] }
  - { name: 'b', workspaces: ['2'] }",
    );
    assert!(validate_workspace_groups(&shared_workspace).is_err());

    let duplicate_name = parse(
      "workspaces: [{ name: '1' }, { name: '2' }]
workspace_groups:
  - { name: 'a', workspaces: ['1'] }
  - { name: 'a', workspaces: ['2'] }",
    );
    assert!(validate_workspace_groups(&duplicate_name).is_err());
  }

  #[test]
  fn workspace_groups_only_active_with_multiple_monitors() {
    let grouped = parse(GROUPED_CONFIG);
    assert!(!workspace_groups_active(&grouped, 1));
    assert!(workspace_groups_active(&grouped, 2));

    let ungrouped = parse("workspaces: [{ name: '1' }]");
    assert!(!workspace_groups_active(&ungrouped, 2));
  }

  #[test]
  fn parses_keybinding_conditions() {
    let config = parse(
      "keybindings:
  - { commands: ['focus --workspace-group main'], bindings: ['alt+1'], when: workspace_groups_active }
  - { commands: ['focus --workspace 1'], bindings: ['alt+1'], when: workspace_groups_inactive }
  - { commands: ['wm-exit'], bindings: ['alt+q'] }",
    );

    let conditions = config
      .keybindings
      .iter()
      .map(|kb| kb.when)
      .collect::<Vec<_>>();

    assert_eq!(
      conditions,
      vec![
        Some(KeybindingCondition::WorkspaceGroupsActive),
        Some(KeybindingCondition::WorkspaceGroupsInactive),
        None
      ]
    );
  }

  #[test]
  fn keybinding_conditions_match_groups_state() {
    assert!(KeybindingCondition::WorkspaceGroupsActive.is_met(true));
    assert!(!KeybindingCondition::WorkspaceGroupsActive.is_met(false));
    assert!(KeybindingCondition::WorkspaceGroupsInactive.is_met(false));
    assert!(!KeybindingCondition::WorkspaceGroupsInactive.is_met(true));
  }
}
