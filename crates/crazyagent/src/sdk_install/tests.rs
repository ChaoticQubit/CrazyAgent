use super::*;
use crate::sdk_slice::{EngineRefusals, bundled_grants};
use bus_gateway::{AllOn, Gateway, Grants};
use kernel_plugin_host::generated::permissions::BUNDLED_GRANTS;
use sdk::Tier;
use sdk::process::Provide;
use sdk_host::{Bound, Providers};
use std::sync::Mutex;

const HASH: &str = "743f85035a1996ecb350659e01cd9faea9709829be09b1b4b03c73eaf3c9cd73";

static SEEN: Mutex<Vec<Recipe>> = Mutex::new(Vec::new());

fn empty(installer: Installer) -> InstallRecipe {
    InstallRecipe {
        installer,
        name: String::new(),
        verb: None,
        version: None,
        target: None,
        python: None,
        lock: None,
        sha256: None,
        manager: None,
        args: Vec::new(),
        flags: Vec::new(),
    }
}

fn some(text: &str) -> Option<String> {
    Some(text.to_owned())
}

/// One recipe per installer with exactly its used fields set, as the bundled pins and
/// manifests send them.
fn every_installer() -> Vec<InstallRecipe> {
    let download = |installer| InstallRecipe {
        version: some("0.13.5"),
        target: some("aarch64-apple-darwin"),
        sha256: some(HASH),
        ..empty(installer)
    };
    vec![
        download(Installer::UvRelease),
        download(Installer::NodeArchive),
        InstallRecipe {
            name: "graphifyy".to_owned(),
            version: some("0.4.21"),
            python: some("3.12"),
            lock: some("graphifyy==0.4.21 --hash=sha256:00"),
            ..empty(Installer::UvTool)
        },
        InstallRecipe {
            name: "openclaw-cli".to_owned(),
            verb: Some(InstallVerb::Update),
            ..empty(Installer::BrewFormula)
        },
        InstallRecipe {
            name: "codex".to_owned(),
            verb: Some(InstallVerb::Install),
            ..empty(Installer::BrewCask)
        },
        InstallRecipe {
            name: "openclaw".to_owned(),
            verb: Some(InstallVerb::Update),
            version: some("latest"),
            flags: vec!["--allow-scripts=openclaw".to_owned()],
            ..empty(Installer::NpmGlobal)
        },
        InstallRecipe {
            name: "astral-sh.uv".to_owned(),
            version: some("0.13.5"),
            ..empty(Installer::Winget)
        },
        InstallRecipe {
            name: "tmux".to_owned(),
            manager: Some(SystemManager::Dnf),
            ..empty(Installer::SystemPackage)
        },
        InstallRecipe {
            name: "codex".to_owned(),
            args: vec!["update".to_owned()],
            ..empty(Installer::SelfUpdate)
        },
    ]
}

/// Every field a recipe may carry, each set to a value its shape accepts.
fn with_field(recipe: &InstallRecipe, field: u16) -> InstallRecipe {
    let mut changed = recipe.clone();
    match field {
        NAME => changed.name = "x".to_owned(),
        VERB => changed.verb = Some(InstallVerb::Install),
        VERSION => changed.version = some("1.0"),
        TARGET => changed.target = some("aarch64-apple-darwin"),
        PYTHON => changed.python = some("3.12"),
        LOCK => changed.lock = some("x"),
        SHA256 => changed.sha256 = some(HASH),
        MANAGER => changed.manager = Some(SystemManager::AptGet),
        ARGS => changed.args = vec!["update".to_owned()],
        FLAGS => changed.flags = vec!["--ignore-scripts".to_owned()],
        _ => unreachable!("one field at a time"),
    }
    changed
}

fn without_field(recipe: &InstallRecipe, field: u16) -> InstallRecipe {
    let mut changed = recipe.clone();
    match field {
        NAME => changed.name = String::new(),
        VERB => changed.verb = None,
        VERSION => changed.version = None,
        TARGET => changed.target = None,
        PYTHON => changed.python = None,
        LOCK => changed.lock = None,
        SHA256 => changed.sha256 = None,
        MANAGER => changed.manager = None,
        ARGS => changed.args = Vec::new(),
        FLAGS => changed.flags = Vec::new(),
        _ => unreachable!("one field at a time"),
    }
    changed
}

const FIELDS: [u16; 10] = [
    NAME, VERB, VERSION, TARGET, PYTHON, LOCK, SHA256, MANAGER, ARGS, FLAGS,
];

