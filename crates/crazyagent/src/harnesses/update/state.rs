use super::run::run_raw;
use super::{
    BREW_PREFIXES, BUNDLED, CHECK_TIME, CheckWhen, Fixed, HarnessNeeds, HarnessVersions,
    InstallFromApp, LOOKUP_FAILED, LastUpdate, Latest, Method, OnlineLookup, Pointer, Ran, Roots,
    START_FAILED, UPDATE_TIME, UpdateMode, UpdateSettings, VERSION_TIME, check_command, guide,
    install_command, installed_version, latest, latest_from_pointer, lookup, method, pointer, run,
    shown, update_command,
};
use crate::engine::EngineError;
use crate::harnesses::{self, HarnessEntry};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

pub const UPDATE_OFF: &str = "harness_update_off";
pub const CHECK_ONLY: &str = "harness_update_check_only";
pub const BUSY: &str = "harness_update_busy";
pub const NOT_INSTALLED: &str = "harness_update_not_installed";
pub const UNKNOWN_HARNESS: &str = "harness_update_unknown_harness";
pub const UPDATE_FAILED: &str = "harness_update_failed";
pub const CHECK_FAILED: &str = "harness_update_check_failed";
pub const SETTINGS_UNREADABLE: &str = "harness_update_settings_unreadable";
pub const INSTALL_OFF: &str = "harness_install_off";
pub const INSTALL_ALREADY: &str = "harness_install_already";
pub const INSTALL_NOT_FOUND: &str = "harness_install_not_found";
pub const INSTALL_FAILED: &str = "harness_install_failed";
pub const INSTALL_LIST_INVALID: &str = "harness_install_list_invalid";

const NO_VERSION: &str = "0";
const VERSION_ARGS: [&str; 1] = ["--version"];
const JOB_THREAD: &str = "harness-update";
const CLOSED: UpdateSettings = UpdateSettings {
    mode: UpdateMode::Off,
    when: CheckWhen::OnButton,
    online: OnlineLookup::Off,
    install: InstallFromApp::Off,
};

const TABLE: &str = "CREATE TABLE IF NOT EXISTS harness_update_settings (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    mode TEXT NOT NULL CHECK(mode IN ('check_and_update', 'check_only', 'off')),
    check_when TEXT NOT NULL CHECK(check_when IN ('on_button', 'on_open')),
    online TEXT NOT NULL DEFAULT 'on' CHECK(online IN ('on', 'off')),
    install TEXT NOT NULL DEFAULT 'on' CHECK(install IN ('on', 'off'))
)";

fn unreadable() -> EngineError {
    EngineError(SETTINGS_UNREADABLE.into())
}

/// Creates the harness update settings table when it is missing.
/// Input: the engine database. Output: Ok, or `harness_update_settings_unreadable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(TABLE).map_err(|_| unreadable())
}

/// The stored name of one choice. Input: the choice. Output: its snake case name.
fn text(choice: impl Serialize) -> Result<String, EngineError> {
    match serde_json::to_value(choice) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        _ => Err(unreadable()),
    }
}

/// The choice a stored name stands for. Input: the name. Output: the choice, or
/// `harness_update_settings_unreadable` for a name this engine does not know.
fn choice<T: DeserializeOwned>(name: String) -> Result<T, EngineError> {
    serde_json::from_value(serde_json::Value::String(name)).map_err(|_| unreadable())
}

