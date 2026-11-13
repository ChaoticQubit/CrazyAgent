use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The host a test probe hands out, none until the test installs tmux, and how many times
/// the runtime asked for it.
#[derive(Clone, Default)]
struct Found {
    host: Arc<Mutex<Option<Arc<dyn Host>>>>,
    asked: Arc<AtomicUsize>,
}

impl Found {
    fn probe(&self) -> TerminalProbe {
        let found = self.clone();
        Box::new(move || {
            found.asked.fetch_add(1, Ordering::SeqCst);
            found.host.lock().unwrap().clone()
        })
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

/// Opens a team runtime as an engine that started before tmux was installed: no terminal
/// host, the stand-in Codex given, and the probe when one is given.
/// Inputs: the fixture, the stand-in script and the probe. Output: the runtime.
fn without_tmux(fixture: &Fixture, script: &str, probe: Option<TerminalProbe>) -> TeamRuntime {
    let bin = fixture.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, script).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let access = fixture.access.clone();
    let runtime = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap();
    match probe {
        Some(probe) => runtime.with_terminal_probe(probe),
        None => runtime,
    }
}

fn says_tmux_is_missing(fixture: &Fixture) -> bool {
    let status = fixture.status();
    !super::super::super::master::live(&fixture.connection, &fixture.setup.project_id).unwrap()
        && status.live_master == Some(false)
        && status.live_problem.as_deref() == Some(super::super::super::master::TERMINAL_MISSING)
}

/// On a fresh Mac the engine starts before tmux is installed. A live master switched on
/// there never waits in silence: the switch turns off with the one line that says tmux is
/// missing and how to install it, in the master's problem line and one team note, and no
/// master row starts. The owner's Try again turns the switch on again, finds tmux still
/// missing and turns it off with the same line, writing no second note.
#[test]
fn a_live_master_with_no_tmux_turns_off_and_says_tmux_is_missing_once() {
    let fixture = Fixture::new(1);
    let mut team = without_tmux(&fixture, RESUMING_CODEX, None);
    let project = fixture.setup.project_id.clone();
    super::super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(
        &mut team,
        "the switch says tmux is missing",
        says_tmux_is_missing,
    );
    idle_ticks(&mut team);
    assert!(masters(&fixture.connection).is_empty(), "no master starts");
    assert_eq!(
        noted(&fixture, super::super::super::master::TERMINAL_MISSING),
        1
    );

    super::super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "Try again says it again", says_tmux_is_missing);
    idle_ticks(&mut team);
    assert!(masters(&fixture.connection).is_empty());
    assert_eq!(
        noted(&fixture, super::super::super::master::TERMINAL_MISSING),
        1,
        "the same reason is noted once"
    );
    team.shutdown().unwrap();
}

/// tmux installed after the engine started is found without an engine restart: the runtime
/// looks for a terminal host again at each live master start, never while the switch is off,
/// and Try again then starts the live master in its terminal and clears the tmux line.
#[test]
fn a_live_master_starts_once_tmux_is_installed_with_no_engine_restart() {
    let fixture = Fixture::new(1);
    let found = Found::default();
    let mut team = without_tmux(&fixture, RESUMING_CODEX, Some(found.probe()));
    let project = fixture.setup.project_id.clone();
    super::super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "no tmux yet", says_tmux_is_missing);
    let asked = found.asked();
    assert!(asked >= 1, "the start looked for tmux");
    idle_ticks(&mut team);
    assert_eq!(
        found.asked(),
        asked,
        "nothing looks while the switch is off"
    );

    let host = fixture.host();
    *found.host.lock().unwrap() = Some(host.clone());
    super::super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "the live master runs in its terminal", |f| {
        live_agent(&f.connection).is_some()
    });
    let master = masters(&fixture.connection)[0];
    assert_eq!(host.alive().unwrap(), names(&[master]));
    fixture.until(&mut team, "the tmux line is gone", |f| {
        f.status().live_problem.is_none()
    });
    assert!(super::super::super::master::live(&fixture.connection, &project).unwrap());
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

static TOOL_LOOKS: AtomicUsize = AtomicUsize::new(0);

fn count_tool_look() {
    TOOL_LOOKS.fetch_add(1, Ordering::SeqCst);
}

/// The owner installs a tool and presses Try again: each time the owner switches the live
/// master on, the runtime asks the tool probes to look again at once
/// (`tools::probe::refresh_now`), whatever their backoff, so a tool found only on the login
/// shell's PATH or npm's prefix is seen; ticks while the switch stays off never look.
#[test]
fn each_live_master_start_the_owner_asks_for_looks_for_tools_again() {
    let fixture = Fixture::new(1);
    let mut team = without_tmux(&fixture, RESUMING_CODEX, None).with_tools_refresh(count_tool_look);
    let project = fixture.setup.project_id.clone();
    super::super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(
        &mut team,
        "the first start says tmux is missing",
        says_tmux_is_missing,
    );
    idle_ticks(&mut team);
    assert_eq!(
        TOOL_LOOKS.load(Ordering::SeqCst),
        1,
        "the start looked once"
    );

    super::super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "Try again says it again", says_tmux_is_missing);
    idle_ticks(&mut team);
    assert_eq!(
        TOOL_LOOKS.load(Ordering::SeqCst),
        2,
        "Try again looked again, once"
    );
    team.shutdown().unwrap();
}

