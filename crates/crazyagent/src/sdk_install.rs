use kernel_processes::install::{
    self as kernel_install, InstallRecipe as Recipe, TEMPLATE_MISMATCH,
};
use kernel_security::exposure;
use sdk::Refusal;
use sdk::process::{self, InstallRecipe, InstallResult, InstallVerb, Installer, SystemManager};
use std::ffi::OsString;

#[cfg(unix)]
const _: () = assert!(kernel_processes::run::OUTPUT_BYTES_MAX <= exposure::OUTPUT_MAX);

/// An install call that reached no executor: the engine has none registered for plugins.
pub(crate) const INSTALLER_MISSING: &str = "install_installer_missing";

const NAME: u16 = 1;
const VERB: u16 = 1 << 1;
const VERSION: u16 = 1 << 2;
const TARGET: u16 = 1 << 3;
const PYTHON: u16 = 1 << 4;
const LOCK: u16 = 1 << 5;
const SHA256: u16 = 1 << 6;
const MANAGER: u16 = 1 << 7;
const ARGS: u16 = 1 << 8;
const FLAGS: u16 = 1 << 9;

/// What an executor ran for one recipe: the exit code, the output as read, and every
/// environment value the run set, which the output is redacted against.
pub(crate) struct Ran {
    pub(crate) code: u32,
    pub(crate) output: Vec<u8>,
    pub(crate) env: Vec<OsString>,
}

/// Runs one validated recipe with the engine's own paths and pins; it takes no path from
/// the call. Input: the recipe. Output: what ran, or the run's code.
pub(crate) type Executor = fn(&Recipe) -> Result<Ran, &'static str>;

/// The `process.install` provider: it maps a plugin's recipe onto the kernel recipe,
/// validates it, hands it to the engine's executor, and redacts and bounds the output.
/// With no executor registered every call is refused.
pub(crate) struct InstallCaps {
    executor: Option<Executor>,
}

impl InstallCaps {
    /// The provider the engine registers: no executor, so every call fails closed.
    pub(crate) const UNREGISTERED: InstallCaps = InstallCaps { executor: None };

    /// A provider over one executor. Input: the executor. Output: the provider.
    #[cfg(test)]
    pub(crate) const fn with(executor: Executor) -> Self {
        Self {
            executor: Some(executor),
        }
    }
}

impl process::Provide for InstallCaps {
    fn install(&self, _: &'static str, recipe: InstallRecipe) -> Result<InstallResult, Refusal> {
        let rejected = |code: &str| Refusal::Rejected {
            code: code.to_owned(),
        };
        let failed = |code: &str| Refusal::Failed {
            code: code.to_owned(),
        };
        let mapped = kernel_recipe(&recipe).map_err(rejected)?;
        kernel_install::validate(&mapped).map_err(rejected)?;
        let executor = self.executor.ok_or_else(|| failed(INSTALLER_MISSING))?;
        let ran = executor(&mapped).map_err(failed)?;
        Ok(bounded(ran, &engine_values()))
    }
}

