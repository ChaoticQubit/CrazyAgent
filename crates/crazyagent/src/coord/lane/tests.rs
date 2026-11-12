use super::*;
use crate::coord::settings::{
    CoordSettings, HeavyLane, RESOURCE_SLOTS_MAX, SHARED_RESOURCES_MAX, SharedResource,
};
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::PathBuf;

const PROJECT: &str = "lane-project";

fn db() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    connection
}

fn project() -> ProjectId {
    ProjectId::new(PROJECT).unwrap()
}

fn waiting(uses: &[&str]) -> Queued {
    Queued {
        uses: uses.iter().map(|name| (*name).to_owned()).collect(),
        granted: false,
    }
}

fn running(uses: &[&str]) -> Queued {
    Queued {
        granted: true,
        ..waiting(uses)
    }
}

fn resource(name: &str, slots: u8) -> SharedResource {
    SharedResource {
        name: name.to_owned(),
        slots,
    }
}

fn ticket(uses: &[&str], label: &str) -> Ticket {
    Ticket {
        uses: uses.iter().map(|name| (*name).to_owned()).collect(),
        label: label.to_owned(),
    }
}

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// Makes `<root>/agent-<id>/tmp/crazyagent-lane` 0700, the layout the engine opens without
/// following a link. Takes the root and the agent. Returns the holder.
fn holder(root: &Path, agent: AgentId) -> Holder {
    let folder = root.join(format!("agent-{agent}")).join("tmp").join(FOLDER);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .unwrap();
    Holder { agent, folder }
}

fn has(holder: &Holder, entry: &Entry, suffix: &str) -> bool {
    holder.folder.join(format!("{}{suffix}", entry.id)).exists()
}

fn read(holder: &Holder, entry: &Entry, suffix: &str) -> String {
    std::fs::read_to_string(holder.folder.join(format!("{}{suffix}", entry.id))).unwrap()
}

fn rows(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT COUNT(*) FROM coord_lane", [], |row| row.get(0))
        .unwrap()
}

/// Runs `grant` on `count` waiting commands that use no shared resource. Takes the lane
/// option and the count. Returns how many run.
fn runs_at_once(lane: HeavyLane, count: usize) -> usize {
    let queue: Vec<Queued> = (0..count).map(|_| waiting(&[])).collect();
    let runs = grant(&queue, lane, &[]);
    let first = runs.iter().take_while(|runs| **runs).count();
    assert_eq!(first, runs.iter().filter(|runs| **runs).count());
    first
}

#[test]
fn heavy_lane_one_runs_one_command_at_a_time() {
    assert_eq!(runs_at_once(HeavyLane::One, 6), 1);
}

#[test]
fn heavy_lane_two_runs_two_commands_at_a_time() {
    assert_eq!(runs_at_once(HeavyLane::Two, 6), 2);
}

#[test]
fn heavy_lane_three_runs_three_commands_at_a_time() {
    assert_eq!(runs_at_once(HeavyLane::Three, 6), 3);
}

#[test]
fn heavy_lane_four_runs_four_commands_at_a_time() {
    assert_eq!(runs_at_once(HeavyLane::Four, 6), 4);
}

#[test]
fn heavy_lane_no_limit_runs_every_command_at_once() {
    assert_eq!(runs_at_once(HeavyLane::NoLimit, 6), 6);
}

#[test]
fn every_heavy_lane_option_has_its_slots_and_words() {
    let slots: Vec<Option<usize>> = HeavyLane::ALL.iter().map(|lane| lane.slots()).collect();
    assert_eq!(slots, vec![Some(1), Some(2), Some(3), Some(4), None]);
    assert_eq!(HeavyLane::default(), HeavyLane::One);
    for lane in HeavyLane::ALL {
        assert_eq!(HeavyLane::parse(lane.as_str()), Some(lane));
        assert!(!lane.words().is_empty());
    }
}

#[test]
fn a_shared_resource_serves_its_slots_and_lets_other_commands_pass() {
    let queue = [waiting(&["ollama"]), waiting(&["ollama"]), waiting(&[])];
    let runs = grant(&queue, HeavyLane::NoLimit, &[resource("ollama", 1)]);
    assert_eq!(runs, vec![true, false, true]);
    let runs = grant(&queue, HeavyLane::NoLimit, &[resource("ollama", 2)]);
    assert_eq!(runs, vec![true, true, true]);
}

