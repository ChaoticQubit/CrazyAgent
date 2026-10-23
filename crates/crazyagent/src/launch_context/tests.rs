use super::*;
use crate::plugins::{EffectiveSecret, EffectiveServer};
use crate::project_setup::TeamPlan;

fn stored(folder: &Path) -> StoredSetup {
    StoredSetup {
        project_id: ProjectId::new("bounds").unwrap(),
        name: "N".repeat(4000),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: folder.display().to_string(),
        created_ms: 1,
    }
}

#[test]
fn the_startup_instruction_stays_within_its_bounds_whatever_the_project_holds() {
    let folder = tempfile::tempdir().unwrap();
    let context = folder.path().join("context");
    fs::create_dir_all(context.join("documents")).unwrap();
    fs::create_dir_all(context.join("research")).unwrap();
    fs::write(context.join("brief.md"), "é".repeat(20_000)).unwrap();
    for index in 0..200 {
        let name = format!("{index:03}-{}.md", "d".repeat(240));
        fs::write(context.join("documents").join(name), "x").unwrap();
    }
    let memories = format!(
        "## Approved memories\n\n{}",
        "m".repeat(MEMORY_SECTION_BYTES_MAX - 32)
    );
    let setup = stored(folder.path());

    let text = render(&setup.project_id, Some(&setup), &memories);

    assert!(text.len() <= INSTRUCTIONS_BYTES_MAX, "{}", text.len());
    assert!(text.contains("The brief continues in context/brief.md."));
    assert!(text.contains("more files in context/"));
    assert!(text.ends_with(&memories));
    assert!(!text.contains(&"N".repeat(NAME_BYTES_MAX + 1)));
}

#[test]
fn a_project_without_a_setup_still_gets_a_plain_header() {
    let text = render(&ProjectId::new("bare").unwrap(), None, "");
    assert_eq!(
        text,
        "# Project context from CrazyAgent\n\nYou are working on the project bare.\n"
    );
    let with_memory = render(
        &ProjectId::new("bare").unwrap(),
        None,
        "## Approved memories\n\nKept.\n",
    );
    assert!(with_memory.ends_with("bare.\n\n## Approved memories\n\nKept.\n"));
}

#[test]
fn clip_never_splits_a_character() {
    assert_eq!(clip("aé", 2), "a");
    assert_eq!(clip("aé", 3), "aé");
    assert_eq!(clip("", 5), "");
}

#[test]
fn the_master_hears_skill_and_server_names_but_no_server_detail() {
    let store = tempfile::tempdir().unwrap();
    let skill = store.path().join("meal-tips");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: meal-tips\ndescription: Gives meal planning tips.\n---\n\nSay tips.\n",
    )
    .unwrap();
    let plugins = EffectivePlugins {
        skills: vec![EffectiveSkill {
            name: "meal-tips".into(),
            folder: skill.clone(),
        }],
        servers: vec![
            EffectiveServer {
                name: "approved-tools".into(),
                spec: ServerSpec::Stdio {
                    command: "/usr/bin/python3".into(),
                    args: vec!["/hidden/server.py".into()],
                },
                secrets: vec![EffectiveSecret {
                    name: "FIXTURE_TOKEN".into(),
                    reference: "secret-reference-1".into(),
                }],
            },
            EffectiveServer {
                name: "remote-docs".into(),
                spec: ServerSpec::Http {
                    url: "https://docs.example.test/mcp".into(),
                },
                secrets: vec![],
            },
        ],
    };

    let text = team_index(&plugins);

    assert!(text.contains("Your own tools are off"));
    assert!(text.contains("- Skill meal-tips: Gives meal planning tips.\n"));
    assert!(text.contains("- MCP server approved-tools\n"));
    assert!(text.contains("- MCP server remote-docs\n"));
    for hidden in [
        "/usr/bin/python3",
        "server.py",
        "FIXTURE_TOKEN",
        "secret-reference-1",
        "docs.example.test",
        store.path().to_str().unwrap(),
    ] {
        assert!(!text.contains(hidden), "{hidden}");
    }
    assert_eq!(team_index(&EffectivePlugins::default()), "");
}

#[test]
fn the_team_index_stays_within_its_bound_with_many_plugins() {
    let plugins = EffectivePlugins {
        skills: (0..300)
            .map(|index| EffectiveSkill {
                name: format!("skill-{index:03}-{}", "s".repeat(40)),
                folder: PathBuf::from("/missing-skill-folder"),
            })
            .collect(),
        servers: (0..100)
            .map(|index| EffectiveServer {
                name: format!("server-{index:03}"),
                spec: ServerSpec::Http {
                    url: "https://example.test/mcp".into(),
                },
                secrets: vec![],
            })
            .collect(),
    };

    let text = team_index(&plugins);

    assert!(
        text.len() <= TEAM_INDEX_BYTES_MAX + SECTION_SLACK,
        "{}",
        text.len()
    );
    assert!(text.contains("- Skill skill-000-"));
    assert!(!text.contains("- MCP server server-099"));
    assert!(text.ends_with(" more\n"));
}
