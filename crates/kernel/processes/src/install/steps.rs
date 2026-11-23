use super::{InstallRecipe, SystemManager, TEMPLATE_MISMATCH, Verb};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The curl every download runs.
pub const CURL: &str = "/usr/bin/curl";
/// The tar every archive unpack runs.
pub const TAR: &str = "/usr/bin/tar";
/// The most bytes one download may fetch.
pub const ARCHIVE_BYTES_MAX: u64 = 128 * 1024 * 1024;
/// The seconds curl may spend on one download.
pub const DOWNLOAD_SECS: u64 = 300;
/// The most bytes of a download source.
pub const SOURCE_BYTES_MAX: usize = 2048;
/// The most bytes of an archive member.
pub const MEMBER_BYTES_MAX: usize = 256;

const UV: &str = if cfg!(windows) { "uv.exe" } else { "uv" };
const UV_RELEASE_PROGRAM: &str = "uv";
const WINGET: &str = "winget.exe";

const PROGRAM: u16 = 1;
const ARCHIVE: u16 = 1 << 1;
const OUT: u16 = 1 << 2;
const SOURCE: u16 = 1 << 3;
const MEMBER: u16 = 1 << 4;
const VENV: u16 = 1 << 5;
const VENV_PYTHON: u16 = 1 << 6;
const REQUIREMENTS: u16 = 1 << 7;
const NPM_PREFIX: u16 = 1 << 8;
const DOWNLOAD: u16 = ARCHIVE | OUT | SOURCE;

/// The folders and files the engine picks for one install run, and the download source and
/// archive member from its pins. Every field a variant does not use is unset. The engine
/// sets them from its own setup layout, never from a plugin, and every path is absolute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPaths {
    pub program: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub source: Option<String>,
    pub member: Option<String>,
    pub venv: Option<PathBuf>,
    pub venv_python: Option<PathBuf>,
    pub requirements: Option<PathBuf>,
    pub npm_prefix: Option<PathBuf>,
}

/// One program run of an install: the absolute program, its arguments in order, and the
/// environment the template fixes on top of the caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(&'static str, OsString)>,
}

/// Builds the runs of one recipe from its variant's fixed template, after the recipe passes
/// validation, the paths are exactly the ones the variant uses, the program is the
/// variant's installer, and the source and member pass today's pin checks. Panics on a
/// relative engine path. Inputs: the recipe and the engine's paths for this run. Output:
/// the steps in order, or the refusal code.
pub fn steps(recipe: &InstallRecipe, paths: &InstallPaths) -> Result<Vec<Step>, &'static str> {
    super::validate(recipe)?;
    absolute(paths);
    if given(paths) != needed(recipe) {
        return Err(TEMPLATE_MISMATCH);
    }
    let built = match recipe {
        InstallRecipe::UvRelease { .. } => {
            let member = paths.member.as_deref().unwrap_or_default();
            if !plain_member(member) {
                return Err(TEMPLATE_MISMATCH);
            }
            fetched(paths, Some(member))?
        }
        InstallRecipe::NodeArchive { .. } => fetched(paths, None)?,
        InstallRecipe::UvTool { python, .. } => {
            let uv = installer(paths, UV)?;
            let venv = paths.venv.as_deref().unwrap_or(Path::new(""));
            let venv_python = paths.venv_python.as_deref().unwrap_or(Path::new(""));
            let requirements = paths.requirements.as_deref().unwrap_or(Path::new(""));
            vec![
                step(
                    uv.clone(),
                    vec![
                        "venv".into(),
                        "--no-project".into(),
                        "--no-config".into(),
                        "--managed-python".into(),
                        "--python".into(),
                        python.into(),
                        venv.into(),
                    ],
                ),
                step(
                    uv,
                    vec![
                        "pip".into(),
                        "install".into(),
                        "--no-config".into(),
                        "--python".into(),
                        venv_python.into(),
                        "--require-hashes".into(),
                        "--no-build".into(),
                        "--requirements".into(),
                        requirements.into(),
                    ],
                ),
            ]
        }
        InstallRecipe::BrewFormula { name, verb } => brew(paths, *verb, "--formula", name)?,
        InstallRecipe::BrewCask { name, verb } => brew(paths, *verb, "--cask", name)?,
        InstallRecipe::NpmGlobal {
            package,
            version,
            flags,
            ..
        } => {
            let npm = installer(paths, "npm")?;
            let spec = if version.is_empty() {
                package.clone()
            } else {
                format!("{package}@{version}")
            };
            let mut args: Vec<OsString> = vec!["install".into(), "-g".into()];
            args.extend(flags.iter().map(OsString::from));
            args.push(spec.into());
            let prefix = paths.npm_prefix.clone().unwrap_or_default();
            vec![Step {
                program: npm,
                args,
                env: vec![("npm_config_prefix", prefix.into_os_string())],
            }]
        }
        InstallRecipe::Winget { id, version } => vec![step(
            installer(paths, WINGET)?,
            [
                "install",
                "--id",
                id,
                "--exact",
                "--version",
                version,
                "--source",
                "winget",
                "--scope",
                "user",
                "--silent",
                "--disable-interactivity",
                "--accept-package-agreements",
                "--accept-source-agreements",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        )],
        InstallRecipe::SystemPackage {
            manager: SystemManager::AptGet,
            package,
        } => vec![Step {
            program: installer(paths, "apt-get")?,
            args: ["install", "-y", "--no-install-recommends", package]
                .into_iter()
                .map(OsString::from)
                .collect(),
            env: vec![("DEBIAN_FRONTEND", OsString::from("noninteractive"))],
        }],
        InstallRecipe::SystemPackage {
            manager: SystemManager::Dnf,
            package,
        } => vec![step(
            installer(paths, "dnf")?,
            ["install", "-y", package]
                .into_iter()
                .map(OsString::from)
                .collect(),
        )],
        InstallRecipe::SelfUpdate { program, args } => vec![step(
            installer(paths, program)?,
            args.iter().map(OsString::from).collect(),
        )],
    };
    assert!(!built.is_empty() && built.len() <= 2);
    assert!(built.iter().all(|step| step.program.is_absolute()));
    assert!(built.iter().all(|step| step.args.len() <= 14));
    Ok(built)
}