#[test]
fn a_waiting_command_is_never_overtaken_on_what_it_waits_for() {
    let queue = [running(&[]), waiting(&[]), waiting(&[])];
    assert_eq!(grant(&queue, HeavyLane::One, &[]), vec![true, false, false]);
    let queue = [
        running(&["ollama"]),
        waiting(&["ollama", "postgres"]),
        waiting(&["postgres"]),
        waiting(&[]),
    ];
    let resources = [resource("ollama", 1), resource("postgres", 1)];
    assert_eq!(
        grant(&queue, HeavyLane::NoLimit, &resources),
        vec![true, false, false, true]
    );
    let after = [waiting(&["ollama", "postgres"]), running(&["postgres"])];
    assert_eq!(
        grant(&after, HeavyLane::NoLimit, &resources),
        vec![false, true]
    );
}

#[test]
fn a_running_command_keeps_running_when_the_lane_shrinks() {
    let queue = [running(&[]), running(&[]), running(&[]), waiting(&[])];
    assert_eq!(
        grant(&queue, HeavyLane::One, &[]),
        vec![true, true, true, false]
    );
}

#[test]
fn a_resource_the_settings_do_not_name_never_runs() {
    let queue = [waiting(&["gone"])];
    assert_eq!(grant(&queue, HeavyLane::NoLimit, &[]), vec![false]);
}

#[test]
fn a_command_naming_an_unknown_resource_is_refused_with_the_known_names() {
    let mut settings = CoordSettings::default();
    let reason = refusal(&ticket(&["ollama"], "make"), &settings).unwrap();
    assert!(
        reason.contains("ollama") && reason.contains("no shared resources"),
        "{reason}"
    );
    settings.shared_resources = vec![resource("postgres", 1)];
    let reason = refusal(&ticket(&["ollama"], "make"), &settings).unwrap();
    assert!(
        reason.contains("ollama") && reason.contains("postgres"),
        "{reason}"
    );
    assert_eq!(refusal(&ticket(&["postgres"], "make"), &settings), None);
    assert_eq!(refusal(&ticket(&[], "make"), &settings), None);
    assert!(refusal(&ticket(&[], ""), &settings).is_some());
}

#[test]
fn parse_reads_the_resources_the_label_and_the_command() {
    let request = parse(&os(&[
        "--folder",
        "/state/agents/agent-3/tmp/crazyagent-lane",
        "--uses",
        "ollama",
        "--uses",
        "pg-16",
        "--label",
        "make check",
        "--",
        "make",
        "check",
    ]))
    .unwrap();
    assert_eq!(
        request.folder,
        Some(PathBuf::from("/state/agents/agent-3/tmp/crazyagent-lane"))
    );
    assert_eq!(request.uses, vec!["ollama".to_owned(), "pg-16".to_owned()]);
    assert_eq!(request.label.as_deref(), Some("make check"));
    assert_eq!(request.program, OsString::from("make"));
    assert_eq!(request.arguments, os(&["check"]));
    let plain = parse(&os(&["--", "cargo", "test", "--", "--nocapture"])).unwrap();
    assert!(plain.folder.is_none() && plain.uses.is_empty() && plain.label.is_none());
    assert_eq!(plain.arguments, os(&["test", "--", "--nocapture"]));
}

#[test]
fn parse_refuses_what_the_lane_cannot_read() {
    for words in [
        &[][..],
        &["make"][..],
        &["--"][..],
        &["--uses"][..],
        &["--folder", "relative/lane", "--", "make"][..],
        &["--folder", "/a", "--folder", "/b", "--", "make"][..],
        &["--folder"][..],
        &["--uses", "Ollama", "--", "make"][..],
        &["--uses", "a b", "--", "make"][..],
        &["--uses", "x", "--uses", "x", "--", "make"][..],
        &[
            "--uses", "a", "--uses", "b", "--uses", "c", "--uses", "d", "--uses", "e", "--", "make",
        ][..],
        &["--label", "", "--", "make"][..],
        &["--label", "a;rm", "--", "make"][..],
        &["--label", " padded", "--", "make"][..],
        &["--other", "--", "make"][..],
    ] {
        let problem = parse(&os(words)).unwrap_err();
        assert!(problem.contains(USAGE), "{words:?}: {problem}");
    }
    let many: Vec<OsString> = std::iter::repeat_n(OsString::from("x"), ARGUMENTS_MAX + 1).collect();
    assert!(parse(&many).is_err());
}

