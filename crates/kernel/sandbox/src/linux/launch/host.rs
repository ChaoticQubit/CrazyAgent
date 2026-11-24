use super::*;

/// Runs a probe as [`passes`] does, but ends it once `time_max` has gone by; the harness
/// the call supervisor probe starts dies with it. Inputs: the probe and its longest run.
/// Output: true only when it exited 0 in time.
fn passes_within(probe: &mut Command, time_max: Duration) -> bool {
    let started = Instant::now();
    let quiet = probe
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = kernel_processes::spawn_gate::spawn(quiet) else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < time_max => std::thread::sleep(PROBE_POLL),
            Ok(None) | Err(_) => break,
        }
    }
    if child.kill().is_ok() {
        let reaped = child.wait();
        assert!(
            reaped.is_ok(),
            "a killed probe is this process's child to reap"
        );
    }
    false
}

/// Makes a folder only this user may open under a random name in `data`, the engine's data
/// folder, for one supervisor probe, so no stale, concurrent or planted folder takes its
/// name (rv-secreview linux-t4c F1). Input: the absolute data folder. Output: the new empty
/// folder, or none when no name was free or the data folder refused it.
pub(super) fn private_folder(data: &Path) -> Option<PathBuf> {
    assert!(data.is_absolute());
    for _ in 0..PROBE_FOLDER_TRIES_MAX {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).ok()?;
        let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(name.len(), 32);
        let folder = data.join(format!("{PROBE_FOLDER_PREFIX}{name}"));
        match fs::DirBuilder::new().mode(0o700).create(&folder) {
            Ok(()) => return Some(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// Tells whether the call supervisor holds every guard here as a launch starts it: the
/// helper's probe ([`probe_supervisor`]), plain or inside bubblewrap's namespaces, in a new
/// private folder under the engine's data folder ([`private_folder`]), removed after. Never
/// as root (rv-sec3 linux-t4b N1). Inputs: the helper, the bubblewrap launches nest under,
/// the data folder and whether this runs as root. Output: true only when the probe held.
pub(super) fn supervises(helper: &Path, bwrap: Option<&Path>, data: &Path, root: bool) -> bool {
    if root || !(clean_absolute(helper) && helper.is_file() && clean_absolute(data)) {
        return false;
    }
    let mut probe = Command::new(helper);
    match bwrap {
        None => {}
        Some(bwrap) if clean_absolute(bwrap) && trusted(bwrap) => {
            probe.arg(NEST).arg("--").arg(bwrap);
            probe.args(NEST_ARGS).arg("--").arg(helper);
        }
        Some(_) => return false,
    }
    let Some(folder) = private_folder(data) else {
        return false;
    };
    assert!(folder.starts_with(data));
    probe.arg(PROBE_SUPERVISOR).arg(&folder);
    let held = private_empty(&folder) && passes_within(&mut probe, SUPERVISOR_PROBE_TIME_MAX);
    if let Err(error) = fs::remove_dir_all(&folder) {
        eprintln!(
            "linux_supervisor_probe_folder_kept {}: {error}",
            folder.display()
        );
    }
    held
}

/// Runs a probe ([`probe_said`]) and, when it failed, writes what it said to `log` as one
/// line ([`probe_log`]). A log that cannot take the line is passed over, since the refusal
/// still names the reason. Inputs: the probe command and the log, the engine's stderr.
/// Output: none when it ran and exited 0, else its reason ([`probe_reason`]).
pub(super) fn probe_failure(probe: &mut Command, log: &mut impl Write) -> Option<String> {
    let (said, ended) = probe_said(probe)?;
    let flags: Vec<&OsStr> = probe.get_args().collect();
    let _ = writeln!(log, "{}", probe_log(&flags, &ended, &said));
    Some(probe_reason(&said, &ended))
}

/// Runs a probe with no environment and no input, keeping at most [`PROBE_SAID_MAX`] bytes of
/// what it says on stderr. The pipe closes before the wait, so a probe that says more gets
/// `EPIPE`, or `SIGPIPE` where it keeps that signal's default, and ends rather than blocking.
/// Input: the probe command. Output: none when it ran and exited 0, else its stderr and how
/// it ended.
pub(super) fn probe_said(probe: &mut Command) -> Option<(String, String)> {
    let started = kernel_processes::spawn_gate::spawn(
        probe
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let mut child = match started {
        Ok(child) => child,
        Err(error) => return Some((String::new(), error.to_string())),
    };
    let mut bytes = Vec::new();
    let read = child
        .stderr
        .take()
        .map(|pipe| pipe.take(PROBE_SAID_MAX as u64).read_to_end(&mut bytes));
    let status = match child.wait() {
        Ok(status) if status.success() => return None,
        Ok(status) => status.to_string(),
        Err(error) => error.to_string(),
    };
    let ended = match read {
        Some(Err(error)) => format!("{status}, stderr unread: {error}"),
        Some(Ok(_)) | None => status,
    };
    let text = String::from_utf8_lossy(&bytes);
    let said = text[..text.floor_char_boundary(PROBE_SAID_MAX)].to_owned();
    assert!(said.len() <= PROBE_SAID_MAX);
    Some((said, ended))
}

/// The log line for a failed probe, naming its flags, how it ended and what it said on
/// stderr, which is quoted with its control characters escaped so it stays one line. Inputs:
/// the flags, how it ended and what it said. Output: the line, without its newline.
pub(super) fn probe_log(flags: &[&OsStr], ended: &str, said: &str) -> String {
    format!("linux_probe_failed: {flags:?} {ended}: {said:?}")
}

/// The reason a refusal carries for a failed probe: the first line it said, or how it ended
/// when that line is blank, with control characters dropped and cut to [`PROBE_REASON_MAX`]
/// bytes on a character boundary. Inputs: its stderr and how it ended. Output: the reason.
fn probe_reason(said: &str, ended: &str) -> String {
    let clean = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let first = clean(said.lines().next().unwrap_or_default());
    let source = if first.trim().is_empty() {
        clean(ended)
    } else {
        first
    };
    let mut cut = PROBE_REASON_MAX.min(source.len());
    while !source.is_char_boundary(cut) {
        cut -= 1;
    }
    source[..cut].to_owned()
}

/// The refusal a failed probe names, unless the probe gave the kernel's answer that it holds
/// the ruleset only in part or not at all, for which the kernel refusals stay. Input: the
/// probe's reason ([`probe_reason`]). Output: the refusal, or none for the kernel's answer.
pub(super) fn probe_refusal(reason: &str) -> Option<String> {
    let kernel = [RulesetStatus::PartiallyEnforced, RulesetStatus::NotEnforced]
        .iter()
        .any(|status| reason == format!("crazyagent-landlock: {HOLD_REFUSED} ({status:?})"));
    (!kernel).then(|| format!("{PROBE_FAILED}: {reason}"))
}

/// Runs a probe with no environment and no input or output. Input: the probe command.
/// Output: true only when it ran and exited 0.
pub(super) fn passes(probe: &mut Command) -> bool {
    kernel_processes::spawn_gate::status(
        probe
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|status| status.success())
}

/// Tells whether the helper gets a mount tree of its own where this backend starts it, by
/// starting it there with a private tree and no cover but `/proc` ([`cut`]): the nested
/// probe ([`nests`]), or bubblewrap's closed box with the socket filter before `/bin/true`.
/// Input: the backend. Output: true only when that start exits 0; never on plain Landlock.
pub(super) fn cuts(backend: &LinuxBackend) -> bool {
    match backend {
        LinuxBackend::Landlock { .. } => false,
        LinuxBackend::Nested { bwrap, helper } => {
            nests(bwrap, helper, &[PROBE.into(), PRIVATE_MOUNTS.into()])
        }
        LinuxBackend::Bubblewrap { bwrap, helper } => {
            let bare = Grants {
                read: Vec::new(),
                write: Vec::new(),
                git_write: Vec::new(),
                guarded: Held::default(),
                hidden: Vec::new(),
            };
            let closed = SandboxNetwork::Only(Vec::new());
            let mut probe = bwrap_command(bwrap, helper, &bare, Path::new("/"), &closed);
            probe.args([PRIVATE_MOUNTS, "--", "/bin/true"]);
            passes(&mut probe)
        }
    }
}

/// Tells whether a bubblewrap is one the helper may nest an agent under ([`trusted_file`]).
/// A host whose bubblewrap fails this stays on plain Landlock with its gap lines. Input:
/// its path. Output: true when it may.
pub fn trusted(bwrap: &Path) -> bool {
    trusted_file(bwrap).is_some()
}

/// Tells whether this machine starts the helper inside bubblewrap's namespaces and holds
/// the same probe there, by running the widest nest a launch asks for: the helper,
/// bubblewrap with [`NEST_ARGS`] and a network of its own with one port bridged, then the
/// helper's probe. A bubblewrap that only exists proves nothing: Ubuntu 24.04 refuses it
/// the user namespace. A bubblewrap [`trusted`] refuses is never started. Inputs:
/// bubblewrap, the helper and the probe's arguments. Output: true only when the nested
/// probe exits 0.
pub fn nests(bwrap: &Path, helper: &Path, flags: &[OsString]) -> bool {
    assert!(flags.first().is_some_and(|flag| flag == PROBE));
    clean_absolute(bwrap)
        && trusted(bwrap)
        && passes(
            Command::new(helper)
                .arg(bridge::BRIDGE)
                .arg(BRIDGE_PORT_MIN.to_string())
                .arg(NEST)
                .arg("--")
                .arg(bwrap)
                .args(NEST_ARGS)
                .arg(bridge::UNSHARE_NET)
                .arg("--")
                .arg(helper)
                .args(flags),
        )
}

/// Tells whether this machine gives a nested agent a network of its own that leaves only
/// through its route ports: the widest routed nest a launch asks for, one route port and
/// one bridged port, starts the helper's probe inside it within [`ROUTE_PROBE_TIME_MAX`];
/// that probe passes only on the setup child's go byte ([`egress::started`]). Asked through
/// [`route_held`] only. Inputs: bubblewrap and the helper. Output: true when it exits 0.
fn routes(bwrap: &Path, helper: &Path) -> bool {
    clean_absolute(bwrap)
        && trusted(bwrap)
        && passes_within(
            Command::new(helper)
                .arg(egress::ROUTE)
                .arg("1")
                .arg(bridge::BRIDGE)
                .arg(BRIDGE_PORT_MIN.to_string())
                .arg(NEST)
                .arg("--")
                .arg(bwrap)
                .args(NEST_ARGS)
                .arg(bridge::UNSHARE_NET)
                .arg("--")
                .arg(helper)
                .arg(PROBE),
            ROUTE_PROBE_TIME_MAX,
        )
}

/// The route answer [`kept_route`] keeps: the bubblewrap and the helper it was asked for,
/// and whether the probe passed.
pub(super) type KeptRoute = Mutex<Option<(PathBuf, PathBuf, bool)>>;

/// Tells whether a nested agent gets a routed network of its own from one kept answer that
/// the Agent access page, the launch notice and the launch all read (rv-code2 SX2-2,
/// SX2B-1): false when `refused`, a true kept for this bubblewrap and helper for the
/// engine's life, a kept false until `again` asks it anew (rv-sa241 SBX-2-R2 N1), else the
/// probe's, kept, with no lock held while it runs and a true kept meanwhile left in place.
/// Inputs: the cell, bubblewrap, the helper, the refusal, `again`, the probe. Output: it.
pub(super) fn kept_route(
    kept: &KeptRoute,
    bwrap: &Path,
    helper: &Path,
    refused: bool,
    again: bool,
    probe: impl FnOnce() -> bool,
) -> bool {
    if refused {
        return false;
    }
    let held = kept.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some((asked_bwrap, asked_helper, answer)) = held
        && asked_bwrap == bwrap
        && asked_helper == helper
        && (answer || !again)
    {
        return answer;
    }
    let answer = probe();
    let mut cell = kept.lock().unwrap_or_else(PoisonError::into_inner);
    let found_meanwhile = cell
        .as_ref()
        .is_some_and(|(asked_bwrap, asked_helper, routed)| {
            *routed && asked_bwrap == bwrap && asked_helper == helper
        });
    if found_meanwhile {
        return true;
    }
    *cell = Some((bwrap.to_path_buf(), helper.to_path_buf(), answer));
    answer
}

/// What [`route_held`] answered, read by the host facts and every launch alike.
static ROUTE_READ: KeptRoute = Mutex::new(None);

/// [`kept_route`] for this machine: refused as root (rv-sa241 NOTE-2), where slirp4netns
/// would run with root's network before it joins the agent's. The tools check runs inside
/// the probe, so a kept true never changes for a key: a tool removed after it gives a routed
/// launch whose setup fails, which the go byte refuses. Inputs: bubblewrap, the helper and
/// `again`, which the host read sets to ask a kept false anew and a launch leaves unset, so
/// a launch never waits on the probe for a kept answer. Output: true when it is routed.
pub(super) fn route_held(bwrap: &Path, helper: &Path, again: bool) -> bool {
    let root = rustix::process::geteuid().is_root();
    let probe = || {
        let tools = SLIRP_PATHS
            .iter()
            .chain(egress::IPTABLES_PATHS.iter())
            .all(|path| Path::new(path).is_file());
        tools && routes(bwrap, helper)
    };
    kept_route(&ROUTE_READ, bwrap, helper, root, again, probe)
}

/// The bubblewrap and the helper [`kept_nest`] found a nest for.
pub(super) type KeptNest = Mutex<Option<(PathBuf, PathBuf)>>;

/// Tells whether this machine nests the helper in bubblewrap from one answer the Agent access
/// page and every launch read (rv-sa241 SBX-2-R2 LOW-1): a nest found for this bubblewrap and
/// helper is kept for the engine's life, and one not found is asked again each time, so a
/// machine that starts to nest is seen at its next read. No lock is held while the probe
/// runs, and a failed probe reads the cell again, so a nest another caller found meanwhile
/// holds. Inputs: the cell, bubblewrap, the helper, the probe. Output: true when it nests.
pub(super) fn kept_nest(
    kept: &KeptNest,
    bwrap: &Path,
    helper: &Path,
    probe: impl FnOnce() -> bool,
) -> bool {
    let found = || {
        kept.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(asked_bwrap, asked_helper)| {
                asked_bwrap == bwrap && asked_helper == helper
            })
    };
    if found() {
        return true;
    }
    if probe() {
        let asked = (bwrap.to_path_buf(), helper.to_path_buf());
        *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(asked);
        return true;
    }
    found()
}

/// What [`nest_held`] found, read by the host facts and every launch alike.
static NEST_READ: KeptNest = Mutex::new(None);

/// [`kept_nest`] for this machine, asked with the probe alone ([`nests`]); a launch then
/// runs its own flags' probe inside the nest and is refused when that fails. Inputs:
/// bubblewrap and the helper. Output: true when the helper nests in it.
pub(super) fn nest_held(bwrap: &Path, helper: &Path) -> bool {
    kept_nest(&NEST_READ, bwrap, helper, || {
        nests(bwrap, helper, &[PROBE.into()])
    })
}

/// Tells whether AppArmor keeps unprivileged programs out of user namespaces, from what
/// [`APPARMOR_USERNS`] reads: only `1`, with or without its newline, does. Input: the text,
/// none where the file cannot be read (no AppArmor, or an older one). Output: true when it
/// restricts them.
pub(crate) fn restricts(setting: Option<&str>) -> bool {
    setting.is_some_and(|text| text.strip_suffix('\n').unwrap_or(text) == "1")
}

/// Reads what this machine holds for an agent beyond its Landlock rules: whether the
/// kernel scopes signals (the helper's `--scoped`), whether a launch gives the agent a
/// process namespace ([`nest_held`], the answer every launch reads, or the plain probe
/// fails), whether the call supervisor
/// holds ([`supervises`]), whether a nested agent gets a routed network of its own
/// ([`route_held`], the answer every launch reads), whether this runs as root and whether
/// AppArmor's user namespace restriction, which never covers root, is why nothing nests
/// ([`restricts`]). Inputs: the helper and the engine's data folder, none where no probe
/// may run. Output: the facts.
pub fn read_host(helper: &Path, data: Option<&Path>) -> crate::host::LinuxHost {
    let usable = clean_absolute(helper) && helper.is_file();
    let scoped = usable && passes(Command::new(helper).arg(SCOPED));
    let held = usable && passes(Command::new(helper).arg(PROBE));
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file());
    let apart = |bwrap: &Path| !held || nest_held(bwrap, helper);
    let nested = usable && bwrap.is_some_and(apart);
    let root = rustix::process::geteuid().is_root();
    let nest = bwrap.filter(|_| nested);
    let supervised = held && data.is_some_and(|data| supervises(helper, nest, data, root));
    let routed = held && nest.is_some_and(|bwrap| route_held(bwrap, helper, true));
    let setting = fs::read_to_string(APPARMOR_USERNS).ok();
    let armored = !nested && !root && restricts(setting.as_deref());
    assert!(!routed || nested);
    assert!(!(root && routed));
    assert!(!(root && supervised));
    assert!(!(armored && nested));
    assert!(!(armored && root));
    crate::host::LinuxHost {
        nested,
        scoped,
        supervised,
        root,
        armored,
        routed,
    }
}

/// Tells whether a project may hold a git repository other than its own top-level `.git`:
/// an entry of any kind that is `.git` to some filesystem below the top, or one at the top
/// not spelled `.git`. A folder it cannot read, or more entries than it reads, answers yes.
/// Links are not followed. Input: the project folder. Output: true when one may be there.
pub(crate) fn holds_other_git(project: &Path) -> bool {
    let mut folders = vec![project.to_path_buf()];
    let mut seen: usize = 0;
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            return true;
        };
        for entry in entries {
            seen += 1;
            let Ok(entry) = entry else {
                return true;
            };
            if seen > GIT_SEARCH_ENTRIES_MAX {
                return true;
            }
            let name = entry.file_name();
            if is_git(&name) {
                let own = folder == project && name.as_os_str() == OsStr::new(".git");
                if !own {
                    return true;
                }
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                return true;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            }
            assert!(folders.len() <= seen);
        }
    }
    assert!(seen <= GIT_SEARCH_ENTRIES_MAX);
    false
}

