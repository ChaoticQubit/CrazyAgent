use crate::credentials::test_keychain::TestKeychain;
use kernel_security::error::StoreError;
use kernel_security::keychain::{
    KEYCHAIN_SUFFIX, LOGIN_KEYCHAIN, SECURITY, forget_record, fresh_profile_keychain,
    open_profile_keychain, prepare_profile_keychain,
};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn read(home: &Path, args: &[&str]) -> String {
    let output = kernel_processes::spawn_gate::output(
        Command::new(SECURITY)
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "security {args:?} failed");
    String::from_utf8(output.stdout).unwrap()
}

fn shown(home: &Path, args: &[&str]) -> String {
    let output = kernel_processes::spawn_gate::output(
        Command::new(SECURITY)
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn private_home_gets_its_own_keychain_that_no_other_home_sees() {
    let root = tempfile::Builder::new()
        .prefix("ca-keychain-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let home = root.path().join("home");
    let other = root.path().join("other");
    for folder in [&home, &other] {
        std::fs::create_dir(folder).unwrap();
    }
    let keychain = prepare_profile_keychain(&home, false).unwrap();
    assert_eq!(keychain, profile_keychain_path(&home));
    let text = keychain.to_str().unwrap().to_owned();
    assert!(read(&home, &["default-keychain", "-d", "user"]).contains(&text));
    assert!(read(&home, &["list-keychains", "-d", "user"]).contains(&text));
    let adopted = TestKeychain::adopt(&home, keychain.clone());
    adopted.add("probe", "crazyagent-keychain-test", "value");
    assert_eq!(adopted.find("crazyagent-keychain-test"), "value");
    assert_eq!(prepare_profile_keychain(&home, false).unwrap(), keychain);
    assert_eq!(adopted.find("crazyagent-keychain-test"), "value");
    for args in [
        ["default-keychain", "-d", "user"],
        ["list-keychains", "-d", "user"],
    ] {
        assert!(
            !shown(&other, &args).contains(&text),
            "another home sees the profile keychain through {args:?}"
        );
    }
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&keychain).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(prepare_profile_keychain(Path::new("relative"), false).is_err());
}

fn entries(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn keychain_folder_links_are_refused_untouched_and_a_sandboxed_agent_cannot_plant_them() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::Builder::new()
        .prefix("ca-keychain-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let home = root.path().canonicalize().unwrap().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let agent = crate::sandbox::test_agent::TestAgent::with_home(&home);
    let preferences = agent.peer.join("Preferences");
    let keychains = agent.peer.join("Keychains");
    std::fs::create_dir(&preferences).unwrap();
    std::fs::create_dir(&keychains).unwrap();
    let planting = agent.run(&format!(
        "mkdir -p '{home}/Library' && ln -s '{preferences}' '{home}/Library/Preferences' \
         && ln -s '{keychains}' '{home}/Library/Keychains'",
        home = home.display(),
        preferences = preferences.display(),
        keychains = keychains.display()
    ));
    assert!(!planting.status.success());
    assert!(std::fs::symlink_metadata(home.join("Library/Preferences")).is_err());
    assert!(std::fs::symlink_metadata(home.join("Library/Keychains")).is_err());

    std::fs::create_dir_all(home.join("Library")).unwrap();
    symlink(&preferences, home.join("Library/Preferences")).unwrap();
    symlink(&keychains, home.join("Library/Keychains")).unwrap();
    assert!(prepare_profile_keychain(&home, false).is_err());
    assert!(fresh_profile_keychain(root.path(), &home, &[]).is_err());
    assert!(
        forget_record(
            &home,
            &home.join("Library/Keychains/login.keychain-db"),
            "crazyagent-keychain-test"
        )
        .is_err()
    );
    assert_eq!(entries(&preferences), Vec::<String>::new());
    assert_eq!(entries(&keychains), Vec::<String>::new());

    let planted = agent.peer.join("keychain");
    std::fs::write(&planted, "peer").unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o644)).unwrap();
    let replanting = agent.run(&format!(
        "cd '{home}/Library' && rm Preferences Keychains && mkdir Keychains \
         && ln -s '{planted}' Keychains/login.keychain-db",
        home = home.display(),
        planted = planted.display()
    ));
    assert!(!replanting.status.success());
    std::fs::remove_file(home.join("Library/Preferences")).unwrap();
    std::fs::remove_file(home.join("Library/Keychains")).unwrap();
    std::fs::create_dir(home.join("Library/Keychains")).unwrap();
    symlink(&planted, home.join("Library/Keychains/login.keychain-db")).unwrap();
    assert!(prepare_profile_keychain(&home, false).is_err());
    assert!(fresh_profile_keychain(root.path(), &home, &[]).is_err());
    assert!(
        forget_record(
            &home,
            &home.join("Library/Keychains/login.keychain-db"),
            "crazyagent-keychain-test"
        )
        .is_err()
    );
    std::fs::rename(
        home.join("Library/Keychains/login.keychain-db"),
        profile_keychain_path(&home),
    )
    .unwrap();
    assert!(prepare_profile_keychain(&home, false).is_err());
    assert!(open_profile_keychain(&home).is_err());
    assert!(
        std::fs::symlink_metadata(profile_keychain_path(&home))
            .is_ok_and(|found| found.is_symlink())
    );
    assert_eq!(std::fs::read_to_string(&planted).unwrap(), "peer");
    assert_eq!(
        std::fs::metadata(&planted).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(entries(&home.join("Library")), ["Keychains"]);
}

fn private_home(root: &Path, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let home = root.canonicalize().unwrap().join(name);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    home
}

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ca-keychain-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap()
}

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn forgetting_a_record_deletes_every_item_of_that_service_and_nothing_else() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let bare = private_home(root.path(), "bare");
    forget_record(
        &bare,
        &bare.join("Library/Keychains/agent-0123456789abcdef.keychain-db"),
        "crazyagent-forget-test",
    )
    .unwrap();
    assert_eq!(entries(&bare), Vec::<String>::new());
    let keychain = fresh_profile_keychain(root.path(), &home, &[]).unwrap();
    let adopted = TestKeychain::adopt(&home, keychain.clone());
    adopted.add("probe", "crazyagent-forget-test", "stale");
    adopted.add("other", "crazyagent-forget-test", "stale too");
    adopted.add("probe", "crazyagent-keep-test", "kept");
    assert!(adopted.holds("crazyagent-forget-test"));

    forget_record(&home, &keychain, "crazyagent-forget-test").unwrap();
    assert!(!adopted.holds("crazyagent-forget-test"));
    assert_eq!(adopted.find("crazyagent-keep-test"), "kept");
    forget_record(&home, &keychain, "crazyagent-forget-test").unwrap();
    assert!(
        forget_record(
            Path::new("relative"),
            Path::new("relative/Library/Keychains/login.keychain-db"),
            "crazyagent-forget-test"
        )
        .is_err()
    );
    assert!(
        forget_record(
            &home,
            &bare.join("Library/Keychains/login.keychain-db"),
            "crazyagent-forget-test"
        )
        .is_err()
    );
    assert!(
        forget_record(
            &home,
            &home.join("login.keychain-db"),
            "crazyagent-forget-test"
        )
        .is_err()
    );
    let profile = private_home(root.path(), "profile");
    let login = TestKeychain::adopt(&profile, prepare_profile_keychain(&profile, false).unwrap());
    login.add("probe", "crazyagent-forget-test", "signed in");
    for named in [
        login.path().to_path_buf(),
        home.join("Library/Keychains/login.keychain-db"),
        home.join("Library/Keychains/agent-0123456789ABCDEF.keychain-db"),
        home.join("Library/Keychains/agent-0123.keychain-db"),
    ] {
        let owner = named.parent().unwrap().parent().unwrap().parent().unwrap();
        assert!(forget_record(owner, &named, "crazyagent-forget-test").is_err());
    }
    assert_eq!(login.find("crazyagent-forget-test"), "signed in");
}