#[test]
fn the_label_never_carries_the_commands_arguments() {
    assert_eq!(program_label(OsStr::new("/usr/bin/make")), "make");
    assert_eq!(program_label(OsStr::new("./run tests.sh")), "runtests.sh");
    assert_eq!(program_label(OsStr::new("/")), "command");
    let request = parse(&os(&["--", "/usr/bin/env", "TOKEN=sk-secret", "make"])).unwrap();
    let label = request
        .label
        .unwrap_or_else(|| program_label(&request.program));
    assert_eq!(label, "env");
    assert!(!label.contains("secret"));
}

#[test]
fn the_waiting_line_names_the_lane_the_queue_and_the_resources() {
    let line = waiting_line(HeavyLane::Two, 2, 3, &["ollama".to_owned()]);
    assert!(
        line.contains("2 at a time") && line.contains("2 running"),
        "{line}"
    );
    assert!(
        line.contains("3 waiting ahead") && line.contains("ollama"),
        "{line}"
    );
    assert!(!waiting_line(HeavyLane::NoLimit, 0, 0, &[]).contains("uses"));
}

#[test]
fn coord_settings_refuse_shared_resources_the_lane_cannot_use() {
    let with = |resources: Vec<SharedResource>| CoordSettings {
        shared_resources: resources,
        ..CoordSettings::default()
    };
    assert!(
        with(vec![
            resource("ollama", 1),
            resource("postgres", RESOURCE_SLOTS_MAX)
        ])
        .validate()
        .is_ok()
    );
    for bad in [
        vec![resource("ollama", 0)],
        vec![resource("ollama", RESOURCE_SLOTS_MAX + 1)],
        vec![resource("Ollama", 1)],
        vec![resource("", 1)],
        vec![resource("-x", 1)],
        vec![resource(&"a".repeat(33), 1)],
        vec![resource("ollama", 1), resource("ollama", 2)],
        (0..=SHARED_RESOURCES_MAX)
            .map(|at| resource(&format!("r{at}"), 1))
            .collect(),
    ] {
        assert_eq!(
            with(bad.clone()).validate().unwrap_err().0,
            "coord_settings_invalid",
            "{bad:?}"
        );
    }
}