/// Runs one step outside the sandbox through the spawn gate with no input, only the base
/// and step environment, and a time bound ([`crate::run::run_tails`]): past the bound its
/// whole process group gets TERM, a grace, then KILL, and on every return the group is
/// killed. Inputs: the step, the caller's base environment, the absolute working folder and
/// the bound. Output: 0 for a success exit and 1 for any other, with the last
/// `run::OUTPUT_BYTES_MAX` bytes of standard output and of standard error, or the run's code
/// (`setup_timeout`, `setup_install_failed`).
#[cfg(unix)]
pub fn run(
    step: &Step,
    base: &[(&str, OsString)],
    cwd: &Path,
    limit: std::time::Duration,
) -> Result<super::InstallResult, &'static str> {
    assert!(step.program.is_absolute());
    assert!(cwd.is_absolute());
    let env: Vec<(&str, OsString)> = base
        .iter()
        .filter(|(name, _)| !step.env.iter().any(|(fixed, _)| fixed == name))
        .cloned()
        .chain(step.env.iter().cloned())
        .collect();
    let ran = crate::run::run_tails(&step.program, &step.args, &env, cwd, limit)
        .map_err(|crate::run::InstallError(code)| code)?;
    assert!(ran.stdout.len() <= crate::run::OUTPUT_BYTES_MAX);
    assert!(ran.stderr.len() <= crate::run::OUTPUT_BYTES_MAX);
    Ok(super::InstallResult {
        code: u32::from(!ran.success),
        output: ran.stdout,
        stderr: ran.stderr,
    })
}

fn step(program: PathBuf, args: Vec<OsString>) -> Step {
    Step {
        program,
        args,
        env: Vec::new(),
    }
}

/// Panics on any path field that is set and relative: the engine builds these from its
/// own layout, so a relative one is an engine fault.
fn absolute(paths: &InstallPaths) {
    let fields = [
        &paths.program,
        &paths.archive,
        &paths.out,
        &paths.venv,
        &paths.venv_python,
        &paths.requirements,
        &paths.npm_prefix,
    ];
    for path in fields.into_iter().flatten() {
        assert!(path.is_absolute(), "install path is not absolute: {path:?}");
    }
}