fn keychain_files(home: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(home.join("Library/Keychains"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_str().unwrap().ends_with(".keychain-db"))
        .collect();
    found.sort();
    found
}

#[test]
fn a_fresh_keychain_under_a_new_name_is_the_default_before_the_locked_one_is_deleted() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let earlier = TestKeychain::adopt(
        &home,
        fresh_profile_keychain(root.path(), &home, &[]).unwrap(),
    );
    let earlier_text = earlier.path().to_str().unwrap().to_owned();
    earlier.add("probe", "crazyagent-fresh-test", "old");
    earlier.security(&["lock-keychain", &earlier_text], None);

    let fresh = fresh_profile_keychain(root.path(), &home, &[]).unwrap();
    assert_ne!(fresh, earlier.path());
    assert_eq!(fresh.parent(), earlier.path().parent());
    assert!(!earlier.path().exists());
    assert_eq!(keychain_files(&home), std::slice::from_ref(&fresh));
    let keychain = TestKeychain::adopt(&home, fresh.clone());
    let text = fresh.to_str().unwrap();
    for which in ["default-keychain", "list-keychains"] {
        let setting = keychain.setting(which);
        assert!(setting.contains(text) && !setting.contains(&earlier_text));
    }
    assert!(!keychain.holds("crazyagent-fresh-test"));
    keychain.add("probe", "crazyagent-fresh-test", "new");
    assert_eq!(keychain.find("crazyagent-fresh-test"), "new");

    let again = TestKeychain::adopt(
        &home,
        fresh_profile_keychain(root.path(), &home, &[]).unwrap(),
    );
    assert_ne!(again.path(), fresh);
    assert!(!fresh.exists());
    assert_eq!(keychain_files(&home), [again.path().to_path_buf()]);
    assert!(
        again
            .setting("default-keychain")
            .contains(again.path().to_str().unwrap())
    );
    assert!(fresh_profile_keychain(root.path(), Path::new("relative"), &[]).is_err());
}