/// The most entries [`holds_other_git`] reads in a project before it answers yes: a read
/// runs inside the engine's read of the project's Agent access page.
pub(super) const GIT_SEARCH_ENTRIES_MAX: usize = 4096;

/// How many times what is kept was marked unsupervised ([`forget_supervisor`]), then what is
/// kept of this machine and when it was read.
pub(crate) type KeptHost = Mutex<(u64, Option<(Instant, crate::host::LinuxHost)>)>;

/// What [`host_within`] last read of this machine, which a launch whose own probe failed
/// marks unsupervised ([`forget_supervisor`]).
pub(super) static HOST_READ: KeptHost = Mutex::new((0, None));

/// [`read_host`] for the helper launches take ([`helper_beside_engine`]) and the engine's
/// data folder, read again once what is kept is older than `age_max`, or with no age only
/// when nothing is kept. The lock is not held while the helper runs, so a reader of what is
/// kept never waits on it. Input: the age. Output: the facts, each false when it cannot be
/// asked.
fn host_within(age_max: Option<Duration>) -> crate::host::LinuxHost {
    let (generation, kept) = *HOST_READ.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, host)) = kept
        && age_max.is_none_or(|age_max| at.elapsed() < age_max)
    {
        return host;
    }
    let helper = helper_beside_engine();
    let host = read_host(
        &helper.unwrap_or_default(),
        kernel_processes::finding::engine_root(),
    );
    publish(&HOST_READ, generation, host);
    host
}