/// Maps a plugin's recipe onto the kernel recipe. Each installer uses an exact set of
/// fields: one it uses left unset, or one it does not use set, is
/// `install_template_mismatch`. A text or list counts as set when it is not empty. An npm
/// recipe with no version is the bare spec; an empty version is refused. Input: the
/// recipe. Output: the kernel recipe, or the refusal code.
pub(crate) fn kernel_recipe(recipe: &InstallRecipe) -> Result<Recipe, &'static str> {
    let (used, needed) = fields(recipe.installer);
    let set = given(recipe);
    if set & !used != 0 || needed & !set != 0 {
        return Err(TEMPLATE_MISMATCH);
    }
    let text = |field: &Option<String>| field.clone().unwrap_or_default();
    let verb = match recipe.verb {
        Some(InstallVerb::Update) => kernel_install::Verb::Update,
        _ => kernel_install::Verb::Install,
    };
    let name = recipe.name.clone();
    Ok(match recipe.installer {
        Installer::UvRelease => Recipe::UvRelease {
            version: text(&recipe.version),
            target: text(&recipe.target),
            sha256: text(&recipe.sha256),
        },
        Installer::NodeArchive => Recipe::NodeArchive {
            version: text(&recipe.version),
            target: text(&recipe.target),
            sha256: text(&recipe.sha256),
        },
        Installer::UvTool => Recipe::UvTool {
            package: name,
            version: text(&recipe.version),
            python: text(&recipe.python),
            lock: text(&recipe.lock),
        },
        Installer::BrewFormula => Recipe::BrewFormula { name, verb },
        Installer::BrewCask => Recipe::BrewCask { name, verb },
        Installer::NpmGlobal => {
            if recipe.version.as_deref() == Some("") {
                return Err(TEMPLATE_MISMATCH);
            }
            Recipe::NpmGlobal {
                package: name,
                version: text(&recipe.version),
                verb,
                flags: recipe.flags.clone(),
            }
        }
        Installer::Winget => Recipe::Winget {
            id: name,
            version: text(&recipe.version),
        },
        Installer::SystemPackage => Recipe::SystemPackage {
            manager: match recipe.manager {
                Some(SystemManager::Dnf) => kernel_install::SystemManager::Dnf,
                _ => kernel_install::SystemManager::AptGet,
            },
            package: name,
        },
        Installer::SelfUpdate => Recipe::SelfUpdate {
            program: name,
            args: recipe.args.clone(),
        },
    })
}

/// The fields one installer may set and the ones it must set. Input: the installer.
/// Output: the two field sets.
fn fields(installer: Installer) -> (u16, u16) {
    match installer {
        Installer::UvRelease | Installer::NodeArchive => {
            let download = VERSION | TARGET | SHA256;
            (download, download)
        }
        Installer::UvTool => {
            let tool = NAME | VERSION | PYTHON | LOCK;
            (tool, tool)
        }
        Installer::BrewFormula | Installer::BrewCask => (NAME | VERB, NAME | VERB),
        Installer::NpmGlobal => (NAME | VERB | VERSION | FLAGS, NAME | VERB),
        Installer::Winget => (NAME | VERSION, NAME | VERSION),
        Installer::SystemPackage => (NAME | MANAGER, NAME | MANAGER),
        Installer::SelfUpdate => (NAME | ARGS, NAME | ARGS),
    }
}

fn given(recipe: &InstallRecipe) -> u16 {
    [
        (!recipe.name.is_empty(), NAME),
        (recipe.verb.is_some(), VERB),
        (recipe.version.is_some(), VERSION),
        (recipe.target.is_some(), TARGET),
        (recipe.python.is_some(), PYTHON),
        (recipe.lock.is_some(), LOCK),
        (recipe.sha256.is_some(), SHA256),
        (recipe.manager.is_some(), MANAGER),
        (!recipe.args.is_empty(), ARGS),
        (!recipe.flags.is_empty(), FLAGS),
    ]
    .into_iter()
    .filter(|(set, _)| *set)
    .fold(0, |mask, (_, bit)| mask | bit)
}

/// The values of the engine's own environment variables an agent may never see, such as
/// tokens and keys, which no install output may carry. Output: the values.
fn engine_values() -> Vec<OsString> {
    std::env::vars_os()
        .filter(|(name, _)| crate::sandbox::agent_env_refused(name))
        .map(|(_, value)| value)
        .collect()
}

/// The output a plugin sees: at most `exposure::OUTPUT_MAX` bytes read, every run value and
/// engine value redacted, then cut to `exposure::OUTPUT_MAX` again, since a redaction can
/// grow it. A value cut at the read edge keeps its prefix. Inputs: what ran and the engine
/// values. Output: the result.
pub(crate) fn bounded(ran: Ran, engine: &[OsString]) -> InstallResult {
    let read = &ran.output[..ran.output.len().min(exposure::OUTPUT_MAX)];
    let values: Vec<&[u8]> = ran
        .env
        .iter()
        .chain(engine)
        .map(|value| value.as_encoded_bytes())
        .collect();
    let mut output = exposure::redact(read, &[], &values);
    output.truncate(exposure::OUTPUT_MAX);
    assert!(output.len() <= exposure::OUTPUT_MAX);
    InstallResult {
        code: ran.code,
        output,
    }
}

#[cfg(test)]
mod tests;