#[test]
fn preparing_one_home_leaves_another_prepared_homes_default_and_search_list_unchanged() {
    let root = scratch();
    let first = private_home(root.path(), "first");
    let second = private_home(root.path(), "second");
    let second_keychain =
        TestKeychain::adopt(&second, prepare_profile_keychain(&second, false).unwrap());
    let settings = |keychain: &TestKeychain| {
        [
            keychain.setting("default-keychain"),
            keychain.setting("list-keychains"),
        ]
    };
    let before = settings(&second_keychain);
    let first_keychain =
        TestKeychain::adopt(&first, prepare_profile_keychain(&first, false).unwrap());
    assert_eq!(settings(&second_keychain), before);
    let second_text = second_keychain.path().to_str().unwrap();
    let first_text = first_keychain.path().to_str().unwrap();
    for text in &before {
        assert!(text.contains(second_text) && !text.contains(first_text));
    }
    for text in settings(&first_keychain) {
        assert!(text.contains(first_text) && !text.contains(second_text));
    }
}

#[test]
fn a_write_naming_no_keychain_lands_in_the_prepared_keychain_the_sandbox_sees_as_default() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let keychain = TestKeychain::adopt(&home, prepare_profile_keychain(&home, false).unwrap());
    let text = keychain.path().to_str().unwrap().to_owned();
    assert!(
        keychain.setting("default-keychain").contains(&text),
        "no write may name no keychain unless the home's default is the prepared one"
    );
    let line = format!(
        "add-generic-password -U -a \"probe\" -s \"crazyagent-unnamed-test\" -X {}\n",
        hex("unnamed")
    );
    let mut child = kernel_processes::spawn_gate::spawn(
        Command::new(SECURITY)
            .arg("-i")
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }
    assert!(child.wait().unwrap().success());
    assert_eq!(keychain.find("crazyagent-unnamed-test"), "unnamed");

    let agent = crate::sandbox::test_agent::TestAgent::with_home(&home);
    let seen = agent.run("/usr/bin/security default-keychain -d user");
    assert!(seen.status.success());
    assert!(
        String::from_utf8_lossy(&seen.stdout).contains(&text),
        "inside the sandbox the harness would not see the prepared keychain as its default"
    );
}