#[test]
fn a_coord_settings_row_from_before_the_lane_reads_with_the_lane_defaults() {
    let old = r#"{"questions_to":"lead","overlaps":"warn","nudges":"off","counted_tools":[]}"#;
    let settings: CoordSettings = serde_json::from_str(old).unwrap();
    assert_eq!(settings.heavy_lane, HeavyLane::One);
    assert!(settings.shared_resources.is_empty());
    let chosen = CoordSettings {
        heavy_lane: HeavyLane::NoLimit,
        shared_resources: vec![resource("ollama", 2)],
        ..CoordSettings::default()
    };
    let json = serde_json::to_string(&chosen).unwrap();
    assert!(json.contains(r#""heavy_lane":"no_limit""#), "{json}");
    assert_eq!(
        serde_json::from_str::<CoordSettings>(&json).unwrap(),
        chosen
    );
}

#[test]
fn tick_lets_one_command_run_and_hands_its_slot_on_when_it_ends() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let settings = CoordSettings::default();
    let a = holder(root.path(), 1);
    let b = holder(root.path(), 2);
    let holders = [a.clone(), b.clone()];
    let first = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    tick(&connection, &project(), &holders, &settings, 1_000).unwrap();
    assert!(has(&a, &first, GO));
    let second = Entry::enter(&b.folder, &ticket(&[], "cargo")).unwrap();
    tick(&connection, &project(), &holders, &settings, 2_000).unwrap();
    assert!(!has(&b, &second, GO));
    let line = read(&b, &second, WAIT);
    assert!(
        line.contains("1 running") && line.contains("0 waiting ahead"),
        "{line}"
    );
    let shown = view(&connection, &project()).unwrap();
    assert_eq!(
        shown.running,
        vec![LaneEntry {
            agent: 1,
            label: "make".into(),
            uses: vec![],
            since_ms: 1_000
        }]
    );
    assert_eq!(shown.waiting.len(), 1);
    assert_eq!(
        (shown.waiting[0].agent, shown.waiting[0].since_ms),
        (2, 2_000)
    );
    first.leave();
    tick(&connection, &project(), &holders, &settings, 3_000).unwrap();
    assert!(has(&b, &second, GO));
    let shown = view(&connection, &project()).unwrap();
    assert_eq!(shown.running.len(), 1);
    assert_eq!(
        (shown.running[0].agent, shown.running[0].since_ms),
        (2, 3_000)
    );
    assert!(shown.waiting.is_empty());
    second.leave();
    tick(&connection, &project(), &holders, &settings, 4_000).unwrap();
    assert_eq!(rows(&connection), 0);
}

#[test]
fn tick_frees_the_place_of_a_lane_command_that_died() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let a = holder(root.path(), 1);
    let entry = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    let id = entry.id.clone();
    tick(
        &connection,
        &project(),
        &[a.clone()],
        &CoordSettings::default(),
        1_000,
    )
    .unwrap();
    assert_eq!(rows(&connection), 1);
    drop(entry);
    tick(
        &connection,
        &project(),
        &[a.clone()],
        &CoordSettings::default(),
        2_000,
    )
    .unwrap();
    assert_eq!(rows(&connection), 0);
    for suffix in SUFFIXES {
        assert!(!a.folder.join(format!("{id}{suffix}")).exists(), "{suffix}");
    }
}

#[test]
fn tick_drops_the_commands_of_an_agent_it_no_longer_reads() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let a = holder(root.path(), 1);
    let entry = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    tick(
        &connection,
        &project(),
        &[a.clone()],
        &CoordSettings::default(),
        1_000,
    )
    .unwrap();
    assert_eq!(rows(&connection), 1);
    tick(
        &connection,
        &project(),
        &[],
        &CoordSettings::default(),
        2_000,
    )
    .unwrap();
    assert_eq!(rows(&connection), 0);
    entry.leave();
}

#[test]
fn tick_refuses_a_ticket_naming_a_resource_the_project_lacks() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let a = holder(root.path(), 1);
    let entry = Entry::enter(&a.folder, &ticket(&["ollama"], "make")).unwrap();
    tick(
        &connection,
        &project(),
        &[a.clone()],
        &CoordSettings::default(),
        1_000,
    )
    .unwrap();
    assert!(read(&a, &entry, NO).contains("ollama"));
    assert!(!has(&a, &entry, GO));
    assert_eq!(rows(&connection), 0);
    let mut out = Vec::new();
    match entry.wait(&mut out) {
        Turn::Refused(reason) => assert!(reason.contains("ollama"), "{reason}"),
        _ => panic!("a refused ticket must end its wait refused"),
    }
    entry.leave();
}

#[test]
fn tick_serves_a_shared_resource_one_command_at_a_time() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let settings = CoordSettings {
        heavy_lane: HeavyLane::NoLimit,
        shared_resources: vec![resource("ollama", 1)],
        ..CoordSettings::default()
    };
    let a = holder(root.path(), 1);
    let b = holder(root.path(), 2);
    let holders = [a.clone(), b.clone()];
    let first = Entry::enter(&a.folder, &ticket(&["ollama"], "make")).unwrap();
    tick(&connection, &project(), &holders, &settings, 1_000).unwrap();
    let second = Entry::enter(&b.folder, &ticket(&["ollama"], "pytest")).unwrap();
    let free = Entry::enter(&b.folder, &ticket(&[], "cargo")).unwrap();
    tick(&connection, &project(), &holders, &settings, 2_000).unwrap();
    assert!(has(&a, &first, GO));
    assert!(!has(&b, &second, GO));
    assert!(read(&b, &second, WAIT).contains("ollama"));
    assert!(has(&b, &free, GO));
    first.leave();
    tick(&connection, &project(), &holders, &settings, 3_000).unwrap();
    assert!(has(&b, &second, GO));
    second.leave();
    free.leave();
}