static RETRY_LOOKS: AtomicUsize = AtomicUsize::new(0);

fn count_retry_look() {
    RETRY_LOOKS.fetch_add(1, Ordering::SeqCst);
}

/// A Codex stand-in whose master starts and draws a line but never reads keys, as one whose
/// terminal stays overdue: it records each launch, resumes its own conversation, and waits.
const SILENT_CODEX: &str = r#"#!/bin/sh
[ "$1" = app-server ] && exit 0
d="$CODEX_HOME/sessions/2026/10/27"
/bin/mkdir -p "$d" || exit 1
printf '%s\n' "$*" >> "$CODEX_HOME/launches"
id=""
prev=""
for a in "$@"; do
  [ "$prev" = "resume" ] && id="$a"
  prev="$a"
done
if [ -z "$id" ]; then
  id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
  f="$d/rollout-2026-10-27T00-00-00-$id.jsonl"
  printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
fi
printf 'Codex stand-in starting\n'
exec /bin/sleep 600
"#;

/// Counts the master launches every team agent home recorded. Takes the fixture. Returns
/// the count.
fn master_launches(fixture: &Fixture) -> usize {
    std::fs::read_dir(fixture.base.join("team/agents"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|agent| std::fs::read_to_string(agent.path().join("home/launches")).ok())
        .map(|log| log.lines().count())
        .sum()
}

/// Sends the owner's live master switch-on, Start or Try again, from the desktop or the TUI
/// (LiveMaster on) through the owner action path. Takes the fixture.
fn owner_switch_on(fixture: &mut Fixture) {
    use crate::protocol::Outcome;
    use crate::team::TeamAction;
    let project = fixture.setup.project_id.clone();
    let tx = fixture.connection.transaction().unwrap();
    let version = crate::team::settings_version(&tx, &project).unwrap();
    let on = TeamAction::LiveMaster {
        project_id: project,
        on: true,
        version,
    };
    assert_eq!(
        crate::team::execute(&tx, &fixture.access, &on).unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
}

/// r2 F1: the owner's Try again while the live master's switch is already on, as on an
/// overdue terminal, is a retry from the desktop and the TUI alike (both send LiveMaster
/// on, DEC-141): the tool probes look again and a master that has not read keys a minute
/// after its start starts once more; a young master only gets the look.
#[test]
fn an_overdue_try_again_looks_for_tools_again_and_restarts_the_master_once() {
    let mut fixture = Fixture::new(1);
    let host = fixture.host();
    let mut team = limited_runtime(&fixture, &host, SILENT_CODEX, TeamLimits::default())
        .with_tools_refresh(count_retry_look);
    let project = fixture.setup.project_id.clone();
    owner_switch_on(&mut fixture);
    fixture.until(&mut team, "the master starts", |f| master_launches(f) == 1);
    idle_ticks(&mut team);
    assert_eq!(
        RETRY_LOOKS.load(Ordering::SeqCst),
        1,
        "the start looked once"
    );

    owner_switch_on(&mut fixture);
    idle_ticks(&mut team);
    assert_eq!(
        (
            RETRY_LOOKS.load(Ordering::SeqCst),
            master_launches(&fixture)
        ),
        (2, 1),
        "a young master only gets the look"
    );

    let id = team.master(&project).expect("the master runs");
    team.workers.get_mut(&id).unwrap().spawned = Instant::now()
        .checked_sub(Duration::from_secs(600))
        .unwrap();
    owner_switch_on(&mut fixture);
    fixture.until(&mut team, "the overdue master starts again", |f| {
        master_launches(f) == 2
    });
    idle_ticks(&mut team);
    assert_eq!(
        (
            RETRY_LOOKS.load(Ordering::SeqCst),
            master_launches(&fixture)
        ),
        (3, 2),
        "one more look and one restart"
    );
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

/// r2 (leader 18:20): the owner's own Try again is never refused by the automatic start
/// limit (`LIVE_STARTS_MAX` starts in `LIVE_STARTS_WINDOW`): each owner press clears the
/// project's automatic start count, so more overdue Try agains than the limit each start
/// the master again and the live master stays on.
#[test]
fn an_owner_try_again_is_never_refused_by_the_start_limit() {
    let mut fixture = Fixture::new(1);
    let host = fixture.host();
    let mut team = limited_runtime(&fixture, &host, SILENT_CODEX, TeamLimits::default())
        .with_tools_refresh(|| {});
    let project = fixture.setup.project_id.clone();
    owner_switch_on(&mut fixture);
    fixture.until(&mut team, "the master starts", |f| master_launches(f) == 1);
    for press in 1..=LIVE_STARTS_MAX {
        let id = team.master(&project).expect("the master runs");
        team.workers.get_mut(&id).unwrap().spawned = Instant::now()
            .checked_sub(Duration::from_secs(600))
            .unwrap();
        owner_switch_on(&mut fixture);
        fixture.until(&mut team, "Try again starts the master again", |f| {
            master_launches(f) == press + 1
        });
    }
    idle_ticks(&mut team);
    assert!(
        super::super::super::master::live(&fixture.connection, &project).unwrap(),
        "{:?}",
        fixture.status().live_problem
    );
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}