#[test]
fn a_sandboxed_agent_cannot_move_link_or_rewrite_its_keychain_or_search_list() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let keychain = TestKeychain::adopt(&home, prepare_profile_keychain(&home, false).unwrap());
    let before = [
        keychain.setting("default-keychain"),
        keychain.setting("list-keychains"),
    ];
    let plist = home.join("Library/Preferences/com.apple.security.plist");
    let settings = std::fs::read(&plist).ok();
    let agent = crate::sandbox::test_agent::TestAgent::with_home(&home);
    for script in [
        "mv \"$HOME/Library\" \"$HOME/moved\"",
        "mv \"$HOME/Library/Keychains\" \"$HOME/moved\"",
        "mv \"$HOME/Library/Preferences\" \"$HOME/moved\"",
        "rm \"$HOME/Library/Keychains/profile.keychain-db\"",
        "mv \"$HOME/Library/Keychains/profile.keychain-db\" \"$HOME/moved\"",
        "printf x >> \"$HOME/Library/Keychains/profile.keychain-db\"",
        "printf x > \"$HOME/Library/Keychains/login.keychain-db\"",
        "ln -s /tmp \"$HOME/Library/Keychains/other.keychain-db\"",
        "printf x > \"$HOME/Library/Preferences/com.apple.security.plist\"",
        "rm \"$HOME/Library/Preferences/com.apple.security.plist\"",
        "ln -s /tmp \"$HOME/Library/preferences/COM.APPLE.SECURITY.PLIST.new\" && mv \"$HOME/Library/preferences/COM.APPLE.SECURITY.PLIST.new\" \"$HOME/Library/preferences/COM.APPLE.SECURITY.PLIST\"",
        "chmod 0755 \"$HOME/Library\"",
    ] {
        assert!(
            !agent.run(script).status.success(),
            "the agent ran: {script}"
        );
    }
    agent.shell(
        "mkdir -p \"$HOME/Library/Caches/tool\" && printf x > \"$HOME/Library/Caches/tool/file\" \
         && printf x > \"$HOME/Library/Preferences/tool.plist\"",
    );
    assert_eq!(std::fs::read(&plist).ok(), settings);
    assert_eq!(
        [
            keychain.setting("default-keychain"),
            keychain.setting("list-keychains"),
        ],
        before
    );
    assert_eq!(
        prepare_profile_keychain(&home, false).unwrap(),
        keychain.path()
    );
}

#[test]
fn removing_an_agents_files_takes_its_keychain_and_leaves_other_homes_untouched() {
    let root = scratch();
    let team = root.path().canonicalize().unwrap().join("team");
    std::fs::create_dir(&team).unwrap();
    let home = private_home(&team, "agents/agent-7/home");
    let other = private_home(root.path(), "other");
    let other_keychain =
        TestKeychain::adopt(&other, prepare_profile_keychain(&other, false).unwrap());
    let before = [
        other_keychain.setting("default-keychain"),
        other_keychain.setting("list-keychains"),
    ];
    let keychain = fresh_profile_keychain(&team, &home, &[]).unwrap();
    assert!(keychain.is_file());

    crate::team::cleanup::remove_agent_files(&team, 7).unwrap();
    assert!(!keychain.exists());
    assert!(!team.join("agents/agent-7").exists());
    let after = [
        other_keychain.setting("default-keychain"),
        other_keychain.setting("list-keychains"),
    ];
    assert_eq!(after, before);
    assert!(
        after
            .iter()
            .all(|text| !text.contains(keychain.to_str().unwrap()))
    );
}

#[test]
fn an_agent_keychain_is_refused_outside_the_root_through_a_link_or_beside_a_login_keychain() {
    use std::os::unix::fs::symlink;
    let root = scratch();
    let engine = private_home(root.path(), "engine");
    let outside = private_home(root.path(), "outside");
    let profile = private_home(&engine, "native-profiles/k/home");
    let login = TestKeychain::adopt(&profile, prepare_profile_keychain(&profile, false).unwrap());
    login.add("probe", "crazyagent-guard-test", "signed in");
    let before = login.setting("default-keychain");
    let agent = private_home(&engine, "agents/agent-1/home");
    let linked = engine.join("linked");
    symlink(&agent, &linked).unwrap();
    let none: Vec<&Path> = Vec::new();
    for (home, sharers) in [
        (outside.as_path(), none.clone()),
        (engine.as_path(), none.clone()),
        (profile.as_path(), none.clone()),
        (linked.as_path(), none.clone()),
        (agent.as_path(), vec![profile.as_path()]),
        (agent.as_path(), vec![outside.as_path()]),
        (agent.as_path(), vec![agent.as_path()]),
    ] {
        assert!(
            fresh_profile_keychain(&engine, home, &sharers).is_err(),
            "{home:?} with {sharers:?} got an agent keychain"
        );
    }
    for home in [&outside, &agent] {
        assert!(
            std::fs::symlink_metadata(home.join("Library")).is_err(),
            "a refused handoff wrote into {home:?}"
        );
    }
    assert_eq!(login.find("crazyagent-guard-test"), "signed in");
    assert_eq!(login.setting("default-keychain"), before);
    assert_eq!(keychain_files(&profile), [login.path().to_path_buf()]);
}

