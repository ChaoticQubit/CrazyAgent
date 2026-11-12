use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;
use trackwright_core::catalog::catalog;
use trackwright_core::model::OpKind;
use trackwright_core::reply::REPLY_BYTES_DEFAULT;

const BIN: &str = env!("CARGO_BIN_EXE_trackwright");

/// Runs the built binary in a folder with the caller's environment for trackwright cleared,
/// plus the given variables. Inputs: folder, arguments, extra variables. Output: the result.
fn run_with(cwd: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Output {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .current_dir(cwd)
        .env_remove("TRACKWRIGHT_DB")
        .env_remove("TRACKWRIGHT_ACTOR");
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().expect("the built trackwright binary runs")
}

/// Runs the built binary with no trackwright variables set. Inputs: folder, arguments.
/// Output: the result.
fn run(cwd: &Path, args: &[&str]) -> Output {
    run_with(cwd, args, &[])
}

/// Reads the one JSON envelope line `--json` prints. Input: the result. Output: the value.
fn envelope(output: &Output) -> Value {
    let text = std::str::from_utf8(&output.stdout).expect("stdout is UTF-8");
    assert_eq!(text.lines().count(), 1, "--json prints one line: {text}");
    let reply: Value = serde_json::from_str(text).expect("stdout is one JSON envelope");
    assert_eq!(reply["api"], 1, "{reply}");
    reply
}

/// Creates the database in `cwd/.trackwright/` and a project with that key, as the owner.
/// Inputs: folder, project key.
fn init(cwd: &Path, key: &str) {
    let output = run(cwd, &["init", "--key", key, "--actor", "owner", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(envelope(&output)["ok"], true);
}

/// Creates a ticket as the owner and returns its id and version. Inputs: folder, project key,
/// title. Output: (id, version).
fn create(cwd: &Path, key: &str, title: &str) -> (String, i64) {
    let args = [
        "create",
        "--project",
        key,
        "--title",
        title,
        "--actor",
        "owner",
    ];
    let output = run(cwd, &[&args[..], &["--json"]].concat());
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let reply = envelope(&output);
    assert_eq!(reply["ok"], true, "{reply}");
    let id = reply["data"]["id"]
        .as_str()
        .expect("create replies the short id");
    let version = reply["data"]["version"]
        .as_i64()
        .expect("create replies the version");
    (id.to_owned(), version)
}

/// The ids a list or ready reply holds in `data.tickets`. Input: the envelope. Output: the
/// ids in order.
fn row_ids(reply: &Value) -> Vec<String> {
    let rows = reply["data"]["tickets"]
        .as_array()
        .expect("a list reply carries data.tickets");
    rows.iter()
        .map(|row| row["id"].as_str().expect("every row has an id").to_owned())
        .collect()
}

#[test]
fn the_built_binary_creates_a_ticket_and_lists_it_ready_in_json() {
    let root = TempDir::new().unwrap();
    init(root.path(), "CA");
    assert!(root.path().join(".trackwright/tracker.db").is_file());
    let (id, version) = create(root.path(), "CA", "First ticket");
    assert_eq!(id, "CA-1");
    let version = version.to_string();
    let moved = run(
        root.path(),
        &[
            "move",
            "--id",
            &id,
            "--version",
            &version,
            "--status",
            "todo",
            "--actor",
            "owner",
            "--json",
        ],
    );
    assert_eq!(moved.status.code(), Some(0), "{moved:?}");
    assert_eq!(envelope(&moved)["ok"], true);
    let ready = run(
        root.path(),
        &["ready", "--project", "CA", "--actor", "agent:one", "--json"],
    );
    assert_eq!(ready.status.code(), Some(0), "{ready:?}");
    let reply = envelope(&ready);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(row_ids(&reply), ["CA-1"]);
    assert_eq!(reply["data"]["tickets"][0]["title"], "First ticket");
    assert!(ready.stderr.is_empty());
}

#[test]
fn every_catalog_op_has_a_subcommand_with_its_fields() {
    let root = TempDir::new().unwrap();
    let names: BTreeSet<&str> = catalog().iter().map(|spec| spec.kind.as_str()).collect();
    let kinds: BTreeSet<&str> = OpKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(names, kinds, "the catalog holds every operation once");
    assert_eq!(names.len(), catalog().len());
    let top = run(root.path(), &["--help"]);
    assert_eq!(top.status.code(), Some(0), "{top:?}");
    let top = String::from_utf8(top.stdout).unwrap();
    for spec in catalog() {
        let op = spec.kind.as_str();
        assert!(
            top.lines()
                .any(|line| line.trim_start().starts_with(&format!("{op} "))),
            "top help lists {op}"
        );
        let help = run(root.path(), &[op, "--help"]);
        assert_eq!(help.status.code(), Some(0), "{help:?}");
        let help = String::from_utf8(help.stdout).unwrap();
        assert!(help.starts_with(&format!("trackwright {op}")), "{help}");
        for field in spec.fields {
            let flag = format!("--{} ", field.name);
            assert!(
                help.lines()
                    .any(|line| line.trim_start().starts_with(&flag)),
                "{op} help names {flag}: {help}"
            );
        }
        let hyphenated = op.replace('_', "-");
        let again = run(root.path(), &[&hyphenated, "--help"]);
        assert_eq!(String::from_utf8(again.stdout).unwrap(), help);
    }
}

#[test]
fn human_output_names_the_next_actions() {
    let root = TempDir::new().unwrap();
    init(root.path(), "CA");
    let created = run(
        root.path(),
        &[
            "create",
            "--project",
            "CA",
            "--title",
            "Readable",
            "--actor",
            "owner",
        ],
    );
    assert_eq!(created.status.code(), Some(0), "{created:?}");
    assert!(String::from_utf8(created.stdout).unwrap().contains("CA-1"));
    let stale = [
        "move",
        "--id",
        "CA-1",
        "--version",
        "999",
        "--status",
        "todo",
        "--actor",
        "owner",
    ];
    let json = run(root.path(), &[&stale[..], &["--json"]].concat());
    assert_eq!(json.status.code(), Some(1), "{json:?}");
    let reply = envelope(&json);
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["code"], "stale_version");
    let next = reply["next"]
        .as_array()
        .expect("a refusal names next actions");
    assert!(next.iter().any(|action| action["op"] == "show"), "{reply}");
    let human = run(root.path(), &stale);
    assert_eq!(human.status.code(), Some(1), "{human:?}");
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("stale_version"), "{text}");
    for action in next {
        let op = action["op"].as_str().unwrap();
        let line = match action["id"].as_str() {
            Some(id) => format!("next: trackwright {op} --id {id}"),
            None => format!("next: trackwright {op}"),
        };
        assert!(
            text.lines().any(|l| l.starts_with(&line)),
            "{line} in {text}"
        );
    }
    assert!(
        !text.trim_start().starts_with('{'),
        "human text is not the envelope"
    );
}

#[cfg(unix)]
#[test]
fn the_database_is_found_upward_and_never_through_a_symlink() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let deep = home.join("a/b/c");
    fs::create_dir_all(&deep).unwrap();
    init(&home, "CA");
    let (id, _) = create(&deep, "CA", "Made deep below");
    let listed = run(
        &home,
        &["list", "--project", "CA", "--actor", "owner", "--json"],
    );
    assert_eq!(row_ids(&envelope(&listed)), [id]);

    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    init(&elsewhere, "EL");
    let planted = root.path().join("planted");
    fs::create_dir_all(planted.join("sub")).unwrap();
    symlink(elsewhere.join(".trackwright"), planted.join(".trackwright")).unwrap();
    let through_folder = run(
        &planted.join("sub"),
        &[
            "create",
            "--project",
            "EL",
            "--title",
            "Through a link",
            "--actor",
            "owner",
            "--json",
        ],
    );
    assert_eq!(through_folder.status.code(), Some(1), "{through_folder:?}");
    assert_eq!(envelope(&through_folder)["code"], "store_path_refused");

    let file_link = root.path().join("file-link");
    fs::create_dir_all(file_link.join(".trackwright")).unwrap();
    symlink(
        elsewhere.join(".trackwright/tracker.db"),
        file_link.join(".trackwright/tracker.db"),
    )
    .unwrap();
    let through_file = run(
        &file_link,
        &[
            "create",
            "--project",
            "EL",
            "--title",
            "Through a link",
            "--actor",
            "owner",
            "--json",
        ],
    );
    assert_eq!(through_file.status.code(), Some(1), "{through_file:?}");
    assert_eq!(envelope(&through_file)["code"], "store_path_refused");

    let db_link = root.path().join("db-link.db");
    symlink(elsewhere.join(".trackwright/tracker.db"), &db_link).unwrap();
    let flagged = run(
        root.path(),
        &[
            "create",
            "--db",
            db_link.to_str().unwrap(),
            "--project",
            "EL",
            "--title",
            "Through a link",
            "--actor",
            "owner",
            "--json",
        ],
    );
    assert_eq!(flagged.status.code(), Some(1), "{flagged:?}");
    assert_eq!(envelope(&flagged)["code"], "store_path_refused");

    let untouched = run(
        &elsewhere,
        &["list", "--project", "EL", "--actor", "owner", "--json"],
    );
    assert_eq!(row_ids(&envelope(&untouched)), Vec::<String>::new());

    let nowhere = root.path().join("nowhere");
    fs::create_dir(&nowhere).unwrap();
    let missing = run(
        &nowhere,
        &["list", "--project", "CA", "--actor", "owner", "--json"],
    );
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert_eq!(envelope(&missing)["code"], "store_not_found");
}