fn recorded(recipe: &Recipe) -> Result<Ran, &'static str> {
    SEEN.lock().unwrap().push(recipe.clone());
    Ok(Ran {
        code: 0,
        output: b"done".to_vec(),
        env: Vec::new(),
    })
}

fn answered(_: &Recipe) -> Result<Ran, &'static str> {
    Ok(Ran {
        code: 0,
        output: b"done".to_vec(),
        env: Vec::new(),
    })
}

fn never_run(_: &Recipe) -> Result<Ran, &'static str> {
    panic!("no executor runs for a refused call")
}

/// Each installer's recipe with exactly its used fields maps and passes validation.
#[test]
fn every_installer_maps_with_its_used_fields() {
    for recipe in every_installer() {
        let mapped = kernel_recipe(&recipe);
        assert!(mapped.is_ok(), "{recipe:?}");
        assert_eq!(
            kernel_install::validate(&mapped.unwrap()),
            Ok(()),
            "{recipe:?}"
        );
    }
}

/// A field its installer does not use, set, is refused for every installer and field.
#[test]
fn an_unused_field_set_is_refused() {
    for recipe in every_installer() {
        let (used, _) = fields(recipe.installer);
        for field in FIELDS.into_iter().filter(|field| used & field == 0) {
            assert_eq!(
                kernel_recipe(&with_field(&recipe, field)),
                Err(TEMPLATE_MISMATCH),
                "{:?} field {field}",
                recipe.installer
            );
        }
    }
}

/// A field its installer needs, left unset, is refused for every installer and field.
#[test]
fn a_used_field_left_unset_is_refused() {
    for recipe in every_installer() {
        let (_, needed) = fields(recipe.installer);
        for field in FIELDS.into_iter().filter(|field| needed & field != 0) {
            assert_eq!(
                kernel_recipe(&without_field(&recipe, field)),
                Err(TEMPLATE_MISMATCH),
                "{:?} field {field}",
                recipe.installer
            );
        }
    }
}

/// An npm recipe with no version is the bare spec, the only used field left unset that
/// maps; an empty version is refused, so the bare spec has one form.
#[test]
fn an_npm_recipe_with_no_version_is_the_bare_spec() {
    let npm = InstallRecipe {
        name: "@openai/codex".to_owned(),
        verb: Some(InstallVerb::Update),
        ..empty(Installer::NpmGlobal)
    };
    assert_eq!(
        kernel_recipe(&npm),
        Ok(Recipe::NpmGlobal {
            package: "@openai/codex".to_owned(),
            version: String::new(),
            verb: kernel_install::Verb::Update,
            flags: Vec::new(),
        })
    );
    let blank = InstallRecipe {
        version: some(""),
        ..npm
    };
    assert_eq!(kernel_recipe(&blank), Err(TEMPLATE_MISMATCH));
}

/// The executor gets the mapped recipe and nothing else from the call, so no field a plugin
/// sends picks a download source, an archive member or any path; an address in a recipe
/// field is refused before any executor runs.
#[test]
fn a_plugin_recipe_cannot_steer_source_or_member() {
    let steering = [
        InstallRecipe {
            version: some("0.13.5"),
            target: some("file:///tmp/evil.tar.gz"),
            sha256: some(HASH),
            ..empty(Installer::UvRelease)
        },
        InstallRecipe {
            version: some("0.13.5"),
            target: some("aarch64-apple-darwin"),
            sha256: some("https://example.test/uv.tar.gz"),
            ..empty(Installer::UvRelease)
        },
        InstallRecipe {
            version: some("../../uv"),
            target: some("aarch64-apple-darwin"),
            sha256: some(HASH),
            ..empty(Installer::NodeArchive)
        },
        InstallRecipe {
            name: "x".to_owned(),
            version: some("0.13.5"),
            target: some("aarch64-apple-darwin"),
            sha256: some(HASH),
            ..empty(Installer::UvRelease)
        },
    ];
    let refusing = InstallCaps::with(never_run);
    for recipe in steering {
        assert!(
            matches!(
                refusing.install("test-plugin", recipe.clone()),
                Err(Refusal::Rejected { .. })
            ),
            "{recipe:?}"
        );
    }
    SEEN.lock().unwrap().clear();
    let recipe = every_installer().remove(0);
    let provider = InstallCaps::with(recorded);
    assert!(provider.install("test-plugin", recipe.clone()).is_ok());
    assert_eq!(*SEEN.lock().unwrap(), vec![kernel_recipe(&recipe).unwrap()]);
}