#[test]
fn a_profile_home_reached_through_a_link_keeps_its_login_keychain_name() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let legacy = TestKeychain::create_named(&home, LOGIN_KEYCHAIN);
    let link = root.path().canonicalize().unwrap().join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    let before = identity(legacy.path());
    assert_eq!(
        open_profile_keychain(&link),
        Err(StoreError::code("native_auth_claude_keychain_unavailable"))
    );
    assert_eq!(
        prepare_profile_keychain(&link, true),
        Err(StoreError::code("native_auth_claude_keychain_unavailable"))
    );
    assert_eq!(identity(legacy.path()), before);
    assert!(std::fs::symlink_metadata(profile_keychain_path(&home)).is_err());
}

#[test]
fn leftover_agent_keychains_past_the_old_cap_are_swept_and_other_keychains_stay() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let directory = home.join("Library/Keychains");
    let mut leftovers = vec![fresh_profile_keychain(root.path(), &home, &[]).unwrap()];
    for index in 0..10_u64 {
        let leftover = directory.join(format!("agent-{:016x}.keychain-db", 0xfeed_0000 + index));
        read(
            &home,
            &["create-keychain", "-p", "", leftover.to_str().unwrap()],
        );
        leftovers.push(leftover);
    }
    let foreign = directory.join("notes.keychain-db");
    read(
        &home,
        &["create-keychain", "-p", "", foreign.to_str().unwrap()],
    );
    let foreign = TestKeychain::adopt(&home, foreign);

    let fresh = TestKeychain::adopt(
        &home,
        fresh_profile_keychain(root.path(), &home, &[]).unwrap(),
    );
    assert!(leftovers.iter().all(|old| !old.exists()));
    assert_eq!(
        keychain_files(&home),
        [fresh.path().to_path_buf(), foreign.path().to_path_buf()]
    );
    for which in ["default-keychain", "list-keychains"] {
        let setting = fresh.setting(which);
        assert!(setting.contains(fresh.path().to_str().unwrap()));
        assert!(!setting.contains(foreign.path().to_str().unwrap()));
    }
}

#[test]
fn a_sharing_home_follows_the_new_keychain_before_the_old_one_is_deleted() {
    let root = scratch();
    let lead = private_home(root.path(), "agents/agent-3/home");
    let mates = private_home(root.path(), "agents/agent-3/mates-home");
    let first = TestKeychain::adopt(
        &lead,
        fresh_profile_keychain(root.path(), &lead, &[&mates]).unwrap(),
    );
    let first_text = first.path().to_str().unwrap().to_owned();
    for which in [
        ["default-keychain", "-d", "user"],
        ["list-keychains", "-d", "user"],
    ] {
        assert!(read(&mates, &which).contains(&first_text));
    }

    let second = TestKeychain::adopt(
        &lead,
        fresh_profile_keychain(root.path(), &lead, &[&mates]).unwrap(),
    );
    assert!(!first.path().exists());
    let second_text = second.path().to_str().unwrap();
    for which in [
        ["default-keychain", "-d", "user"],
        ["list-keychains", "-d", "user"],
    ] {
        let setting = read(&mates, &which);
        assert!(setting.contains(second_text) && !setting.contains(&first_text));
    }
    assert!(
        std::fs::read_dir(mates.join("Library/Keychains")).map_or(true, |found| found
            .map(|entry| entry.unwrap().file_name())
            .all(|name| !name.to_string_lossy().ends_with(KEYCHAIN_SUFFIX))),
        "the sharing home got a keychain of its own"
    );
}