fn given(paths: &InstallPaths) -> u16 {
    [
        (paths.program.is_some(), PROGRAM),
        (paths.archive.is_some(), ARCHIVE),
        (paths.out.is_some(), OUT),
        (paths.source.is_some(), SOURCE),
        (paths.member.is_some(), MEMBER),
        (paths.venv.is_some(), VENV),
        (paths.venv_python.is_some(), VENV_PYTHON),
        (paths.requirements.is_some(), REQUIREMENTS),
        (paths.npm_prefix.is_some(), NPM_PREFIX),
    ]
    .into_iter()
    .filter(|(set, _)| *set)
    .fold(0, |mask, (_, bit)| mask | bit)
}

fn needed(recipe: &InstallRecipe) -> u16 {
    match recipe {
        InstallRecipe::UvRelease { .. } => DOWNLOAD | MEMBER,
        InstallRecipe::NodeArchive { .. } => DOWNLOAD,
        InstallRecipe::UvTool { .. } => PROGRAM | VENV | VENV_PYTHON | REQUIREMENTS,
        InstallRecipe::NpmGlobal { .. } => PROGRAM | NPM_PREFIX,
        InstallRecipe::BrewFormula { .. }
        | InstallRecipe::BrewCask { .. }
        | InstallRecipe::Winget { .. }
        | InstallRecipe::SystemPackage { .. }
        | InstallRecipe::SelfUpdate { .. } => PROGRAM,
    }
}

/// The engine's installer program when its file name is the variant's installer. Inputs:
/// the paths and the installer's file name. Output: the program, or `install_template_mismatch`.
fn installer(paths: &InstallPaths, name: &str) -> Result<PathBuf, &'static str> {
    paths
        .program
        .clone()
        .filter(|program| program.file_name() == Some(std::ffi::OsStr::new(name)))
        .ok_or(TEMPLATE_MISMATCH)
}

fn brew(
    paths: &InstallPaths,
    verb: Verb,
    kind: &str,
    name: &str,
) -> Result<Vec<Step>, &'static str> {
    let word = match verb {
        Verb::Install => "install",
        Verb::Update => "upgrade",
    };
    Ok(vec![step(
        installer(paths, "brew")?,
        vec![word.into(), kind.into(), name.into()],
    )])
}

/// The curl download into the archive file, then the tar unpack into the out folder, with
/// the member last when one is named. Inputs: the paths and the member. Output: the two
/// steps, or `install_template_mismatch` for a source that fails today's pin check.
fn fetched(paths: &InstallPaths, member: Option<&str>) -> Result<Vec<Step>, &'static str> {
    let source = paths.source.as_deref().unwrap_or_default();
    if !plain_source(source) {
        return Err(TEMPLATE_MISMATCH);
    }
    let archive = paths.archive.as_deref().unwrap_or(Path::new(""));
    let out = paths.out.as_deref().unwrap_or(Path::new(""));
    let mut unpack: Vec<OsString> = vec![
        "--no-same-owner".into(),
        "--no-same-permissions".into(),
        "-xzf".into(),
        archive.into(),
        "-C".into(),
        out.into(),
    ];
    unpack.extend(member.map(OsString::from));
    Ok(vec![
        step(
            PathBuf::from(CURL),
            vec![
                "-q".into(),
                "-fsSL".into(),
                "--proto".into(),
                "=https,file".into(),
                "--max-filesize".into(),
                ARCHIVE_BYTES_MAX.to_string().into(),
                "--max-time".into(),
                DOWNLOAD_SECS.to_string().into(),
                "-o".into(),
                archive.into(),
                source.into(),
            ],
        ),
        step(PathBuf::from(TAR), unpack),
    ])
}

/// Whether a download source is an https or local file address of at most
/// [`SOURCE_BYTES_MAX`] bytes with no control or space character. Input: the source.
/// Output: the answer.
fn plain_source(source: &str) -> bool {
    (source.starts_with("https://") || source.starts_with("file:///"))
        && source.len() <= SOURCE_BYTES_MAX
        && !source.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Whether a uv archive member is a relative path of plain parts, at most
/// [`MEMBER_BYTES_MAX`] bytes, not starting with `-`, whose file name is `uv`. Input: the
/// member. Output: the answer.
fn plain_member(member: &str) -> bool {
    let path = Path::new(member);
    member.len() <= MEMBER_BYTES_MAX
        && !member.starts_with('-')
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.file_name() == Some(std::ffi::OsStr::new(UV_RELEASE_PROGRAM))
}

#[cfg(all(test, unix))]
mod tests;
