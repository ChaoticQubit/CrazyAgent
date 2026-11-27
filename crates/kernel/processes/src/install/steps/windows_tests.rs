use super::*;
use crate::install::{InstallRecipe, TEMPLATE_MISMATCH, Verb};

/// On Windows the npm template takes Node's own `npm.cmd`, the npm every Windows Node ships,
/// for an install and an update alike, with the same arguments and prefix as on Unix; an
/// `npm` with no `.cmd` is not the installer there.
#[test]
fn npm_on_windows_is_the_npm_cmd_node_ships() {
    let node = PathBuf::from(r"C:\Program Files\nodejs");
    let prefix = PathBuf::from(r"C:\engine\setup\harnesses");
    let paths = |program: &str| InstallPaths {
        program: Some(node.join(program)),
        npm_prefix: Some(prefix.clone()),
        ..InstallPaths::default()
    };
    for verb in [Verb::Install, Verb::Update] {
        let recipe = InstallRecipe::NpmGlobal {
            package: "@openai/codex".to_owned(),
            version: String::new(),
            verb,
            flags: Vec::new(),
        };
        assert_eq!(
            steps(&recipe, &paths("npm.cmd")),
            Ok(vec![Step {
                program: node.join("npm.cmd"),
                args: vec!["install".into(), "-g".into(), "@openai/codex".into()],
                env: vec![("npm_config_prefix", prefix.clone().into_os_string())],
            }]),
            "{verb:?}"
        );
        assert_eq!(
            steps(&recipe, &paths("npm")),
            Err(TEMPLATE_MISMATCH),
            "{verb:?}"
        );
    }
}