#[test]
fn the_db_flag_beats_the_environment_which_beats_the_upward_search() {
    let root = TempDir::new().unwrap();
    init(root.path(), "UP");
    let env_db = root.path().join("env/tracker.db");
    let flag_db = root.path().join("flag/tracker.db");
    for (path, key) in [(&env_db, "EN"), (&flag_db, "FL")] {
        let output = run(
            root.path(),
            &[
                "init",
                "--db",
                path.to_str().unwrap(),
                "--key",
                key,
                "--actor",
                "owner",
            ],
        );
        assert_eq!(output.status.code(), Some(0), "{output:?}");
    }
    let env = [("TRACKWRIGHT_DB", env_db.as_os_str())];
    let list = |key: &str, flag: bool, env: &[(&str, &OsStr)]| {
        let mut args = vec!["list", "--project", key, "--actor", "owner", "--json"];
        if flag {
            args.extend(["--db", flag_db.to_str().unwrap()]);
        }
        envelope(&run_with(root.path(), &args, env))
    };
    assert_eq!(list("FL", true, &env)["ok"], true);
    assert_eq!(list("EN", true, &env)["code"], "project_unknown");
    assert_eq!(list("EN", false, &env)["ok"], true);
    assert_eq!(list("UP", false, &env)["code"], "project_unknown");
    assert_eq!(list("UP", false, &[])["ok"], true);
    let actor = [("TRACKWRIGHT_ACTOR", OsStr::new("owner"))];
    let by_env = run_with(root.path(), &["list", "--project", "UP", "--json"], &actor);
    assert_eq!(envelope(&by_env)["ok"], true, "{by_env:?}");
}