/// With no executor registered, every recipe, valid or not, is refused and nothing runs.
#[test]
fn the_unregistered_provider_refuses_every_call() {
    let provider = InstallCaps::UNREGISTERED;
    for recipe in every_installer() {
        assert_eq!(
            provider.install("test-plugin", recipe),
            Err(Refusal::Failed {
                code: INSTALLER_MISSING.to_owned(),
            })
        );
    }
}

/// Every run value and engine value of at least `ENV_VALUE_MIN` bytes is redacted, and a
/// shorter one is kept.
#[test]
fn run_and_engine_values_are_redacted() {
    let ran = Ran {
        code: 1,
        output: b"key ghp_secret1 at /e/setup/npm, port 8080".to_vec(),
        env: vec![OsString::from("/e/setup/npm"), OsString::from("8080")],
    };
    let result = bounded(ran, &[OsString::from("ghp_secret1")]);
    assert_eq!(result.code, 1);
    assert_eq!(
        result.output,
        b"key [REDACTED] at [REDACTED], port 8080".to_vec()
    );
}

/// An output read past `OUTPUT_MAX`, and one a redaction grows past it, are each cut to
/// `OUTPUT_MAX`.
#[test]
fn the_output_is_bounded_before_and_after_redaction() {
    let long = Ran {
        code: 0,
        output: vec![b'a'; exposure::OUTPUT_MAX + 7],
        env: Vec::new(),
    };
    assert_eq!(bounded(long, &[]).output.len(), exposure::OUTPUT_MAX);
    let value = "v".repeat(exposure::ENV_VALUE_MIN);
    let filled = format!("{value}|").repeat(exposure::OUTPUT_MAX / (exposure::ENV_VALUE_MIN + 1));
    let grows = Ran {
        code: 0,
        output: filled.into_bytes(),
        env: vec![OsString::from(&value)],
    };
    let output = bounded(grows, &[]).output;
    assert_eq!(output.len(), exposure::OUTPUT_MAX);
    assert!(
        !output
            .windows(value.len())
            .any(|window| window == value.as_bytes())
    );
}

/// No bundled plugin holds the `process.install` permission.
#[test]
fn no_bundled_plugin_holds_process_install() {
    assert!(
        BUNDLED_GRANTS
            .iter()
            .all(|grant| !grant.permissions.contains(&process::INSTALL.needs))
    );
}

/// The bundled agent-quit plugin calling `process.install` is refused by the gateway and
/// logged once, and the provider is never reached.
#[test]
fn agent_quit_calling_process_install_is_refused_and_logs_one_call_refused() {
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(bundled_grants(), &refusals, &AllOn);
    let provider = InstallCaps::with(never_run);
    let providers = Providers {
        editor: &crate::security_caps::Absent,
        net: &crate::security_caps::Absent,
        process: &provider,
        secrets: &crate::security_caps::Absent,
        settings: &crate::security_caps::Absent,
        service: &crate::security_caps::Absent,
        update: &crate::security_caps::Absent,
    };
    let plugin = BUNDLED_GRANTS[0].plugin;
    let host = Bound::new(gateway.bind(plugin), &providers);
    let result = process::install(&host, every_installer().remove(0));
    assert_eq!(
        result,
        Err(Refusal::NotGranted {
            capability: process::INSTALL.name,
            needs: process::INSTALL.needs,
        })
    );
    assert_eq!(refusals.count(plugin), 1);
}

/// A test grant table holding `process.install` reaches the provider, and the plugin gets
/// the executor's bounded result.
#[test]
fn a_test_grant_table_reaches_the_install_provider() {
    let grants = Grants::new(&[("installer-test", Tier::T1, &["process.install"])]).unwrap();
    let refusals = EngineRefusals::new();
    let gateway = Gateway::new(&grants, &refusals, &AllOn);
    let provider = InstallCaps::with(answered);
    let providers = Providers {
        editor: &crate::security_caps::Absent,
        net: &crate::security_caps::Absent,
        process: &provider,
        secrets: &crate::security_caps::Absent,
        settings: &crate::security_caps::Absent,
        service: &crate::security_caps::Absent,
        update: &crate::security_caps::Absent,
    };
    let host = Bound::new(gateway.bind("installer-test"), &providers);
    assert_eq!(
        process::install(&host, every_installer().remove(3)),
        Ok(InstallResult {
            code: 0,
            output: b"done".to_vec(),
        })
    );
    assert_eq!(refusals.count("installer-test"), 0);
}
