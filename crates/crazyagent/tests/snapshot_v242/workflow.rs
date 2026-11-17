use super::Scenario;
use super::e2e_support::{Engine, Owner, profile};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OwnerResponse, OwnerResult};
use crazyagent::instruction_check::{Finding, FindingKind};
use crazyagent::instruction_cleanup::{Scope, prompt};
use crazyagent::protocol::ProjectId;
use serde_json::{Value, json};
use std::future::Future;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// The project every engine scenario of P3 sets up, as New project does.
pub(super) const PROJECT: &str = "snapshot-shop";

/// The app-wide instructions the workflow scenarios save.
const APP_TEXT: &str = "Run the tests before every commit.\nKeep each change set small.\n";

/// The project instructions the workflow scenarios save.
const PROJECT_TEXT: &str = "Use the sensor fixtures in tests/data.\nNever call the live API.\n";

/// Lists the workflow scenarios: process text, the default process and the Edit with AI
/// request. Output: the scenarios in the order the snapshot row runs them.
pub(super) fn scenarios() -> Vec<Scenario> {
    let workflow = |name: &'static str, capture: fn(&Path) -> Result<Value, String>| Scenario {
        kind: "workflow",
        name,
        capture,
    };
    vec![
        workflow("default-process", default_process),
        workflow("process-text-unsaved", process_text_unsaved),
        workflow("process-text-app-saved", process_text_app_saved),
        workflow("process-text-project-saved", process_text_project_saved),
        workflow("process-save-project-blank", process_save_project_blank),
        workflow("propose-app-without-pool", propose_app_without_pool),
        workflow("propose-project-unsaved", propose_project_unsaved),
        workflow("propose-request-app", propose_request_app),
        workflow("propose-request-project", propose_request_project),
    ]
}

/// Starts the real engine on a private root with a fixture HOME made under the scratch
/// folder, so it never reads the owner's home. Input: the scratch folder. Output: the
/// running engine, killed when dropped.
pub(super) fn engine(scratch: &Path) -> Result<Engine, String> {
    let home = private_folder(scratch, "home")?;
    let mut engine = Engine::new("ca-snap-");
    engine.home = Some(home);
    engine.launch();
    Ok(engine)
}

/// Makes one private folder below the scratch folder. Inputs: the scratch folder and the
/// name. Output: the new folder's path.
pub(super) fn private_folder(scratch: &Path, name: &str) -> Result<PathBuf, String> {
    let folder = scratch.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .map_err(|error| format!("{name}: {error}"))?;
    Ok(folder)
}

/// Attaches to the engine as the TUI, runs the scenario's steps and detaches.
/// Inputs: the engine and the steps, which hand the owner back with the capture.
/// Output: the capture.
pub(super) fn attached<F, Fut>(engine: &Engine, steps: F) -> Result<Value, String>
where
    F: FnOnce(Owner) -> Fut,
    Fut: Future<Output = (Owner, Result<Value, String>)>,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            let (mut owner, captured) = steps(Owner::attach(engine).await).await;
            owner.controller.detach();
            captured
        })
}

/// Saves the first-run profile, a projects home below the scratch folder and the
/// [`PROJECT`] project, as the first run and New project do. Inputs: the owner and the
/// scratch folder. Output: the project, or the step that was not applied.
pub(super) async fn seeded(owner: &mut Owner, scratch: &Path) -> Result<ProjectId, String> {
    let projects = private_folder(scratch, "projects")?;
    applied("profile", owner.save_profile(profile()).await)?;
    applied("projects home", owner.set_home(&projects).await)?;
    applied(
        "project",
        owner.create_project(PROJECT, "Snapshot shop").await,
    )?;
    ProjectId::new(PROJECT).map_err(|error| error.to_string())
}

/// Accepts only an applied owner change. Inputs: the step's name and its result.
/// Output: unit, or the step with what the engine answered.
pub(super) fn applied(step: &str, result: OwnerResult) -> Result<(), String> {
    match result {
        OwnerResult::Applied => Ok(()),
        other => Err(format!("{step}: {other:?}")),
    }
}

/// Records one owner change's reply: the result alone when the engine applied or committed
/// it, since the operation's provenance and revision differ per run, else the whole response,
/// or the plain code it was refused with. Input: the reply.
/// Output: `{"result": ..}`, `{"response": ..}` or `{"refused": code}`.
pub(super) fn recorded(answer: Result<OwnerResponse, EngineError>) -> Result<Value, String> {
    let recorded = match answer {
        Ok(OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation }) => {
            serde_json::to_value(&operation.result).map(|result| json!({ "result": result }))
        }
        Ok(other) => serde_json::to_value(&other).map(|response| json!({ "response": response })),
        Err(error) => return Ok(json!({ "refused": error.0 })),
    };
    recorded.map_err(|error| error.to_string())
}