#[test]
fn a_bad_flag_exits_2_with_one_line() {
    let root = TempDir::new().unwrap();
    let cases: &[&[&str]] = &[
        &[],
        &["nosuchop", "--actor", "owner"],
        &[
            "create",
            "--titel",
            "x",
            "--project",
            "CA",
            "--actor",
            "owner",
        ],
        &["create", "--project", "CA", "--actor", "owner", "--title"],
        &[
            "create",
            "--title",
            "a",
            "--title",
            "b",
            "--project",
            "CA",
            "--actor",
            "owner",
        ],
        &[
            "move",
            "--id",
            "CA-1",
            "--version",
            "two",
            "--status",
            "todo",
            "--actor",
            "owner",
        ],
        &["list", "stray", "--project", "CA", "--actor", "owner"],
        &["list", "--project", "CA"],
        &["list", "--project", "CA", "--actor", "owner", "--json=yes"],
        &["list", "--project", "CA", "--actor", "a", "--actor", "b"],
        &["init", "--name", "No key", "--actor", "owner"],
        &["mcp"],
        &["list", "-x", "--actor", "owner"],
    ];
    for args in cases {
        for json in [false, true] {
            let mut args = args.to_vec();
            if json {
                args.push("--json");
            }
            let output = run(root.path(), &args);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
            assert!(stderr.starts_with("trackwright: "), "{args:?}: {stderr}");
            assert!(stderr.ends_with('\n'));
        }
    }
    assert!(
        fs::read_dir(root.path()).unwrap().next().is_none(),
        "usage errors write nothing"
    );
}

