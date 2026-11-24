use crate::plugins::{HookView, Scope};

/// What a hook runs, as the list shows it: its event, the tools it matches and its command.
/// Input: the hook. Output: one line for the list.
pub fn hook_runs(hook: &HookView) -> String {
    match &hook.matcher {
        Some(matcher) => format!("{} {matcher}: {}", hook.event, hook.command),
        None => format!("{}: {}", hook.event, hook.command),
    }
}

/// One plugin's hooks as the Hooks list groups them: the plugin, where it applies, how many
/// distinct events it runs on, the skills or servers it runs with and the indexes of its
/// hooks in the engine's list, in that list's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookGroup {
    pub plugin: String,
    pub scope: Scope,
    pub events: usize,
    pub follows: Vec<String>,
    pub hooks: Vec<usize>,
}

/// Whether a plugin's hooks are all on, all off or some of each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookGroupState {
    On,
    Off,
    Partly,
}

impl HookGroupState {
    /// The word the list shows for the state. Output: On, Off or Partly on.
    pub fn word(self) -> &'static str {
        match self {
            Self::On => "On",
            Self::Off => "Off",
            Self::Partly => "Partly on",
        }
    }
}

/// Groups hooks per plugin and scope in the order the engine lists them, so a plugin with
/// several events reads as one plugin and not as several installs. Input: the hooks. Output:
/// the groups, each holding at least one hook and together every hook exactly once.
pub fn hook_groups(hooks: &[HookView]) -> Vec<HookGroup> {
    let mut groups: Vec<HookGroup> = Vec::new();
    for (index, hook) in hooks.iter().enumerate() {
        let at = match groups
            .iter()
            .position(|group| group.plugin == hook.plugin && group.scope == hook.scope)
        {
            Some(at) => at,
            None => {
                groups.push(HookGroup {
                    plugin: hook.plugin.clone(),
                    scope: hook.scope.clone(),
                    events: 0,
                    follows: Vec::new(),
                    hooks: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let group = &mut groups[at];
        if !group
            .hooks
            .iter()
            .any(|&seen| hooks[seen].event == hook.event)
        {
            group.events += 1;
        }
        if let Some(follows) = &hook.follows
            && !group.follows.contains(follows)
        {
            group.follows.push(follows.clone());
        }
        group.hooks.push(index);
    }
    assert_eq!(
        groups.iter().map(|group| group.hooks.len()).sum::<usize>(),
        hooks.len()
    );
    groups
}

/// Reads whether a plugin's hooks are on, off or partly on. Inputs: the hooks, the group
/// and what counts as on for one hook where the list is shown. Output: the state.
pub fn hook_group_state(
    hooks: &[HookView],
    group: &HookGroup,
    on: impl Fn(&HookView) -> bool,
) -> HookGroupState {
    assert!(!group.hooks.is_empty());
    let count = group
        .hooks
        .iter()
        .filter(|&&index| on(&hooks[index]))
        .count();
    if count == 0 {
        HookGroupState::Off
    } else if count == group.hooks.len() {
        HookGroupState::On
    } else {
        HookGroupState::Partly
    }
}

/// What a plugin's hooks row says: how many events it runs on and, when it came with a
/// skill or MCP server, that it runs only while that item is on. Input: the group. Output:
/// the line.
pub fn hook_group_line(group: &HookGroup) -> String {
    let events = match group.events {
        1 => "1 event".to_owned(),
        count => format!("{count} events"),
    };
    match group.follows.as_slice() {
        [] => events,
        follows => format!("{events}, runs while {} is on", follows.join(", ")),
    }
}

/// What one event row under a plugin says it runs: the tools it matches, when set, and
/// its command. Input: the hook. Output: the line.
pub fn hook_event_runs(hook: &HookView) -> String {
    match &hook.matcher {
        Some(matcher) => format!("{matcher}: {}", hook.command),
        None => hook.command.clone(),
    }
}