/// Keeps a read of this machine that began when what is kept had been marked unsupervised
/// `generation` times, only if it has not been marked since, so a read that began before
/// [`forget_supervisor`] never brings back a supervisor a launch found missing. Inputs: what
/// is kept, the count the read began at and the facts read.
pub(super) fn publish(kept: &KeptHost, generation: u64, host: crate::host::LinuxHost) {
    let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
    if held.0 == generation {
        held.1 = Some((Instant::now(), host));
    }
    assert!(held.0 >= generation);
}

/// Marks what is kept of this machine as running no call supervisor, after a launch with
/// the setting On found it does not hold (rv-secreview linux-t4c F2), so the Agent access
/// page shows the wider lines until the next read, and counts the mark so a read that began
/// before it is not kept ([`publish`]). Nothing kept stays nothing. Input: what is kept.
pub(crate) fn forget_supervisor(kept: &KeptHost) {
    let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
    held.0 += 1;
    if let Some((_, host)) = held.1.as_mut() {
        host.supervised = false;
    }
    assert!(held.1.as_ref().is_none_or(|(_, host)| !host.supervised));
}

/// What this machine holds, read again once what is kept is older than [`HOST_AGE_MAX`]:
/// it may start the helper and bubblewrap, so it is not for a database transaction. No
/// input. Output: the facts as [`read_host`] gives them.
pub(crate) fn host_now() -> crate::host::LinuxHost {
    host_within(Some(HOST_AGE_MAX))
}

/// What this machine holds as it was last read, however long ago, read now only when it
/// never was: after [`host_now`] it starts nothing. No input. Output: the facts as
/// [`read_host`] gives them.
pub(crate) fn host_kept() -> crate::host::LinuxHost {
    host_within(None)
}

/// Refuses a policy neither Linux backend starts: one confined to its own worktree, or a
/// limited network under [`LinuxSockets::Refuse`]. Input: the policy. Output: Ok, or the
/// plain `Unavailable` reason.
pub(super) fn admitted(sandbox: &ProjectSandbox) -> Result<(), SandboxError> {
    if sandbox.checkout().is_some() || sandbox.clone_root().is_some() {
        return Err(SandboxError::Unavailable(CONFINED_UNAVAILABLE.into()));
    }
    if matches!(sandbox.network(), SandboxNetwork::Only(_))
        && sandbox.linux_sockets() == LinuxSockets::Refuse
    {
        return Err(SandboxError::Unavailable(LIMITED_NETWORK_OFF.into()));
    }
    Ok(())
}