/// Loads the owner's harness update settings. Input: the engine database with the table in
/// place. Output: the stored settings, every default before the first save, or
/// `harness_update_settings_unreadable` so the engine acts on no guess.
pub fn settings(db: &Connection) -> Result<UpdateSettings, EngineError> {
    let stored: Option<(String, String, String, String)> = db
        .query_row(
            "SELECT mode,check_when,online,install FROM harness_update_settings WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|_| unreadable())?;
    let Some((mode, when, online, install)) = stored else {
        return Ok(UpdateSettings::default());
    };
    Ok(UpdateSettings {
        mode: choice(mode)?,
        when: choice(when)?,
        online: choice(online)?,
        install: choice(install)?,
    })
}

/// Saves the owner's harness update settings over the stored ones.
/// Inputs: the engine database with the table in place and the settings.
/// Output: Ok once the one row is stored, or `harness_update_settings_unreadable`.
pub fn set_settings(db: &Connection, settings: UpdateSettings) -> Result<(), EngineError> {
    let stored = db
        .execute(
            "INSERT INTO harness_update_settings(id,mode,check_when,online,install) VALUES(1,?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET mode=excluded.mode, check_when=excluded.check_when, online=excluded.online, install=excluded.install",
            rusqlite::params![
                text(settings.mode)?,
                text(settings.when)?,
                text(settings.online)?,
                text(settings.install)?
            ],
        )
        .map_err(|_| unreadable())?;
    assert_eq!(stored, 1);
    Ok(())
}

impl Roots {
    /// The roots of this machine: the engine's own HOME, the same one the harness search
    /// uses, the usual Homebrew prefixes, casks only on macOS and the resolver's places with
    /// no engine root. Output: the roots; an unset or relative HOME is an empty home.
    pub fn system() -> Self {
        Self::on(crate::tools::Places::from_environment(None))
    }

    /// The roots of this machine with the given resolver places: the engine's HOME, the
    /// usual Homebrew prefixes, casks only on macOS and no npm yet. Input: the places.
    /// Output: the roots; an unset or relative HOME is an empty home that matches nothing.
    fn on(places: crate::tools::Places) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .unwrap_or_default();
        Self {
            home,
            brew_prefixes: BREW_PREFIXES.iter().map(PathBuf::from).collect(),
            casks: cfg!(target_os = "macos"),
            npm: None,
            places,
        }
    }

    /// The roots of this machine for one engine: [`Roots::system`] with the npm that npm
    /// harnesses install with, found by setup's one Node test on the engine's resolver
    /// places, and the engine's own prefix `<engine root>/setup/harnesses`. Nothing is run.
    /// Input: the engine root. Output: the roots; no npm when no usable Node is found.
    pub fn engine(engine_root: &Path) -> Self {
        let mut roots = Self::on(crate::tools::Places::from_environment(Some(engine_root)));
        roots.npm = engine_npm(engine_root, &roots.places);
        roots
    }
}

/// Finds the npm an npm harness installs with for one engine on the engine's own resolver
/// places. Inputs: the engine root and those places. Output: the npm, or none with no
/// usable Node.
#[cfg(unix)]
fn engine_npm(engine_root: &Path, places: &crate::tools::Places) -> Option<super::NpmInstall> {
    engine_npm_at(engine_root, places)
}

/// Finds the npm an npm harness installs with: the Node setup's one test finds on the places
/// and the engine prefix, named by its real path when it exists so an install found inside
/// it matches. Inputs: the engine root and the places. Output: the npm, or none with no
/// usable Node.
#[cfg(unix)]
pub(crate) fn engine_npm_at(
    engine_root: &Path,
    places: &crate::tools::Places,
) -> Option<super::NpmInstall> {
    let found = crate::setup::node::usable(places)?;
    let named = crate::setup::harness_prefix(engine_root);
    let prefix = named.canonicalize().unwrap_or(named);
    let node_bin = found.node.parent()?.to_path_buf();
    Some(super::NpmInstall {
        node: found.node,
        program: found.npm,
        search: vec![node_bin, prefix.join("bin")],
        prefix,
    })
}

/// Finds no npm where setup has no Node. Inputs: the engine root and the places, unused.
/// Output: none.
#[cfg(not(unix))]
fn engine_npm(_engine_root: &Path, _places: &crate::tools::Places) -> Option<super::NpmInstall> {
    None
}

/// Makes the engine prefix private before an npm install into it, so npm never makes it
/// with the umask's modes. Inputs: the install command and the roots. Output: Ok, or
/// `harness_install_failed` when the prefix can't be made private.
#[cfg(unix)]
fn ready_prefix(fixed: &Fixed, roots: &Roots) -> Result<(), &'static str> {
    match (&fixed.prefix, &roots.npm) {
        (Some(prefix), Some(engine)) if *prefix == engine.prefix => {
            crate::setup::node::make_prefix(prefix, &engine.node).map_err(|_| INSTALL_FAILED)
        }
        _ => Ok(()),
    }
}

