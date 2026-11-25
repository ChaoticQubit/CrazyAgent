use super::*;

#[test]
fn each_skill_row_says_where_the_skill_came_from_before_its_folder() {
    for (width, height) in [(80, 24), (120, 36)] {
        let (skills, servers) = lists();
        let mut shell = Shell::with(
            plugin_ctx(width, height),
            Box::new(Plugins::with(skills, servers)),
        );
        let mut rows = render(&mut shell, width, height);
        for _ in 0..3 {
            shell.key(Key::Down);
        }
        rows.extend(render(&mut shell, width, height));
        let row = |name: &str| {
            rows.iter()
                .find(|row| row.contains(&format!(" {name} ")))
                .cloned()
                .unwrap_or_default()
        };
        assert!(
            row("frontend-design").contains("Added by you  ~/Skills/frontend-design"),
            "{width}: {rows:?}"
        );
        assert!(
            row("linear-method").contains("Imported from Claude Code  ~/.claude/ski"),
            "{width}: {rows:?}"
        );
        assert!(
            row("release-notes").contains("Default pack: release-notes  ~/Library/"),
            "the label comes first, so a cut keeps it: {width}: {rows:?}"
        );
    }
}
