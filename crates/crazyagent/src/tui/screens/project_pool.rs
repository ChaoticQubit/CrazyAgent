use crate::config::{AccountPool, EffectiveConfig, ModelAccessEdit};
use crate::protocol::{CredentialRef, ProjectId};

const POOL_PROJECTS_MAX: usize = 64;

/// Builds the settings edits that give a project one pool.
/// Takes the engine configuration, the project and the pool to assign.
/// Returns the edits in send order (other pools lose the project first), none when it is already assigned, or a plain problem.
pub(in crate::tui) fn assign_edits(
    config: &EffectiveConfig,
    project: &ProjectId,
    pool: &CredentialRef,
) -> Result<Vec<ModelAccessEdit>, String> {
    let access = config
        .settings
        .model_access
        .as_ref()
        .ok_or("There are no pools yet. Create one in Settings, Pools.")?;
    let project = CredentialRef::new(project.as_str())
        .map_err(|_| "This project can't be given a pool.".to_owned())?;
    let target = access
        .pools
        .iter()
        .find(|candidate| &candidate.id == pool)
        .ok_or("That pool no longer exists. Pick another pool.")?;
    if target.members.is_empty() {
        return Err("That pool has no accounts. Add an account to it first.".into());
    }
    let holds = |candidate: &AccountPool| candidate.projects.contains(&project);
    let others: Vec<&AccountPool> = access
        .pools
        .iter()
        .filter(|candidate| candidate.id != *pool && holds(candidate))
        .collect();
    if holds(target) && others.is_empty() {
        return Ok(Vec::new());
    }
    if !holds(target) && target.projects.len() >= POOL_PROJECTS_MAX {
        return Err(format!(
            "That pool already serves {POOL_PROJECTS_MAX} projects. Pick another pool."
        ));
    }
    let mut edits: Vec<ModelAccessEdit> = others
        .into_iter()
        .map(|other| {
            let mut pool = other.clone();
            pool.projects.retain(|candidate| candidate != &project);
            ModelAccessEdit::ReplacePool { pool }
        })
        .collect();
    if !holds(target) {
        let mut pool = target.clone();
        pool.projects.push(project.clone());
        edits.push(ModelAccessEdit::ReplacePool { pool });
    }
    assert!(!edits.is_empty());
    assert!(edits.iter().all(|edit| edit.validate().is_ok()));
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_config_bytes;

    const SETTINGS: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-api"]
[[model_access.connections]]
id = "a-api"
provider = "provider-api"
harness = "rcli"
protocol = "openai"
account_ref = "account-api"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "model-api", author = "author-api" }
auth = { method = "none" }
[[model_access.pools]]
id = "pool-main"
name = "Main"
members = ["a-api"]
projects = ["other-project"]
roles = []
fallback_allowed = false
[[model_access.pools]]
id = "pool-local"
members = ["a-api"]
projects = ["tidepool"]
roles = []
fallback_allowed = false
"#;

    fn config(settings: &str) -> EffectiveConfig {
        resolve_config_bytes(settings, "").unwrap()
    }

    fn projects(edit: &ModelAccessEdit) -> (&str, Vec<&str>) {
        let ModelAccessEdit::ReplacePool { pool } = edit else {
            panic!("pool assignment only replaces pools, got {edit:?}");
        };
        (
            pool.id.as_str(),
            pool.projects.iter().map(CredentialRef::as_str).collect(),
        )
    }

    #[test]
    fn assigning_moves_the_project_out_of_its_old_pool_first() {
        let config = config(SETTINGS);
        let project = ProjectId::new("tidepool").unwrap();
        let main = CredentialRef::new("pool-main").unwrap();
        let edits = assign_edits(&config, &project, &main).unwrap();
        assert_eq!(
            edits.iter().map(projects).collect::<Vec<_>>(),
            [
                ("pool-local", vec![]),
                ("pool-main", vec!["other-project", "tidepool"])
            ]
        );
        let ModelAccessEdit::ReplacePool { pool } = &edits[1] else {
            unreachable!()
        };
        assert_eq!(pool.name.as_deref(), Some("Main"));
        assert_eq!(pool.members.len(), 1);
        let local = CredentialRef::new("pool-local").unwrap();
        assert_eq!(assign_edits(&config, &project, &local).unwrap(), []);
        let fresh = ProjectId::new("fieldnotes").unwrap();
        assert_eq!(
            assign_edits(&config, &fresh, &local)
                .unwrap()
                .iter()
                .map(projects)
                .collect::<Vec<_>>(),
            [("pool-local", vec!["tidepool", "fieldnotes"])]
        );
    }

    #[test]
    fn missing_and_full_pools_explain_what_to_do() {
        let project = ProjectId::new("tidepool").unwrap();
        let none = config("timeout_ms = 1000\n");
        assert_eq!(
            assign_edits(&none, &project, &CredentialRef::new("pool-main").unwrap()),
            Err("There are no pools yet. Create one in Settings, Pools.".into())
        );
        let config = config(SETTINGS);
        assert_eq!(
            assign_edits(&config, &project, &CredentialRef::new("pool-gone").unwrap()),
            Err("That pool no longer exists. Pick another pool.".into())
        );
        let many: Vec<String> = (0..64).map(|n| format!("\"p{n}\"")).collect();
        let full = SETTINGS.replace(
            "projects = [\"other-project\"]",
            &format!("projects = [{}]", many.join(", ")),
        );
        assert_eq!(
            assign_edits(
                &super::tests::config(&full),
                &project,
                &CredentialRef::new("pool-main").unwrap()
            ),
            Err("That pool already serves 64 projects. Pick another pool.".into())
        );
    }
}