/// Makes nothing where npm installs never run. Inputs: unused. Output: Ok.
#[cfg(not(unix))]
fn ready_prefix(_fixed: &Fixed, _roots: &Roots) -> Result<(), &'static str> {
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn failed(code: &str) -> Latest {
    Latest::Failed { code: code.into() }
}

/// Asks a harness for its version with `--version`, bounded like every other run.
/// Inputs: the harness executable and the home folder. Output: the version, None when the
/// harness cannot be asked or prints no plain version.
pub(super) fn version_of(executable: &Path, home: &Path) -> Option<String> {
    if !executable.is_absolute() {
        return None;
    }
    let fixed = Fixed {
        program: executable.to_path_buf(),
        args: VERSION_ARGS.to_vec(),
        search: vec![executable.parent()?.to_path_buf()],
        prefix: None,
    };
    let raw = run_raw(&fixed, home, VERSION_TIME).ok()?;
    installed_version(&String::from_utf8_lossy(&raw.stdout))
        .or_else(|| installed_version(&String::from_utf8_lossy(&raw.stderr)))
}

/// Asks the tool that installed a harness whether a newer version exists. Homebrew reports
/// an outdated named package with a failing exit, so the output decides before the exit does.
/// Inputs: the harness id, its method, its real path, the roots and the installed version.
/// Output: the check result; a refusal or a failed tool is `Failed` with its code.
fn tool_check(id: &str, method: &Method, real: &Path, roots: &Roots, installed: &str) -> Latest {
    let fixed = match check_command(id, real, roots) {
        Ok(Some(fixed)) => fixed,
        Ok(None) => return Latest::NotChecked,
        Err(code) => return failed(code),
    };
    match run_raw(&fixed, &roots.home, CHECK_TIME) {
        Ok(raw) => {
            let found = latest(method, &String::from_utf8_lossy(&raw.stdout), installed);
            if !raw.success && matches!(found, Latest::Failed { .. }) {
                failed(CHECK_FAILED)
            } else {
                found
            }
        }
        Err(code) => failed(code),
    }
}

/// What an installed harness lacks before it can run, read as a launch reads it: the program
/// its first line asks for when the tool resolver finds none, with the resolver's owner line.
/// Any other read leaves the file to run as it is. Nothing is run. Inputs: the catalog's file
/// and the resolver's places. Output: the need, or none.
pub(crate) fn needs(executable: &Path, places: &crate::tools::Places) -> Option<HarnessNeeds> {
    let missing = crate::tools::resolve_entry(executable, places).err()?;
    (missing.reason == crate::tools::Reason::NeedsInterpreter).then(|| HarnessNeeds {
        line: missing.line(),
        program: missing.needs,
    })
}

/// The versions row of a harness that is not installed: the exact install line when the
/// setting offers Install and a trusted tool can install it, the reason when not, and
/// always the vendor's guide. No process is started.
/// Inputs: the catalog id, the roots and the settings. Output: the row.
fn missing(id: &str, roots: &Roots, settings: UpdateSettings) -> HarnessVersions {
    let line = match settings.install {
        InstallFromApp::On => install_command(id, roots).map(|fixed| shown(&fixed)),
        InstallFromApp::Off => Err(INSTALL_OFF),
    };
    let (install, refusal) = match line {
        Ok(line) => (Some(line), NOT_INSTALLED),
        Err(code) => (None, code),
    };
    HarnessVersions {
        id: id.to_owned(),
        method: Method::Unknown,
        installed: None,
        latest: Latest::NotChecked,
        command: None,
        install,
        refusal: Some(refusal.to_owned()),
        guide: guide(id).map(str::to_owned),
        running: false,
        last: None,
        needs: None,
        sign_in_min: None,
    }
}

