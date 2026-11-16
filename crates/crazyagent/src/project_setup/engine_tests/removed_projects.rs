use super::*;
use crate::memory::library::MemoryScope;

const NOTE: &str = "Deploys go out on Tuesdays.";

/// A folder under the test engine's projects home, made on disk. Inputs: the engine and the
/// folder names below the home. Output: the canonical folder.
fn folder(engine: &Engine, names: &[&str]) -> PathBuf {
    let mut path = engine.home();
    for name in names {
        path.push(name);
    }
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

/// Starts onboarding a folder on the test pool, as Found's Add does. Inputs: the owner
/// client, the folder and the client generation. Output: the engine's result.
async fn onboard(owner: &mut OwnerController, folder: &Path, generation: u64) -> OwnerResult {
    result(
        owner
            .start_onboarding(
                folder.to_str().unwrap().into(),
                CredentialRef::new("pool-team").unwrap(),
                generation,
            )
            .await
            .unwrap(),
    )
}

fn started(project: &str) -> OwnerResult {
    OwnerResult::OnboardingStarted {
        project_id: ProjectId::new(project).unwrap(),
    }
}

/// Saves the owner's note for one project. Inputs: the owner client, the project and the
/// client generation. Output: none; it checks the note was saved.
async fn note(owner: &mut OwnerController, project: &ProjectId, generation: u64) {
    let added = owner
        .add_memory(
            MemoryScope::Project {
                project_id: project.clone(),
            },
            NOTE.into(),
            generation,
        )
        .await
        .unwrap();
    assert_eq!(result(added), OwnerResult::Applied);
}

fn notes(engine: &Engine, project: &str) -> i64 {
    engine.rows(&format!(
        "SELECT count(*) FROM memory_library WHERE project='{project}'"
    ))
}

fn write(engine: &Engine, sql: &str) {
    let db = rusqlite::Connection::open(engine.root.path().join("engine.db")).unwrap();
    db.busy_timeout(ENGINE_WAIT).unwrap();
    db.execute_batch(sql).unwrap();
}

async fn has_note(owner: &mut OwnerController, project: &str) -> bool {
    owner
        .approved_memories(ProjectId::new(project).unwrap())
        .await
        .unwrap()
        .items
        .iter()
        .any(|item| item.text == NOTE)
}

#[tokio::test]
async fn a_project_removed_from_the_dashboard_comes_back_with_its_notes_when_its_own_folder_is_added_again()
 {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let path = folder(&engine, &["my-app"]);
    assert_eq!(onboard(&mut owner, &path, 1).await, started("my-app"));
    let project = ProjectId::new("my-app").unwrap();
    note(&mut owner, &project, 2).await;
    let removed = owner.remove_project(project.clone(), 3).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    assert_eq!(notes(&engine, "my-app"), 1, "Remove from dashboard keeps the note");

    assert_eq!(
        onboard(&mut owner, &path, 4).await,
        started("my-app"),
        "the same folder brings the same project back"
    );
    assert!(has_note(&mut owner, "my-app").await, "its note comes back with it");
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects_removed WHERE project='my-app'"),
        0,
        "a project back is held no more"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn another_folder_with_a_removed_projects_name_gets_a_new_id_and_none_of_its_notes() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let path = folder(&engine, &["my-app"]);
    assert_eq!(onboard(&mut owner, &path, 1).await, started("my-app"));
    let project = ProjectId::new("my-app").unwrap();
    note(&mut owner, &project, 2).await;
    let removed = owner.remove_project(project.clone(), 3).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);

    let other = folder(&engine, &["elsewhere", "my-app"]);
    assert_eq!(
        onboard(&mut owner, &other, 4).await,
        started("my-app-2"),
        "another folder with the same name is a new project"
    );
    assert!(!has_note(&mut owner, "my-app-2").await, "the new project gets none of the notes");
    assert_eq!(notes(&engine, "my-app-2"), 0);
    assert_eq!(notes(&engine, "my-app"), 1, "the note stays with its own project");
    assert_eq!(
        onboard(&mut owner, &path, 5).await,
        started("my-app"),
        "the removed project's own folder still brings it back"
    );
    assert!(has_note(&mut owner, "my-app").await);
    drop(owner);
    engine.stop().await;
}

