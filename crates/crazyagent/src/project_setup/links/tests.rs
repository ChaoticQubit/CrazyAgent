use super::*;
use std::os::unix::fs::symlink;

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

fn saved(db: &mut Connection, project: &ProjectId, links: &[DocLink]) {
    let tx = db.transaction().unwrap();
    set(&tx, project, links).unwrap();
    tx.commit().unwrap();
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
    set(&tx, &project, &many[..LINKS_MAX]).unwrap();
    assert_eq!(list(&tx, &project).unwrap().len(), LINKS_MAX);
}

#[test]
fn a_stored_row_of_an_unknown_kind_or_a_path_outside_is_left_out() {
    let db = db();
    let project = ProjectId::new("alpha").unwrap();
    for (path, kind) in [
        ("a.md", "roadmap"),
        ("b.md", "research:market_research"),
        ("c.md", "research:unknown"),
        ("d.md", "release"),
        ("../e.md", "roadmap"),
        (".crazyagent/f.md", "research:market_research"),
    ] {
        db.execute(
            "INSERT INTO project_doc_links(project,path,kind) VALUES (?1,?2,?3)",
            params![project.as_str(), path, kind],
        )
        .unwrap();
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
fn the_roadmap_file_is_the_linked_one_before_the_planned_one() {
    let mut db = db();
    let folder = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(folder.path()).unwrap();
    std::fs::create_dir_all(folder.join(".crazyagent/context")).unwrap();
    std::fs::write(folder.join(".crazyagent/context/roadmap.md"), "# Roadmap\n").unwrap();
    let project = ProjectId::new("alpha").unwrap();
    assert_eq!(
        roadmap_file(&db, &project, &folder).unwrap(),
        Some(folder.join(".crazyagent/context/roadmap.md"))
    );
    saved(
        &mut db,
        &project,
        &[link("docs/plan-2026.md", LinkKind::Roadmap)],
    );
    assert_eq!(
        roadmap_file(&db, &project, &folder).unwrap(),
        Some(folder.join(".crazyagent/context/roadmap.md"))
    );
    std::fs::create_dir_all(folder.join("docs")).unwrap();
    std::fs::write(folder.join("docs/plan-2026.md"), "# Plan\n").unwrap();
    assert_eq!(
        roadmap_file(&db, &project, &folder).unwrap(),
        Some(folder.join("docs/plan-2026.md"))
    );
}

#[test]
fn no_roadmap_without_a_link_or_a_planned_file() {
    let db = db();
    let folder = tempfile::tempdir().unwrap();
    let project = ProjectId::new("alpha").unwrap();
    assert_eq!(roadmap_file(&db, &project, folder.path()).unwrap(), None);
}

#[test]
fn a_linked_file_reached_through_a_link_is_not_returned() {
    let mut db = db();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("plan.md"), "# Elsewhere\n").unwrap();
    let folder = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(folder.path()).unwrap();
    symlink(outside.path(), folder.join("away")).unwrap();
    std::fs::create_dir_all(folder.join("docs")).unwrap();
    std::fs::write(folder.join("docs/real.md"), "# Real\n").unwrap();
    symlink(folder.join("docs/real.md"), folder.join("docs/alias.md")).unwrap();
    let project = ProjectId::new("alpha").unwrap();
    saved(
        &mut db,
        &project,
        &[
            link("away/plan.md", LinkKind::Roadmap),
            link(
                "docs/alias.md",
                LinkKind::Research(ResearchKind::MarketResearch),
            ),
            link(
                "away/plan.md",
                LinkKind::Research(ResearchKind::DesignResearch),
            ),
        ],
    );
    assert_eq!(roadmap_file(&db, &project, &folder).unwrap(), None);
    assert!(research_files(&db, &project, &folder).unwrap().is_empty());
}

#[test]
fn research_files_are_the_linked_research_files_present_in_the_folder() {
    let mut db = db();
    let folder = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(folder.path()).unwrap();
    std::fs::create_dir_all(folder.join("docs/gone")).unwrap();
    std::fs::write(folder.join("docs/market.md"), "# Market\n").unwrap();
    std::fs::write(folder.join("docs/plan.md"), "# Plan\n").unwrap();
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
        research_files(&db, &project, &folder).unwrap(),
        vec![(ResearchKind::MarketResearch, folder.join("docs/market.md"))]
    );
}