/// Turns the end of an update or install run into its record.
/// Inputs: how the run ended, the code for a tool that failed, and the versions before and
/// after. Output: the record, with the masked last lines when the tool ran.
fn outcome(
    ran: Result<Ran, &'static str>,
    failure: &'static str,
    from: Option<String>,
    to: Option<String>,
) -> LastUpdate {
    let (ok, code, output) = match ran {
        Ok(ran) if ran.success => (true, None, ran.output),
        Ok(ran) => (false, Some(failure), ran.output),
        Err(code) => (false, Some(code), String::new()),
    };
    assert!(ok == code.is_none());
    LastUpdate {
        ok,
        code: code.map(str::to_owned),
        output,
        from,
        to,
        ended_ms: now_ms(),
    }
}

/// Runs one update and reads the harness's version before and after it.
/// Inputs: the update command, the harness executable and the home folder.
/// Output: the record of how it ended.
fn updated(fixed: &Fixed, executable: &Path, home: &Path) -> LastUpdate {
    let from = version_of(executable, home);
    let ran = run(fixed, home, UPDATE_TIME);
    let to = version_of(executable, home);
    outcome(ran, UPDATE_FAILED, from, to)
}

/// Runs one install, then looks for the harness with the caller's search and reads its
/// version. Inputs: the install command, the home folder and the search.
/// Output: the record; an install that ends well but leaves no harness is not found.
fn installed_now(
    fixed: &Fixed,
    home: &Path,
    after: impl FnOnce() -> Option<PathBuf>,
) -> LastUpdate {
    assert!(fixed.args.first() == Some(&"install"));
    let ran = match run(fixed, home, UPDATE_TIME) {
        Ok(ran) if ran.success => ran,
        other => return outcome(other, INSTALL_FAILED, None, None),
    };
    match after() {
        Some(found) => outcome(Ok(ran), INSTALL_FAILED, None, version_of(&found, home)),
        None => LastUpdate {
            ok: false,
            code: Some(INSTALL_NOT_FOUND.to_owned()),
            output: ran.output,
            from: None,
            to: None,
            ended_ms: now_ms(),
        },
    }
}

type Reader = Box<dyn Fn(&Pointer) -> Result<Vec<u8>, &'static str> + Send + Sync>;

#[derive(Default)]
struct State {
    running: Option<&'static str>,
    queued: Vec<&'static str>,
    last: HashMap<&'static str, LastUpdate>,
    latest: HashMap<&'static str, Latest>,
}

/// The engine's harness update state: the one update or install that may run at a time, how
/// the last one of each harness ended, and the last check result of each harness. Keys are
/// catalog ids only, so each map holds at most one entry per catalog harness.
pub struct Updates {
    state: Mutex<State>,
    reader: Reader,
}

/// The one running job's hold on the job slot; dropping it frees the slot on every path.
struct Running {
    updates: Arc<Updates>,
    id: &'static str,
}

impl Running {
    /// Stores how the job ended and forgets the harness's last check result, then frees the
    /// slot. Input: the record. Output: none.
    fn finish(self, last: LastUpdate) {
        let mut state = self.updates.state();
        assert!(state.running == Some(self.id));
        state.last.insert(self.id, last);
        state.latest.remove(self.id);
        assert!(state.last.len() <= harnesses::specs().len());
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut state = self.updates.state();
        if state.running == Some(self.id) {
            state.running = None;
        }
        assert!(state.running != Some(self.id));
    }
}

