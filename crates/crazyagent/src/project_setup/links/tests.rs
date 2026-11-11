use super::*;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db
}

fn link(path: &str, kind: LinkKind) -> DocLink {
    DocLink {
        path: path.into(),
        kind,
    }
}

fn text(path: &str, text: &str) -> LinkedText {
    LinkedText {
        path: path.into(),
        text: text.into(),
    }
}

fn saved(db: &mut Connection, project: &ProjectId, links: &[DocLink]) {
    let tx = db.transaction().unwrap();
    set(&tx, project, links).unwrap();
    tx.commit().unwrap();
}

fn project_folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(dir.path()).unwrap();
    (dir, folder)
}

fn write(folder: &Path, path: &str, body: &[u8]) {
    let file = folder.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, body).unwrap();
}

#[test]
fn links_round_trip_and_a_second_set_replaces_the_first() {
    let mut db = db();
    let project = ProjectId::new("alpha").unwrap();
    let other = ProjectId::new("beta").unwrap();
    saved(&mut db, &other, &[link("notes.md", LinkKind::Roadmap)]);
    let tx = db.transaction().unwrap();
    set(
        &tx,
        &project,
        &[
            link("docs/plan-2026.md", LinkKind::Roadmap),
            link(
                "docs/research-competitors.md",
                LinkKind::Research(ResearchKind::CompetitorResearch),
            ),
        ],
    )
    .unwrap();
    set(
        &tx,
        &project,
        &[link("docs/plan-2026.md", LinkKind::Roadmap)],
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        list(&db, &project).unwrap(),
        vec![link("docs/plan-2026.md", LinkKind::Roadmap)]
    );
    assert_eq!(
        list(&db, &other).unwrap(),
        vec![link("notes.md", LinkKind::Roadmap)]
    );
}

#[test]
fn every_kind_reads_back_as_it_was_linked_in_path_order() {
    let mut db = db();
    let project = ProjectId::new("alpha").unwrap();
    let mut links: Vec<DocLink> = ResearchKind::ALL
        .into_iter()
        .enumerate()
        .map(|(index, kind)| link(&format!("docs/r{index}.md"), LinkKind::Research(kind)))
        .collect();
    links.push(link("a/roadmap.md", LinkKind::Roadmap));
    saved(&mut db, &project, &links);
    links.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(list(&db, &project).unwrap(), links);
}

#[test]
fn a_link_outside_the_project_folder_is_refused() {
    let mut db = db();
    let project = ProjectId::new("alpha").unwrap();
    let long = "a/".repeat(LINK_PATH_BYTES_MAX / 2) + "x.md";
    let tx = db.transaction().unwrap();
    for path in [
        "../secret.md",
        "/etc/hosts",
        "docs/../../x.md",
        "docs/./x.md",
        "docs//x.md",
        "docs/x.md/",
        "",
        ".crazyagent/context/brief.md",
        ".CrazyAgent/context/brief.md",
        ".CRAZYAGENT/x.md",
        "docs\\..\\x.md",
        "C:x.md",
        "docs/a:b.md",
        "docs/\u{7}.md",
        long.as_str(),
    ] {
        let refused = set(&tx, &project, &[link(path, LinkKind::Roadmap)]);
        assert_eq!(refused.unwrap_err().0, "project_link_invalid", "{path:?}");
    }
    assert!(list(&tx, &project).unwrap().is_empty());
}

#[test]
fn more_links_than_the_bound_or_a_planning_research_link_are_refused() {
    let mut db = db();
    let project = ProjectId::new("alpha").unwrap();
    saved(&mut db, &project, &[link("kept.md", LinkKind::Roadmap)]);
    let many: Vec<DocLink> = (0..=LINKS_MAX)
        .map(|index| link(&format!("docs/{index}.md"), LinkKind::Roadmap))
        .collect();
    let tx = db.transaction().unwrap();
    assert_eq!(
        set(&tx, &project, &many).unwrap_err().0,
        "project_link_invalid"
    );
    assert_eq!(
        set(
            &tx,
            &project,
            &[link(
                "docs/plan.md",
                LinkKind::Research(ResearchKind::RoadmapPlanning)
            )]
        )
        .unwrap_err()
        .0,
        "project_link_invalid"
    );
    assert_eq!(
        list(&tx, &project).unwrap(),
        vec![link("kept.md", LinkKind::Roadmap)]
    );
    set(&tx, &project, &many[..LINKS_MAX]).unwrap();
    assert_eq!(list(&tx, &project).unwrap().len(), LINKS_MAX);
}

#[test]
fn a_stored_row_of_an_unknown_kind_or_a_path_outside_is_left_out() {
    let db = db();
    let project = ProjectId::new("alpha").unwrap();
    let insert = |path: &str, kind: &str| {
        db.execute(
            "INSERT INTO project_doc_links(project,path,kind) VALUES (?1,?2,?3)",
            params![project.as_str(), path, kind],
        )
        .unwrap();
    };
    for index in 0..LINKS_MAX + 8 {
        insert(&format!("0/{index:03}.md"), "research:newer_kind");
    }
    for (path, kind) in [
        ("a.md", "roadmap"),
        ("b.md", "research:market_research"),
        ("c.md", "research:unknown"),
        ("d.md", "release"),
        ("../e.md", "roadmap"),
        (".crazyagent/f.md", "research:market_research"),
        (".CrazyAgent/g.md", "roadmap"),
    ] {
        insert(path, kind);
    }
    assert_eq!(
        list(&db, &project).unwrap(),
        vec![
            link("a.md", LinkKind::Roadmap),
            link("b.md", LinkKind::Research(ResearchKind::MarketResearch)),
        ]
    );
}