/// Records one engine reply whole: the value it gave, or the plain code it refused with.
/// Input: the reply. Output: `{"ok": value}` or `{"refused": code}`.
pub(super) fn reply<T: serde::Serialize>(answer: Result<T, EngineError>) -> Result<Value, String> {
    match answer {
        Ok(value) => serde_json::to_value(value)
            .map(|value| json!({ "ok": value }))
            .map_err(|error| error.to_string()),
        Err(error) => Ok(json!({ "refused": error.0 })),
    }
}

/// The CrazyAgent default process the engine shows for a preview.
/// Input: the scratch folder. Output: the text.
fn default_process(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    attached(&engine, |owner| async move {
        let captured = reply(owner.controller.default_process().await);
        (owner, captured)
    })
}

/// The app-wide instructions before anybody saved them.
/// Input: the scratch folder. Output: the read.
fn process_text_unsaved(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    attached(&engine, |owner| async move {
        let captured = reply(owner.controller.process_text(None).await);
        (owner, captured)
    })
}

/// A first save of the app-wide instructions and the read after it.
/// Input: the scratch folder. Output: the save's reply and the read.
fn process_text_app_saved(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    attached(&engine, |owner| async move {
        let saved = owner
            .controller
            .save_process_text(None, APP_TEXT.into(), 0, None)
            .await;
        let read = owner.controller.process_text(None).await;
        let captured = reply(saved)
            .and_then(|saved| reply(read).map(|read| json!({ "saved": saved, "read": read })));
        (owner, captured)
    })
}

/// A first save of one project's instructions and the read after it.
/// Input: the scratch folder. Output: the save's reply and the read.
fn process_text_project_saved(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        let project = match seeded(&mut owner, &scratch).await {
            Ok(project) => project,
            Err(problem) => return (owner, Err(problem)),
        };
        let saved = owner
            .controller
            .save_process_text(Some(project.clone()), PROJECT_TEXT.into(), 0, None)
            .await;
        let read = owner.controller.process_text(Some(project)).await;
        let captured = reply(saved)
            .and_then(|saved| reply(read).map(|read| json!({ "saved": saved, "read": read })));
        (owner, captured)
    })
}

/// A blank save of a project's instructions while no app-wide text is saved, which would
/// leave its agents no instructions at all. Input: the scratch folder. Output: the reply.
fn process_save_project_blank(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        let project = match seeded(&mut owner, &scratch).await {
            Ok(project) => project,
            Err(problem) => return (owner, Err(problem)),
        };
        let captured = reply(
            owner
                .controller
                .save_process_text(Some(project), String::new(), 0, None)
                .await,
        );
        (owner, captured)
    })
}

/// Edit with AI on the saved app-wide text with no pool picked for the run.
/// Input: the scratch folder. Output: the reply.
fn propose_app_without_pool(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    attached(&engine, |owner| async move {
        let saved = owner
            .controller
            .save_process_text(None, APP_TEXT.into(), 0, None)
            .await;
        let proposed = owner.controller.propose_process(None, None).await;
        let captured = reply(saved).and_then(|saved| {
            reply(proposed).map(|proposed| json!({ "saved": saved, "proposed": proposed }))
        });
        (owner, captured)
    })
}

/// Edit with AI on a new project that has no saved text and no team.
/// Input: the scratch folder. Output: the reply.
fn propose_project_unsaved(scratch: &Path) -> Result<Value, String> {
    let engine = engine(scratch)?;
    let scratch = scratch.to_path_buf();
    attached(&engine, |mut owner| async move {
        let project = match seeded(&mut owner, &scratch).await {
            Ok(project) => project,
            Err(problem) => return (owner, Err(problem)),
        };
        let captured = reply(owner.controller.propose_process(Some(project), None).await);
        (owner, captured)
    })
}

/// The request Edit with AI sends the model for the app-wide text with one engine finding.
/// The run needs a pool whose account key would be stored in the keychain, so the request
/// is taken from the public builder the engine calls. Input: the scratch folder, unused.
/// Output: the request text.
fn propose_request_app(_scratch: &Path) -> Result<Value, String> {
    let found = [Finding::new(
        FindingKind::Duplicate,
        vec![1],
        "This line is also in the project instructions.",
    )];
    Ok(json!({ "prompt": prompt(Scope::App, APP_TEXT, "", &found) }))
}

/// The request Edit with AI sends the model for a project's text compared with the app-wide
/// text, without findings, from the same public builder. Input: the scratch folder, unused.
/// Output: the request text.
fn propose_request_project(_scratch: &Path) -> Result<Value, String> {
    Ok(json!({ "prompt": prompt(Scope::Project, APP_TEXT, PROJECT_TEXT, &[]) }))
}