impl Updates {
    /// Builds the engine's update state, reading vendor pointers over the network.
    /// Output: the shared state with nothing running and nothing checked.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            reader: Box::new(lookup),
        })
    }

    /// Builds the update state with a stand-in for the network read, for tests.
    /// Input: the reader a pointer lookup calls. Output: the shared state.
    #[cfg(all(test, unix))]
    pub(super) fn with_reader(reader: Reader) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            reader,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reports whether no update or install is running. Output: true when the slot is free.
    pub fn idle(&self) -> bool {
        self.state().running.is_none()
    }

    /// Takes the one job slot for a harness. Input: its catalog id.
    /// Output: the hold, or `harness_update_busy` while any update or install runs.
    fn begin(self: &Arc<Self>, id: &'static str) -> Result<Running, &'static str> {
        let mut state = self.state();
        if state.running.is_some() {
            return Err(BUSY);
        }
        state.running = Some(id);
        assert!(state.running == Some(id));
        Ok(Running {
            updates: self.clone(),
            id,
        })
    }

    /// Starts the job thread: it does the work, stores the record, frees the slot, then
    /// runs the follow-up with whether the work ended well. Inputs: the hold, the work and
    /// the follow-up. Output: Ok once the thread runs; a thread that cannot start frees
    /// the slot.
    fn spawn(
        running: Running,
        work: impl FnOnce() -> LastUpdate + Send + 'static,
        then: impl FnOnce(bool) + Send + 'static,
    ) -> Result<(), &'static str> {
        std::thread::Builder::new()
            .name(JOB_THREAD.into())
            .spawn(move || {
                let last = work();
                let ok = last.ok;
                assert!(ok == last.code.is_none());
                running.finish(last);
                then(ok);
            })
            .map(|_| ())
            .map_err(|_| START_FAILED)
    }

    /// Reads a self-updating harness's vendor pointer and compares it with the installed
    /// version. Inputs: the catalog id, its method and the installed version.
    /// Output: the check result; a failed read is `Failed` with the lookup code.
    fn pointed(&self, id: &str, method: &Method, installed: &str) -> Latest {
        assert!(*method == Method::SelfUpdate);
        let Some(pointer) = pointer(id) else {
            return Latest::NotChecked;
        };
        assert!(pointer.url.starts_with("https://"));
        match (self.reader)(pointer) {
            Ok(body) => latest_from_pointer(pointer, &body, installed),
            Err(_) => failed(LOOKUP_FAILED),
        }
    }

    /// Checks one installed harness for a newer version, by its install method: the
    /// installing tool for Homebrew and npm, the vendor pointer for a self-updating install
    /// with online lookups on. Inputs: the id, method, real path, roots, settings and the
    /// installed version. Output: the check result.
    fn looked(
        &self,
        id: &str,
        method: &Method,
        real: &Path,
        roots: &Roots,
        settings: UpdateSettings,
        installed: &str,
    ) -> Latest {
        match method {
            Method::Homebrew { .. } | Method::HomebrewFormula { .. } | Method::Npm { .. } => {
                tool_check(id, method, real, roots, installed)
            }
            Method::SelfUpdate if settings.online == OnlineLookup::Off => Latest::NoCheck,
            Method::SelfUpdate => self.pointed(id, method, installed),
            Method::WithApp | Method::Unknown => Latest::NotChecked,
        }
    }

    /// The versions row of an installed harness: with updates on, asked for its version only
    /// when the table knows its install or CrazyAgent starts it (never a look-alike), and only
    /// once it `runs`, so one that still needs a program is neither asked nor checked.
    /// Inputs: the spec, executable, roots, settings, whether to check now and whether it runs.
    /// Output: the row without the job fields.
    fn present(
        &self,
        spec: &'static harnesses::HarnessSpec,
        executable: &Path,
        roots: &Roots,
        settings: UpdateSettings,
        look: bool,
        runs: bool,
    ) -> HarnessVersions {
        let id = spec.id;
        let real = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
        let method = method(id, &real, roots);
        let command = update_command(id, executable, &real, roots);
        let off = settings.mode == UpdateMode::Off;
        let asked = match method {
            Method::WithApp => false,
            Method::Unknown => spec.launch.is_none(),
            _ => true,
        };
        let installed = if off || !asked || !runs {
            None
        } else {
            version_of(executable, &roots.home)
        };
        let latest = if off {
            Latest::NotChecked
        } else if look && runs {
            let version = installed.as_deref().unwrap_or(NO_VERSION);
            let found = self.looked(id, &method, &real, roots, settings, version);
            let mut state = self.state();
            state.latest.insert(id, found.clone());
            assert!(state.latest.len() <= harnesses::specs().len());
            found
        } else {
            let cached = self.state().latest.get(id).cloned();
            cached.unwrap_or(Latest::NotChecked)
        };
        HarnessVersions {
            id: id.to_owned(),
            method,
            installed,
            latest,
            guide: command
                .is_err()
                .then(|| guide(id))
                .flatten()
                .map(str::to_owned),
            refusal: command.as_ref().err().map(|code| (*code).to_owned()),
            command: command.ok().map(|fixed| shown(&fixed)),
            install: None,
            running: false,
            last: None,
            needs: None,
            sign_in_min: None,
        }
    }

    /// Everything a client shows about one harness's versions. A check for a newer version
    /// runs only with `look`, and a vendor pointer is read only then, only for a
    /// self-updating install and only with online lookups on. A harness that still needs a
    /// program is never run, so the reads that wait on Node start nothing. Inputs: the engine
    /// database, the catalog entry, the roots and whether to check now. Output: the row.
    pub fn versions(
        &self,
        db: &Connection,
        entry: &HarnessEntry,
        roots: &Roots,
        look: bool,
    ) -> HarnessVersions {
        let settings = settings(db).unwrap_or(CLOSED);
        let Some(spec) = harnesses::spec(&entry.id) else {
            let mut unknown = missing(&entry.id, roots, CLOSED);
            unknown.refusal = Some(UNKNOWN_HARNESS.to_owned());
            return unknown;
        };
        let mut versions = match &entry.executable {
            Some(executable) => {
                let needs = needs(executable, &roots.places);
                let runs = needs.is_none();
                HarnessVersions {
                    needs,
                    ..self.present(spec, executable, roots, settings, look, runs)
                }
            }
            None => missing(spec.id, roots, settings),
        };
        assert!(versions.needs.is_none() || versions.installed.is_none());
        assert!(versions.needs.is_none() || entry.executable.is_some());
        let state = self.state();
        versions.running = state.running == Some(spec.id);
        versions.last = state.last.get(spec.id).cloned();
        versions.sign_in_min = harnesses::sign_in_min(spec.id).map(str::to_owned);
        versions
    }

    /// Starts the one update of an installed harness in the background, with the harness's
    /// own documented updater. Inputs: the engine database, the catalog entry, the roots and
    /// what to do once the update ended, told whether it ended well. Output: Ok once it
    /// runs, or the refusal code.
    pub fn start(
        self: &Arc<Self>,
        db: &Connection,
        entry: &HarnessEntry,
        roots: &Roots,
        after: impl FnOnce(bool) + Send + 'static,
    ) -> Result<(), &'static str> {
        let id = harnesses::spec(&entry.id).ok_or(UNKNOWN_HARNESS)?.id;
        match settings(db).map_err(|_| SETTINGS_UNREADABLE)?.mode {
            UpdateMode::Off => return Err(UPDATE_OFF),
            UpdateMode::CheckOnly => return Err(CHECK_ONLY),
            UpdateMode::CheckAndUpdate => {}
        }
        let executable = entry.executable.clone().ok_or(NOT_INSTALLED)?;
        let running = self.begin(id)?;
        let real = std::fs::canonicalize(&executable).unwrap_or_else(|_| executable.clone());
        let fixed = update_command(id, &executable, &real, roots)?;
        let home = roots.home.clone();
        Self::spawn(running, move || updated(&fixed, &executable, &home), after)
    }

    /// Starts the one install of a harness that is not installed, in the background, with
    /// Homebrew or npm and the table's package. Inputs: the engine database, the catalog
    /// entry, the roots and the search that finds the harness after the install.
    /// Output: Ok once it runs, or the refusal code.
    pub fn install(
        self: &Arc<Self>,
        db: &Connection,
        entry: &HarnessEntry,
        roots: &Roots,
        after: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    ) -> Result<(), &'static str> {
        let id = harnesses::spec(&entry.id).ok_or(UNKNOWN_HARNESS)?.id;
        if entry.executable.is_some() {
            return Err(INSTALL_ALREADY);
        }
        if settings(db).map_err(|_| SETTINGS_UNREADABLE)?.install == InstallFromApp::Off {
            return Err(INSTALL_OFF);
        }
        let running = self.begin(id)?;
        let fixed = install_command(id, roots)?;
        ready_prefix(&fixed, roots)?;
        let home = roots.home.clone();
        Self::spawn(running, move || installed_now(&fixed, &home, after), |_| {})
    }

    /// The harnesses waiting in the install queue, in the order they install; the one
    /// installing now is no longer listed. Output: their catalog ids.
    pub fn queued(&self) -> Vec<String> {
        let state = self.state();
        assert!(state.queued.len() <= harnesses::specs().len());
        state.queued.iter().map(|id| (*id).to_owned()).collect()
    }

    /// Puts a list of harnesses in the install queue: each a catalog harness once, never the
    /// RCLI that comes with the app, at most every catalog harness, while Install from the app
    /// is on and no other list waits. Inputs: the engine database and the ids, aliases
    /// allowed. Output: the catalog ids in install order, or the refusal code.
    pub fn queue(
        &self,
        db: &Connection,
        ids: &[String],
    ) -> Result<Vec<&'static str>, &'static str> {
        if settings(db).map_err(|_| SETTINGS_UNREADABLE)?.install == InstallFromApp::Off {
            return Err(INSTALL_OFF);
        }
        if ids.is_empty() || ids.len() > harnesses::specs().len() {
            return Err(INSTALL_LIST_INVALID);
        }
        let mut picked: Vec<&'static str> = Vec::with_capacity(ids.len());
        for id in ids {
            let spec = harnesses::spec(id).ok_or(INSTALL_LIST_INVALID)?;
            if spec.id == BUNDLED || picked.contains(&spec.id) {
                return Err(INSTALL_LIST_INVALID);
            }
            picked.push(spec.id);
        }
        let mut state = self.state();
        if !state.queued.is_empty() {
            return Err(BUSY);
        }
        state.queued.clone_from(&picked);
        assert!(state.queued.len() == ids.len());
        Ok(picked)
    }

    /// Installs the queued harnesses one after another: each starts only once no update or
    /// install runs, a refusal or a wait past `wait_max` becomes that harness's last record,
    /// and the rest still install. It drains only the list queued when it began, so a list
    /// queued after that belongs to its own drain. Inputs: what starts one install, the pause
    /// between looks and the longest wait for the job slot. Output: none.
    pub fn drain(
        self: &Arc<Self>,
        mut start: impl FnMut(&'static str) -> Result<(), &'static str>,
        pause: Duration,
        wait_max: Duration,
    ) {
        assert!(!pause.is_zero() && pause <= wait_max);
        let looks = wait_max.as_millis() / pause.as_millis().max(1);
        let taken = self.state().queued.len();
        assert!(taken <= harnesses::specs().len());
        for _ in 0..taken {
            let Some(id) = self.next_queued() else {
                break;
            };
            let mut result = Err(BUSY);
            for _ in 0..=looks {
                if self.idle() {
                    result = start(id);
                    if result != Err(BUSY) {
                        break;
                    }
                }
                std::thread::sleep(pause);
            }
            self.dequeue(id, result.err());
        }
    }

    /// The next harness on the install queue, left on it until its install started or was
    /// refused, so a read never finds it neither queued nor running. Output: its catalog id,
    /// none when the queue is empty.
    fn next_queued(&self) -> Option<&'static str> {
        self.state().queued.first().copied()
    }

    /// Takes a harness off the front of the install queue once its install started, and in
    /// the same step records a refusal as its last record. Inputs: the catalog id and the
    /// refusal code, none when the install started.
    fn dequeue(&self, id: &'static str, refusal: Option<&'static str>) {
        let mut state = self.state();
        assert_eq!(state.queued.first().copied(), Some(id));
        if let Some(code) = refusal {
            state.last.insert(
                id,
                LastUpdate {
                    ok: false,
                    code: Some(code.to_owned()),
                    output: String::new(),
                    from: None,
                    to: None,
                    ended_ms: now_ms(),
                },
            );
        }
        state.queued.remove(0);
        assert!(state.last.len() <= harnesses::specs().len());
    }
}