#[test]
fn tick_never_follows_a_link_planted_at_an_answer() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let a = holder(root.path(), 1);
    let victim: PathBuf = root.path().join("victim");
    std::fs::write(&victim, "kept").unwrap();
    let entry = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    symlink(&victim, a.folder.join(format!("{}{GO}", entry.id))).unwrap();
    tick(
        &connection,
        &project(),
        &[a.clone()],
        &CoordSettings::default(),
        1_000,
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "kept");
    let other = holder(root.path(), 2);
    let waits = Entry::enter(&other.folder, &ticket(&[], "cargo")).unwrap();
    symlink(
        &victim,
        other.folder.join(format!("{}{WAIT_PART}", waits.id)),
    )
    .unwrap();
    tick(
        &connection,
        &project(),
        &[a.clone(), other.clone()],
        &CoordSettings::default(),
        2_000,
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "kept");
    assert!(read(&other, &waits, WAIT).contains("1 running"));
    entry.leave();
    waits.leave();
}

#[test]
fn tick_never_follows_a_link_at_the_lane_folder() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let real = holder(root.path(), 1);
    let entry = Entry::enter(&real.folder, &ticket(&[], "make")).unwrap();
    let tmp = root.path().join("agent-2").join("tmp");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&tmp)
        .unwrap();
    symlink(&real.folder, tmp.join(FOLDER)).unwrap();
    let linked = Holder {
        agent: 2,
        folder: tmp.join(FOLDER),
    };
    tick(
        &connection,
        &project(),
        &[linked],
        &CoordSettings::default(),
        1_000,
    )
    .unwrap();
    assert_eq!(rows(&connection), 0);
    assert!(!has(&real, &entry, GO));
    entry.leave();
}

#[test]
fn a_wait_with_no_word_from_the_engine_runs_the_command_at_once() {
    let root = tempfile::tempdir().unwrap();
    let a = holder(root.path(), 1);
    let mut entry = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    entry.since = std::time::SystemTime::now() - SILENCE_MAX - Duration::from_secs(1);
    let mut out = Vec::new();
    assert!(matches!(entry.wait(&mut out), Turn::Silent));
    entry.leave();
}

#[test]
fn a_ticket_is_locked_before_the_engine_can_see_it() {
    use rustix::fs::{FlockOperation, flock};
    let root = tempfile::tempdir().unwrap();
    let a = holder(root.path(), 1);
    let entry = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    let probe = std::fs::File::open(a.folder.join(format!("{}{TICKET}", entry.id))).unwrap();
    assert!(flock(&probe, FlockOperation::NonBlockingLockExclusive).is_err());
    assert!(!a.folder.join(format!("{}{PART}", entry.id)).exists());
    entry.leave();
    assert!(flock(&probe, FlockOperation::NonBlockingLockExclusive).is_ok());
}

#[test]
fn the_lane_table_goes_with_its_project() {
    let connection = db();
    connection
        .execute(
            "INSERT INTO coord_lane(project,agent,ticket,uses,label,seq,created_ms,seen_ms) VALUES (?1,1,'t','[]','make',1,1,1)",
            [PROJECT],
        )
        .unwrap();
    crate::coord::forget_project(&connection, &project()).unwrap();
    assert_eq!(rows(&connection), 0);
}

#[test]
fn a_project_whose_last_agent_stopped_is_still_ticked_until_its_lane_is_clear() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let a = holder(root.path(), 1);
    let entry = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    tick(
        &connection,
        &project(),
        &[a.clone()],
        &CoordSettings::default(),
        1_000,
    )
    .unwrap();
    assert_eq!(projects(&connection).unwrap(), vec![project()]);
    tick(
        &connection,
        &project(),
        &[],
        &CoordSettings::default(),
        2_000,
    )
    .unwrap();
    assert!(projects(&connection).unwrap().is_empty());
    let shown = view(&connection, &project()).unwrap();
    assert!(shown.running.is_empty() && shown.waiting.is_empty());
    entry.leave();
}