fn profile_keychain_path(home: &Path) -> PathBuf {
    home.join("Library/Keychains/profile.keychain-db")
}

fn aside_path(home: &Path) -> PathBuf {
    home.join("Library/Keychains/profile-locked.keychain-db")
}

fn profile_folders(home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for folder in ["Library", "Library/Keychains", "Library/Preferences"] {
        std::fs::create_dir_all(home.join(folder)).unwrap();
        std::fs::set_permissions(home.join(folder), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
}

fn locked_with_another_password(home: &Path, path: PathBuf) -> TestKeychain {
    profile_folders(home);
    let keychain = TestKeychain::adopt(home, path);
    let text = keychain.path().to_str().unwrap().to_owned();
    keychain.security(&["create-keychain", "-p", "crazyagent-dummy", &text], None);
    keychain.security(&["list-keychains", "-d", "user", "-s", &text], None);
    keychain.security(&["default-keychain", "-d", "user", "-s", &text], None);
    keychain.security(&["lock-keychain", &text], None);
    keychain
}

fn identity(path: &Path) -> (u64, Vec<u8>) {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file());
    (metadata.ino(), std::fs::read(path).unwrap())
}

#[test]
fn a_profile_keychain_the_empty_password_cannot_open_is_set_aside_at_sign_in_and_replaced() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let keychain = locked_with_another_password(&home, profile_keychain_path(&home));
    let old = identity(keychain.path());
    assert_eq!(
        prepare_profile_keychain(&home, true).unwrap(),
        profile_keychain_path(&home)
    );
    let _aside = TestKeychain::adopt(&home, aside_path(&home));
    assert_eq!(identity(&aside_path(&home)), old);
    assert_ne!(identity(keychain.path()).0, old.0);
    let text = keychain.path().to_str().unwrap().to_owned();
    keychain.security(&["lock-keychain", &text], None);
    keychain.security(&["unlock-keychain", "-p", "", &text], None);
    keychain.add("crazyagent", "crazyagent-aside-test", "new");
    assert_eq!(keychain.find("crazyagent-aside-test"), "new");
    assert!(keychain.setting("default-keychain").contains(&text));
    assert!(keychain.setting("list-keychains").contains(&text));
}

#[test]
fn a_keychain_the_empty_password_cannot_open_stays_in_place_unless_a_login_follows() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let keychain = locked_with_another_password(&home, profile_keychain_path(&home));
    let before = identity(keychain.path());
    assert!(prepare_profile_keychain(&home, false).is_err());
    assert_eq!(identity(keychain.path()), before);
    assert!(std::fs::symlink_metadata(aside_path(&home)).is_err());
}

#[test]
fn a_sign_in_keeps_both_files_when_the_aside_name_is_already_taken() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let keychain = locked_with_another_password(&home, profile_keychain_path(&home));
    std::fs::write(aside_path(&home), "taken").unwrap();
    let state = || {
        (
            identity(keychain.path()),
            identity(&aside_path(&home)),
            keychain.setting("default-keychain"),
            keychain.setting("list-keychains"),
        )
    };
    let before = state();
    assert!(prepare_profile_keychain(&home, true).is_err());
    assert_eq!(state(), before);
}

#[test]
fn a_sign_in_refuses_a_link_at_the_profile_keychain_and_leaves_its_target_untouched() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let target = locked_with_another_password(&home, home.join("target.keychain-db"));
    std::os::unix::fs::symlink(target.path(), profile_keychain_path(&home)).unwrap();
    let before = identity(target.path());
    assert!(prepare_profile_keychain(&home, true).is_err());
    assert_eq!(identity(target.path()), before);
    assert_eq!(
        std::fs::read_link(profile_keychain_path(&home)).unwrap(),
        target.path()
    );
    assert!(std::fs::symlink_metadata(aside_path(&home)).is_err());
}

