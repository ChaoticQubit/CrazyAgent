use super::*;

/// The part only a native lead's teammates read after the lead's own instructions: they work
/// in the lead's agent team, and graphify finds the project graph through the lead's
/// worktree, since its read-only graph links exist only there. Input: the lead's worktree.
/// Output: the section, or `launch_path_invalid` for a path with a control character.
pub(super) fn teammate_section(worktree: &Path) -> Result<String, LaunchError> {
    let folder = worktree
        .to_str()
        .filter(|text| !text.chars().any(|c| c.is_control() || c == '`'))
        .ok_or_else(|| failure("launch_path_invalid"))?;
    Ok(format!(
        "\n## You are a teammate\n\nYou are a teammate in the Claude Code agent team of a CrazyAgent lead, which works in `{folder}`. Your own worktree has no project graph: run graphify with `--graph {folder}/graphify-out/graph.json`.\n"
    ))
}