#[test]
fn the_heavy_commands_section_names_the_engine_the_folder_and_the_resources() {
    let engine = Path::new("/Applications/CrazyAgent.app/Contents/MacOS/crazyagent");
    let folder = Path::new("/state/team/agents/agent-4/tmp/crazyagent-lane");
    let section = crate::launch_context::heavy_lane_section(
        engine,
        folder,
        &[resource("ollama", 1), resource("postgres", 2)],
    )
    .unwrap();
    assert!(section.contains(
        "'/Applications/CrazyAgent.app/Contents/MacOS/crazyagent' lane --folder '/state/team/agents/agent-4/tmp/crazyagent-lane' -- <command>"
    ), "{section}");
    assert!(
        section.contains("ollama, postgres") && section.contains("--uses ollama"),
        "{section}"
    );
    let bare = crate::launch_context::heavy_lane_section(engine, folder, &[]).unwrap();
    assert!(bare.contains("no shared resources"), "{bare}");
    for (engine, folder) in [
        (Path::new("/tmp/it's/crazyagent"), folder),
        (engine, Path::new("/tmp/line\nbreak")),
        (Path::new("relative/crazyagent"), folder),
        (engine, Path::new("relative/lane")),
    ] {
        assert_eq!(
            crate::launch_context::heavy_lane_section(engine, folder, &[]),
            None,
            "{engine:?} {folder:?}"
        );
    }
    let most: Vec<SharedResource> = (0..SHARED_RESOURCES_MAX)
        .map(|at| resource(&format!("{}{at:02}", "r".repeat(29)), 1))
        .collect();
    let long = format!("/{}", "e".repeat(1023));
    let section =
        crate::launch_context::heavy_lane_section(Path::new(&long), Path::new(&long), &most)
            .unwrap();
    assert!(section.len() <= crate::launch_context::HEAVY_LANE_BYTES_MAX);
}

/// F28: a running command keeps its slot while its agent's lane folder cannot be opened for
/// a tick, so another agent's command still waits, and it hands the slot on once it ends.
#[test]
fn tick_keeps_a_running_commands_slot_while_its_folder_cannot_be_read() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let settings = CoordSettings::default();
    let a = holder(root.path(), 1);
    let b = holder(root.path(), 2);
    let first = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    tick(
        &connection,
        &project(),
        &[a.clone(), b.clone()],
        &settings,
        1_000,
    )
    .unwrap();
    assert!(has(&a, &first, GO));
    let second = Entry::enter(&b.folder, &ticket(&[], "cargo")).unwrap();
    let unreadable = Holder {
        agent: 1,
        folder: root.path().join("agent-1").join("tmp").join("missing"),
    };
    tick(
        &connection,
        &project(),
        &[unreadable, b.clone()],
        &settings,
        2_000,
    )
    .unwrap();
    assert!(!has(&b, &second, GO), "the lane is still full");
    let shown = view(&connection, &project()).unwrap();
    assert_eq!(
        (shown.running.len(), shown.waiting.len()),
        (1, 1),
        "{shown:?}"
    );
    assert_eq!(shown.running[0].agent, 1);
    tick(
        &connection,
        &project(),
        &[a.clone(), b.clone()],
        &settings,
        3_000,
    )
    .unwrap();
    assert!(!has(&b, &second, GO));
    first.leave();
    tick(&connection, &project(), &[a, b.clone()], &settings, 4_000).unwrap();
    assert!(has(&b, &second, GO));
    second.leave();
}

