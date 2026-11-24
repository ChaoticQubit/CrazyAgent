use super::*;

#[tokio::test]
async fn owner_adds_documents_and_deletes_research_and_documents_through_the_engine() {
    let engine = Engine::start().await;
    let sources = tempfile::tempdir().unwrap();
    std::fs::write(sources.path().join("first.md"), "first").unwrap();
    std::fs::write(sources.path().join("later.txt"), "later notes").unwrap();
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-editable").unwrap();
    let setup = ProjectSetup {
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![sources.path().join("first.md").to_str().unwrap().into()],
    };
    assert_eq!(
        result(
            owner
                .setup_project(project.clone(), spec("Editable"), setup, 1)
                .await
                .unwrap()
        ),
        OwnerResult::Applied
    );
    let folder = owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap()
        .state_folder();

    let added = owner
        .add_project_documents(
            project.clone(),
            vec![sources.path().join("later.txt").to_str().unwrap().into()],
            2,
        )
        .await
        .unwrap();
    assert_eq!(result(added), OwnerResult::Applied);
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        stored
            .documents
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["first.md", "later.txt"]
    );
    let brief = std::fs::read_to_string(folder.join("context/brief.md")).unwrap();
    assert!(brief.contains("- context/documents/later.txt"), "{brief}");
    let duplicate = owner
        .add_project_documents(
            project.clone(),
            vec![sources.path().join("first.md").to_str().unwrap().into()],
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        result(duplicate),
        OwnerResult::Rejected {
            code: "document_name_duplicate".into()
        }
    );
    assert_eq!(
        std::fs::read(folder.join("context/documents/first.md")).unwrap(),
        b"first"
    );

    std::fs::write(
        folder.join("context/research/market_research.md"),
        "# Market\n",
    )
    .unwrap();
    for (path, generation) in [
        ("context/research/market_research.md", 4),
        ("context/documents/first.md", 5),
    ] {
        let deleted = owner
            .delete_project_context(project.clone(), path.into(), generation)
            .await
            .unwrap();
        assert_eq!(result(deleted), OwnerResult::Applied, "{path}");
        assert!(!folder.join(path).exists(), "{path}");
    }
    let stored = owner.project_setup(project.clone()).await.unwrap().unwrap();
    assert_eq!(
        stored
            .documents
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["later.txt"]
    );
    let brief = std::fs::read_to_string(folder.join("context/brief.md")).unwrap();
    assert!(!brief.contains("first.md"), "{brief}");
    assert!(brief.contains("- context/documents/later.txt"), "{brief}");
    for (path, code, generation) in [
        ("context/brief.md", "context_item_protected", 6),
        ("context/documents/first.md", "context_item_missing", 7),
    ] {
        let refused = owner
            .delete_project_context(project.clone(), path.into(), generation)
            .await
            .unwrap();
        assert_eq!(
            result(refused),
            OwnerResult::Rejected { code: code.into() },
            "{path}"
        );
    }
    std::fs::create_dir(folder.join("context/research/nested.md")).unwrap();
    let directory = owner
        .delete_project_context(project.clone(), "context/research/nested.md".into(), 8)
        .await
        .unwrap();
    assert_eq!(
        result(directory),
        OwnerResult::Rejected {
            code: "context_item_missing".into()
        }
    );
    assert!(folder.join("context/research/nested.md").is_dir());
    let outside = sources.path().join("outside.md");
    std::fs::write(&outside, "keep").unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("context/research/linked.md")).unwrap();
    let linked = owner
        .delete_project_context(project.clone(), "context/research/linked.md".into(), 9)
        .await
        .unwrap();
    assert_eq!(
        result(linked),
        OwnerResult::Rejected {
            code: "context_item_missing".into()
        }
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    assert!(folder.join("context/brief.md").is_file());
    drop(owner);
    engine.stop().await;
}