#[test]
fn a_profile_keychain_the_empty_password_cannot_open_is_refused_and_its_settings_stay() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let keychain = locked_with_another_password(&home, profile_keychain_path(&home));
    let before = [
        keychain.setting("default-keychain"),
        keychain.setting("list-keychains"),
    ];
    assert!(open_profile_keychain(&home).is_err());
    let after = [
        keychain.setting("default-keychain"),
        keychain.setting("list-keychains"),
    ];
    assert_eq!(after, before);
    let metadata = std::fs::symlink_metadata(keychain.path()).unwrap();
    assert!(metadata.is_file() && metadata.nlink() == 1);
}

#[test]
fn a_locked_profile_keychain_opens_with_the_empty_password_without_a_prompt() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let keychain = TestKeychain::adopt(&home, prepare_profile_keychain(&home, false).unwrap());
    let text = keychain.path().to_str().unwrap().to_owned();
    keychain.add("probe", "crazyagent-locked-test", "signed in");
    keychain.security(&["lock-keychain", &text], None);
    keychain.security(&["unlock-keychain", "-p", "", &text], None);
    keychain.security(&["lock-keychain", &text], None);
    assert_eq!(
        prepare_profile_keychain(&home, false).unwrap(),
        keychain.path()
    );
    assert_eq!(keychain.find("crazyagent-locked-test"), "signed in");
}

#[test]
fn an_earlier_engines_login_keychain_is_renamed_and_found_through_the_search_list() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let legacy = TestKeychain::create_named(&home, LOGIN_KEYCHAIN);
    legacy.add("probe", "crazyagent-rename-test", "signed in");
    let keychain = prepare_profile_keychain(&home, false).unwrap();
    assert_eq!(keychain, profile_keychain_path(&home));
    assert!(!legacy.path().exists());
    let keychain = TestKeychain::adopt(&home, keychain);
    let text = keychain.path().to_str().unwrap().to_owned();
    for which in ["default-keychain", "list-keychains"] {
        assert!(keychain.setting(which).contains(&text), "{which}");
    }
    assert_eq!(keychain.find("crazyagent-rename-test"), "signed in");
    let found = read(
        &home,
        &[
            "find-generic-password",
            "-s",
            "crazyagent-rename-test",
            "-w",
        ],
    );
    assert_eq!(found.trim_end(), "signed in");
}

#[test]
fn a_locked_login_keychain_an_earlier_engine_made_is_renamed_opened_and_read() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let legacy = TestKeychain::create_named(&home, LOGIN_KEYCHAIN);
    legacy.add("probe", "crazyagent-locked-rename-test", "signed in");
    let text = legacy.path().to_str().unwrap().to_owned();
    legacy.security(&["lock-keychain", &text], None);
    let keychain = TestKeychain::adopt(&home, prepare_profile_keychain(&home, false).unwrap());
    assert_eq!(keychain.path(), profile_keychain_path(&home));
    assert!(!legacy.path().exists());
    assert_eq!(keychain.find("crazyagent-locked-rename-test"), "signed in");
}

#[test]
fn a_profile_home_holding_both_keychain_names_is_refused_and_both_stay() {
    let root = scratch();
    let home = private_home(root.path(), "home");
    let legacy = TestKeychain::create_named(&home, LOGIN_KEYCHAIN);
    legacy.add("probe", "crazyagent-both-test", "old");
    let current = TestKeychain::create_named(&home, "profile.keychain-db");
    current.add("probe", "crazyagent-both-test", "new");
    assert!(prepare_profile_keychain(&home, false).is_err());
    assert_eq!(legacy.find("crazyagent-both-test"), "old");
    assert_eq!(current.find("crazyagent-both-test"), "new");
}

#[test]
fn an_agent_keychain_is_refused_in_a_home_holding_any_profile_keychain_name() {
    let root = scratch();
    let engine = private_home(root.path(), "engine");
    let names = [
        LOGIN_KEYCHAIN,
        "profile.keychain-db",
        "profile-locked.keychain-db",
    ];
    for (index, name) in names.iter().enumerate() {
        let home = private_home(&engine, &format!("agents/agent-{index}/home"));
        let profile = TestKeychain::create_named(&home, name);
        let before = profile.setting("default-keychain");
        assert!(
            fresh_profile_keychain(&engine, &home, &[]).is_err(),
            "{name}"
        );
        assert_eq!(profile.setting("default-keychain"), before, "{name}");
    }
}