/// F28: a command that already runs keeps running when the owner removes the shared
/// resource it uses, while a waiting one that names it is refused.
#[test]
fn removing_a_shared_resource_refuses_only_the_commands_that_wait_for_it() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let with = CoordSettings {
        heavy_lane: HeavyLane::NoLimit,
        shared_resources: vec![resource("ollama", 1)],
        ..CoordSettings::default()
    };
    let a = holder(root.path(), 1);
    let b = holder(root.path(), 2);
    let first = Entry::enter(&a.folder, &ticket(&["ollama"], "eval")).unwrap();
    let second = Entry::enter(&b.folder, &ticket(&["ollama"], "eval")).unwrap();
    let holders = [a.clone(), b.clone()];
    tick(&connection, &project(), &holders, &with, 1_000).unwrap();
    let without = CoordSettings {
        shared_resources: vec![],
        ..with.clone()
    };
    tick(&connection, &project(), &holders, &without, 2_000).unwrap();
    let (runner, runner_holder, waiter, waiter_holder) = if has(&a, &first, GO) {
        (&first, &a, &second, &b)
    } else {
        (&second, &b, &first, &a)
    };
    assert!(has(runner_holder, runner, GO));
    assert!(
        !has(runner_holder, runner, NO),
        "a running command is never refused"
    );
    assert!(read(waiter_holder, waiter, NO).contains("ollama"));
    let shown = view(&connection, &project()).unwrap();
    assert_eq!(
        (shown.running.len(), shown.waiting.len()),
        (1, 0),
        "{shown:?}"
    );
    first.leave();
    second.leave();
}

/// F28 L5R1-L1: a running command whose lane folder is gone keeps its slot only until
/// `SILENCE_MAX` after the engine last read it; then its row goes and the next command in
/// the lane gets its go, so a removed folder never holds the lane until its agent stops.
#[test]
fn a_gone_lane_folder_holds_its_slot_no_longer_than_silence_max() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let settings = CoordSettings::default();
    let a = holder(root.path(), 1);
    let b = holder(root.path(), 2);
    let holders = [a.clone(), b.clone()];
    let first = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    tick(&connection, &project(), &holders, &settings, 1_000).unwrap();
    assert!(has(&a, &first, GO));
    let second = Entry::enter(&b.folder, &ticket(&[], "cargo")).unwrap();
    std::fs::remove_dir_all(&a.folder).unwrap();
    let silence = i64::try_from(SILENCE_MAX.as_millis()).unwrap();
    tick(
        &connection,
        &project(),
        &holders,
        &settings,
        1_000 + silence,
    )
    .unwrap();
    assert!(!has(&b, &second, GO), "the slot is kept up to SILENCE_MAX");
    assert_eq!(view(&connection, &project()).unwrap().running[0].agent, 1);
    tick(
        &connection,
        &project(),
        &holders,
        &settings,
        1_001 + silence,
    )
    .unwrap();
    assert!(
        has(&b, &second, GO),
        "past SILENCE_MAX the next command runs"
    );
    let shown = view(&connection, &project()).unwrap();
    assert_eq!(
        (
            shown.running.len(),
            shown.waiting.len(),
            shown.running[0].agent
        ),
        (1, 0, 2),
        "{shown:?}"
    );
    second.leave();
    drop(first);
}

/// F28 L5R1-L1: a command the engine keeps reading never loses its slot, however long it
/// runs: its seen time moves with each heartbeat.
#[test]
fn a_long_running_command_that_is_read_keeps_its_slot_past_silence_max() {
    let root = tempfile::tempdir().unwrap();
    let connection = db();
    let settings = CoordSettings::default();
    let a = holder(root.path(), 1);
    let b = holder(root.path(), 2);
    let holders = [a.clone(), b.clone()];
    let first = Entry::enter(&a.folder, &ticket(&[], "make")).unwrap();
    let second = Entry::enter(&b.folder, &ticket(&[], "cargo")).unwrap();
    let heartbeat = i64::try_from(HEARTBEAT.as_millis()).unwrap();
    let mut now = 1_000;
    for _ in 0..20 {
        tick(&connection, &project(), &holders, &settings, now).unwrap();
        now += heartbeat;
    }
    let shown = view(&connection, &project()).unwrap();
    assert_eq!(
        (shown.running.len(), shown.waiting.len()),
        (1, 1),
        "{shown:?}"
    );
    assert_eq!((shown.running[0].agent, shown.waiting[0].agent), (1, 2));
    assert!(has(&a, &first, GO) && !has(&b, &second, GO));
    first.leave();
    second.leave();
}