/// Delete everything forgets the project's notes, imports and saved selections, and its id is
/// never given out again, not even to its own folder made again.
/// Runs on macOS only, the one platform that offers Delete everything (DEC-118).
#[cfg(target_os = "macos")]
#[tokio::test]
async fn delete_everything_forgets_the_notes_and_the_id_never_comes_back() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let path = folder(&engine, &["my-app"]);
    assert_eq!(onboard(&mut owner, &path, 1).await, started("my-app"));
    let project = ProjectId::new("my-app").unwrap();
    note(&mut owner, &project, 2).await;
    write(
        &engine,
        "INSERT INTO memory_library(id,project,origin,text) VALUES
            ('m-import','my-app','{\"kind\":\"imported\",\"file\":\"notes.md\"}','Imported text.'),
            ('m-pick','my-app','{\"kind\":\"selected\",\"path\":\"a.md\"}','Pick.')",
    );
    assert_eq!(notes(&engine, "my-app"), 3);
    let name = owner
        .project_setup(project.clone())
        .await
        .unwrap()
        .unwrap()
        .name;
    let deleted = owner.delete_project(project.clone(), name, 3).await.unwrap();
    assert_eq!(result(deleted), OwnerResult::Applied);
    assert_eq!(
        notes(&engine, "my-app"),
        0,
        "Delete everything forgets its notes, imports and saved selections"
    );
    assert!(!path.exists(), "its folder went to the Trash");

    let again = folder(&engine, &["my-app"]);
    assert_eq!(again, path);
    assert_eq!(
        onboard(&mut owner, &again, 4).await,
        started("my-app-2"),
        "a deleted project's id is never given out again"
    );
    assert_eq!(notes(&engine, "my-app-2"), 0);
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn start_onboarding_answers_the_id_it_gave() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let path = folder(&engine, &["Fresh App"]);
    assert_eq!(onboard(&mut owner, &path, 1).await, started("fresh-app"));
    assert_eq!(
        onboard(&mut owner, &path, 2).await,
        started("fresh-app"),
        "a retry of a folder already onboarding answers its project"
    );
    assert_eq!(
        engine.rows("SELECT count(*) FROM onboarding WHERE project='fresh-app'"),
        1
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn new_project_refuses_an_id_a_removal_holds() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let project = ProjectId::new("setup-pooled").unwrap();
    let created = owner
        .setup_project(project.clone(), spec("Meal planner"), pooled_setup(vec![]), 1)
        .await
        .unwrap();
    assert_eq!(result(created), OwnerResult::Applied);
    let removed = owner.remove_project(project.clone(), 2).await.unwrap();
    assert_eq!(result(removed), OwnerResult::Applied);
    let again = owner
        .setup_project(project.clone(), spec("Meal planner"), pooled_setup(vec![]), 3)
        .await
        .unwrap();
    assert_eq!(result(again), rejected("project_id_removed"));
    assert_eq!(
        engine.rows("SELECT count(*) FROM projects WHERE id='setup-pooled'"),
        0,
        "a New Project never gets a removed project's id"
    );
    drop(owner);
    engine.stop().await;
}

#[tokio::test]
async fn a_full_record_refuses_a_removal_before_anything_is_removed() {
    let engine = Engine::start().await;
    let mut owner = engine.owner().await;
    let path = folder(&engine, &["my-app"]);
    assert_eq!(onboard(&mut owner, &path, 1).await, started("my-app"));
    let project = ProjectId::new("my-app").unwrap();
    note(&mut owner, &project, 2).await;
    write(
        &engine,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{})
             INSERT INTO projects_removed(project,folder,returns,removed_ms)
             SELECT 'gone-'||i, NULL, 0, 0 FROM n",
            crate::project_setup::removed::REMOVED_MAX
        ),
    );
    let removed = owner.remove_project(project.clone(), 3).await.unwrap();
    assert_eq!(result(removed), rejected("projects_removed_full"));
    assert_eq!(engine.rows("SELECT count(*) FROM projects WHERE id='my-app'"), 1);
    assert_eq!(notes(&engine, "my-app"), 1, "a refused removal keeps everything");
    #[cfg(target_os = "macos")]
    {
        let name = owner
            .project_setup(project.clone())
            .await
            .unwrap()
            .unwrap()
            .name;
        let deleted = owner.delete_project(project.clone(), name, 4).await.unwrap();
        assert_eq!(result(deleted), rejected("projects_removed_full"));
        assert!(path.is_dir(), "a refused Delete everything moves nothing to the Trash");
        assert_eq!(engine.rows("SELECT count(*) FROM projects WHERE id='my-app'"), 1);
    }
    drop(owner);
    engine.stop().await;
}