#[test]
fn the_roadmap_is_the_linked_one_before_the_planned_one() {
    let mut db = db();
    let (_dir, folder) = project_folder();
    write(&folder, ".crazyagent/context/roadmap.md", b"# Roadmap\n");
    let project = ProjectId::new("alpha").unwrap();
    let planned = text(".crazyagent/context/roadmap.md", "# Roadmap\n");
    assert_eq!(
        roadmap(&db, &project, &folder).unwrap(),
        Some(planned.clone())
    );
    saved(
        &mut db,
        &project,
        &[link("docs/plan-2026.md", LinkKind::Roadmap)],
    );
    assert_eq!(roadmap(&db, &project, &folder).unwrap(), Some(planned));
    write(&folder, "docs/plan-2026.md", b"# Plan\n");
    assert_eq!(
        roadmap(&db, &project, &folder).unwrap(),
        Some(text("docs/plan-2026.md", "# Plan\n"))
    );
}

#[test]
fn no_roadmap_without_a_link_or_a_planned_file() {
    let db = db();
    let (_dir, folder) = project_folder();
    let project = ProjectId::new("alpha").unwrap();
    assert_eq!(roadmap(&db, &project, &folder).unwrap(), None);
}

#[test]
fn research_is_the_text_of_each_linked_research_file_present_in_the_folder() {
    let mut db = db();
    let (_dir, folder) = project_folder();
    std::fs::create_dir_all(folder.join("docs/gone")).unwrap();
    write(&folder, "docs/market.md", b"# Market\n");
    write(&folder, "docs/plan.md", b"# Plan\n");
    let project = ProjectId::new("alpha").unwrap();
    saved(
        &mut db,
        &project,
        &[
            link(
                "docs/market.md",
                LinkKind::Research(ResearchKind::MarketResearch),
            ),
            link(
                "docs/missing.md",
                LinkKind::Research(ResearchKind::CompetitorResearch),
            ),
            link(
                "docs/gone",
                LinkKind::Research(ResearchKind::TechStackResearch),
            ),
            link("docs/plan.md", LinkKind::Roadmap),
        ],
    );
    assert_eq!(
        research(&db, &project, &folder).unwrap(),
        vec![(
            ResearchKind::MarketResearch,
            text("docs/market.md", "# Market\n")
        )]
    );
}

#[test]
fn a_linked_file_or_folder_swapped_for_a_link_after_linking_is_not_read() {
    let mut db = db();
    let (_outside, away) = project_folder();
    write(&away, "research.md", b"SECRET research\n");
    write(&away, "plan.md", b"SECRET plan\n");
    let (_dir, folder) = project_folder();
    write(&folder, "docs/research.md", b"# Research\n");
    write(&folder, "notes/plan.md", b"# Plan\n");
    let project = ProjectId::new("alpha").unwrap();
    saved(
        &mut db,
        &project,
        &[
            link(
                "docs/research.md",
                LinkKind::Research(ResearchKind::MarketResearch),
            ),
            link("notes/plan.md", LinkKind::Roadmap),
        ],
    );
    assert_eq!(
        research(&db, &project, &folder).unwrap(),
        vec![(
            ResearchKind::MarketResearch,
            text("docs/research.md", "# Research\n")
        )]
    );
    assert_eq!(
        roadmap(&db, &project, &folder).unwrap(),
        Some(text("notes/plan.md", "# Plan\n"))
    );

    std::fs::remove_file(folder.join("docs/research.md")).unwrap();
    symlink(away.join("research.md"), folder.join("docs/research.md")).unwrap();
    std::fs::rename(folder.join("notes"), folder.join("notes-before")).unwrap();
    symlink(&away, folder.join("notes")).unwrap();

    assert_eq!(read_linked(&folder, "docs/research.md"), None);
    assert_eq!(read_linked(&folder, "notes/plan.md"), None);
    assert!(research(&db, &project, &folder).unwrap().is_empty());
    assert_eq!(roadmap(&db, &project, &folder).unwrap(), None);
}

#[test]
fn a_hard_linked_fifo_oversized_or_non_utf8_file_is_not_read() {
    let (_dir, folder) = project_folder();
    write(&folder, "docs/one.md", b"# One\n");
    std::fs::hard_link(folder.join("docs/one.md"), folder.join("docs/two.md")).unwrap();
    assert_eq!(read_linked(&folder, "docs/one.md"), None);
    assert_eq!(read_linked(&folder, "docs/two.md"), None);

    let made = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo").arg(folder.join("docs/pipe.md")),
    )
    .unwrap();
    assert!(made.success());
    assert_eq!(read_linked(&folder, "docs/pipe.md"), None);

    let cap = usize::try_from(LINK_BYTES_MAX).unwrap();
    write(&folder, "docs/full.md", &vec![b'a'; cap]);
    assert_eq!(
        read_linked(&folder, "docs/full.md").map(|text| text.len()),
        Some(cap)
    );
    write(&folder, "docs/over.md", &vec![b'a'; cap + 1]);
    assert_eq!(read_linked(&folder, "docs/over.md"), None);
    write(&folder, "docs/bytes.md", &[0xff, 0xfe, b'\n']);
    assert_eq!(read_linked(&folder, "docs/bytes.md"), None);
    write(&folder, ".CrazyAgent/context/brief.md", b"# Brief\n");
    assert_eq!(read_linked(&folder, ".CrazyAgent/context/brief.md"), None);
    assert_eq!(read_linked(&folder, "../one.md"), None);
}
