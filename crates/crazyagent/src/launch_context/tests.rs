use super::*;
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
    let memories: Vec<Memory> = (0..100)
        .map(|index| Memory {
            global: index % 2 == 0,
            revision: index,
            text: format!("memory {index} {}", "m".repeat(3900)),
        })
        .collect();
    let setup = stored(folder.path());

    let text = render(&setup.project_id, Some(&setup), &memories);

    assert!(text.len() <= INSTRUCTIONS_BYTES_MAX, "{}", text.len());
    assert!(text.contains("The brief continues in context/brief.md."));
    assert!(text.contains("more files in context/"));
    assert!(text.contains("Some approved memories were left out"));
    assert!(text.matches("\n### From ").count() <= MEMORY_ITEMS_MAX);
    assert!(!text.contains(&"N".repeat(NAME_BYTES_MAX + 1)));
}

#[test]
fn approved_memories_come_project_first_then_global_newest_first() {
    let memories = vec![
        Memory {
            global: false,
            revision: 9,
            text: "newest project".into(),
        },
        Memory {
            global: false,
            revision: 3,
            text: "older project".into(),
        },
        Memory {
            global: true,
            revision: 12,
            text: "global".into(),
        },
    ];
    let section = render_memories(&memories);
    let first = section.find("newest project").unwrap();
    let second = section.find("older project").unwrap();
    let third = section.find("global").unwrap();
    assert!(first < second && second < third, "{section}");
    assert!(!section.contains("left out"));
    assert_eq!(render_memories(&[]), "");
}

#[test]
fn a_project_without_a_setup_still_gets_a_plain_header() {
    let text = render(&ProjectId::new("bare").unwrap(), None, &[]);
    assert_eq!(
        text,
        "# Project context from CrazyAgent\n\nYou are working on the project bare.\n"
    );
}

#[test]
fn clip_never_splits_a_character() {
    assert_eq!(clip("aé", 2), "a");
    assert_eq!(clip("aé", 3), "aé");
    assert_eq!(clip("", 5), "");
}