#[test]
fn trackwright_schema_prints_schema_v1_json() {
    let root = TempDir::new().unwrap();
    let output = run(root.path(), &["schema"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let published = include_str!("../../trackwright-core/schema/v1.json");
    assert_eq!(std::str::from_utf8(&output.stdout).unwrap(), published);
    assert!(
        fs::read_dir(root.path()).unwrap().next().is_none(),
        "the plain schema reads no database"
    );
}

#[test]
fn schema_json_replies_the_core_schema_op_like_every_front_end() {
    let root = TempDir::new().unwrap();
    let anonymous = run(root.path(), &["schema", "--json"]);
    assert_eq!(anonymous.status.code(), Some(2), "{anonymous:?}");
    assert!(anonymous.stdout.is_empty(), "{anonymous:?}");
    init(root.path(), "CA");
    let listed = run(root.path(), &["schema", "--actor", "owner", "--json"]);
    assert_eq!(listed.status.code(), Some(0), "{listed:?}");
    assert!(listed.stdout.len() <= REPLY_BYTES_DEFAULT + 1, "{listed:?}");
    let listed = envelope(&listed);
    assert_eq!(listed["ok"], true, "{listed}");
    assert_eq!(listed["data"]["api"], 1, "{listed}");
    let ops: BTreeSet<&str> = listed["data"]["ops"]
        .as_array()
        .expect("the schema op lists the operations")
        .iter()
        .map(|entry| entry["op"].as_str().expect("every entry names its op"))
        .collect();
    let catalog_ops: BTreeSet<&str> = catalog().iter().map(|spec| spec.kind.as_str()).collect();
    assert!(ops.is_subset(&catalog_ops), "{listed}");
    assert!(ops.contains("create"), "{listed}");
    let one = run(
        root.path(),
        &["schema", "--op", "create", "--actor", "owner", "--json"],
    );
    let one = envelope(&one);
    assert_eq!(one["ok"], true, "{one}");
    assert_eq!(one["data"]["op"]["op"], "create", "{one}");
}

#[test]
fn init_below_a_database_refuses_and_names_it() {
    let root = TempDir::new().unwrap();
    init(root.path(), "CA");
    let sub = root.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let nested = run(&sub, &["init", "--key", "XY", "--actor", "owner", "--json"]);
    assert_eq!(nested.status.code(), Some(1), "{nested:?}");
    let refusal = envelope(&nested);
    assert_eq!(refusal["code"], "store_exists_above", "{refusal}");
    let above = root
        .path()
        .canonicalize()
        .unwrap()
        .join(".trackwright/tracker.db");
    let message = refusal["message"].as_str().unwrap();
    assert!(message.contains(above.to_str().unwrap()), "{refusal}");
    assert!(
        message.contains("--db ./.trackwright/tracker.db"),
        "{refusal}"
    );
    assert!(
        !sub.join(".trackwright").exists(),
        "a refused init writes nothing"
    );

    let added = run(
        &sub,
        &[
            "init",
            "--key",
            "XY",
            "--db",
            above.to_str().unwrap(),
            "--actor",
            "owner",
            "--json",
        ],
    );
    assert_eq!(added.status.code(), Some(0), "{added:?}");
    for key in ["CA", "XY"] {
        let listed = run(
            &sub,
            &["list", "--project", key, "--actor", "owner", "--json"],
        );
        assert_eq!(envelope(&listed)["ok"], true, "{key}: {listed:?}");
    }
    let again = run(
        root.path(),
        &["init", "--key", "ZZ", "--actor", "owner", "--json"],
    );
    assert_eq!(envelope(&again)["ok"], true, "{again:?}");

    let own = root.path().join("own");
    fs::create_dir(&own).unwrap();
    let separate = run(
        &own,
        &[
            "init",
            "--key",
            "OW",
            "--db",
            "./.trackwright/tracker.db",
            "--actor",
            "owner",
            "--json",
        ],
    );
    assert_eq!(envelope(&separate)["ok"], true, "{separate:?}");
    assert!(own.join(".trackwright/tracker.db").is_file());
    let mine = run(
        &own,
        &["list", "--project", "OW", "--actor", "owner", "--json"],
    );
    assert_eq!(envelope(&mine)["ok"], true, "{mine:?}");
    let hidden = run(
        &own,
        &["list", "--project", "CA", "--actor", "owner", "--json"],
    );
    assert_eq!(envelope(&hidden)["code"], "project_unknown", "{hidden:?}");
}
