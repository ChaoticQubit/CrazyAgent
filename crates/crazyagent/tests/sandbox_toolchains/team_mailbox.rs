use super::*;

/// SECAUDIT-8-R2-L1: an empty tool mailbox is as fixed as one holding answers. `rename` can
/// replace an empty folder, so neither agent of a native team replaces it or swaps it with a
/// folder of its own. Each first moves that folder, so the refusal is the mailbox's own.
#[test]
fn no_agent_of_a_native_team_replaces_or_swaps_an_empty_tool_mailbox() {
    use std::os::unix::fs::MetadataExt;
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    let pinned = fs::metadata(&mailbox).unwrap().ino();
    assert_eq!(fs::read_dir(&mailbox).unwrap().count(), 0);
    for who in ["teammate", "lead"] {
        let place = |name: &str| team.case.tmp.join(format!("{name}-{who}"));
        let (made, moved) = (place("made"), place("moved"));
        fs::create_dir(&made).unwrap();
        fs::set_permissions(&made, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(made.join("forged.reply"), b"forged").unwrap();
        let (from, to, folder) = (quoted(&made), quoted(&moved), quoted(&mailbox));
        let script = format!(
            "swap() {{ /usr/bin/perl -e '$r = syscall(488, -2, $ARGV[0], -2, $ARGV[1], 2); exit($r == 0 ? 0 : 1)' \"$1\" \"$2\"; }}\n\
             rename() {{ /usr/bin/perl -e 'exit(rename($ARGV[0], $ARGV[1]) ? 0 : 1)' \"$1\" \"$2\"; }}\n\
             rename {from} {to}; echo control=$?\n\
             rename {to} {folder}; echo replaced=$?\n\
             swap {to} {folder}; echo swappedfolder=$?\n"
        );
        let output = if who == "lead" {
            team.as_lead(&script)
        } else {
            team.teammate(&script)
        };
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let errors = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(status_of(&text, "control"), "0", "{who}: {errors}");
        for name in ["replaced", "swappedfolder"] {
            assert_ne!(status_of(&text, name), "0", "{who} {name}: {errors}");
        }
        assert_eq!(fs::metadata(&mailbox).unwrap().ino(), pinned, "{who}");
        assert_eq!(fs::read_dir(&mailbox).unwrap().count(), 0, "{who}");
    }
}

/// What the tool server of each agent of a native team does stays open beside those rules:
/// it sends a call and removes the answer it has read.
#[test]
fn native_team_agents_still_send_a_call_and_remove_the_answer_they_read() {
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    for (who, number) in [("teammate", 40_u32), ("lead", 50_u32)] {
        let id = format!("{number:032x}");
        let answer = mailbox.join(format!("{id}.reply"));
        fs::write(&answer, b"{}").unwrap();
        let call = mailbox.join(format!("{id}.call"));
        let (part, sent, reply) = (
            quoted(&mailbox.join(format!("{id}.part"))),
            quoted(&call),
            quoted(&answer),
        );
        let script = format!(
            "/usr/bin/touch {part}; echo part=$?\n\
             /bin/mv {part} {sent}; echo call=$?\n\
             /bin/cat {reply} > /dev/null; echo read=$?\n\
             /bin/rm {reply}; echo removed=$?\n"
        );
        let output = if who == "lead" {
            team.as_lead(&script)
        } else {
            team.teammate(&script)
        };
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let errors = String::from_utf8_lossy(&output.stderr);
        for name in ["part", "call", "read", "removed"] {
            assert_eq!(status_of(&text, name), "0", "{who} {name}: {errors}");
        }
        assert!(call.exists(), "{who}");
        assert!(!answer.exists(), "{who}");
    }
}

/// A native lead's teammates neither read nor change the lead's part of the relay mailbox,
/// where the engine keeps the lead's letters, reports, asks and answers: reading a letter,
/// listing the part, taking a letter by rename, planting an entry, and removing or moving
/// the part are all refused, and only the roster the engine keeps there is readable. The
/// same reads, plants and moves spelled in another letter case, and a hard link of a
/// letter out of the part, are refused too (relay RC-1). The lead reads and takes its own
/// letters (DEC-111 relay mailbox forgery, C1).
#[test]
fn native_teammates_neither_read_nor_change_the_leads_part_of_the_relay_mailbox() {
    let team = LiveTeam::start();
    let mailbox = team.case.tmp.join(crazyagent::team::tool::MAILBOX_FOLDER);
    let part = mailbox.join(crazyagent::coord::relay::LEAD_PART);
    fs::create_dir(&part).unwrap();
    fs::set_permissions(&part, fs::Permissions::from_mode(0o700)).unwrap();
    let letter = part.join(format!("{:032x}.inbox", 5));
    fs::write(&letter, b"letter for the lead").unwrap();
    fs::write(part.join("relay.json"), b"{}").unwrap();
    let (l, r, p) = (
        quoted(&letter),
        quoted(&part.join("relay.json")),
        quoted(&part),
    );
    let shown = quoted(&part.join(format!("{:032x}.shown", 5)));
    let away = quoted(&mailbox.join("away"));
    let upper = mailbox
        .join(crazyagent::coord::relay::LEAD_PART.to_uppercase())
        .join(letter.file_name().unwrap());
    let mixed = mailbox.join("Lead");
    let linked = mailbox.join("linked");
    let calls = team
        .case
        .tmp
        .join(crazyagent::team::tool::MAILBOX_FOLDER.to_uppercase());
    assert!(
        upper.is_file() && mixed.is_dir() && calls.is_dir(),
        "the volume finds the part and the mailbox in any letter case"
    );
    let (u, x, k, c) = (
        quoted(&upper),
        quoted(&mixed),
        quoted(&linked),
        quoted(&calls),
    );
    let gone = quoted(&team.case.tmp.join("calls-away"));
    let probed = team.teammate(&format!(
        "/bin/cat {l}; echo read=$?\n/bin/cat {r} >/dev/null; echo roster=$?\n/bin/ls {p}; echo list=$?\n/bin/mv {l} {shown}; echo take=$?\n/usr/bin/touch {p}/planted; echo plant=$?\n/bin/rm -r {p}; echo rm=$?\n/bin/mv {p} {away}; echo move=$?\n/bin/cat {u}; echo read_case=$?\n/usr/bin/touch {x}/planted; echo plant_case=$?\n/bin/ln {l} {k}; echo link=$?\n/bin/mv {c} {gone}; echo move_case=$?\n"
    ));
    let text = String::from_utf8_lossy(&probed.stdout).into_owned();
    let errors = String::from_utf8_lossy(&probed.stderr);
    assert!(
        !text.contains("letter for the lead"),
        "a teammate read the lead's letter: {text}"
    );
    for name in [
        "read",
        "list",
        "take",
        "plant",
        "rm",
        "move",
        "read_case",
        "plant_case",
        "link",
        "move_case",
    ] {
        assert_ne!(
            status_of(&text, name),
            "0",
            "the teammate's {name} in the lead's part succeeded: {errors}"
        );
    }
    assert_eq!(status_of(&text, "roster"), "0", "{errors}");
    assert!(
        letter.is_file()
            && !part.join("planted").exists()
            && fs::symlink_metadata(&linked).is_err()
            && mailbox.is_dir(),
        "the lead's part is as the engine left it"
    );

    let lead = team.as_lead(&format!(
        "/bin/cat {l} >/dev/null; echo read=$?\n/bin/mv {l} {shown}; echo take=$?\n"
    ));
    let text = String::from_utf8_lossy(&lead.stdout).into_owned();
    for name in ["read", "take"] {
        assert_eq!(
            status_of(&text, name),
            "0",
            "{}",
            String::from_utf8_lossy(&lead.stderr)
        );
    }
}

/// A confined agent can neither give its home's launch lock a second link, in its home or
/// anywhere else it writes, nor remove or replace it (RV-DS241-137 r3): a launch refuses a
/// lock with a second link, so one `ln` would refuse every later launch of that home.
#[test]
fn a_confined_agent_never_links_removes_or_replaces_its_homes_launch_lock() {
    use std::os::unix::fs::MetadataExt;
    let case = case();
    let lock = case.home.join("crazyagent.lock");
    write(&lock, "");
    let sandbox = ProjectSandbox::new(
        &crazyagent::sandbox::PROFILE_HOOKS,
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let profile = case.profiles.join("lock.sb");
    let held = quoted(&lock);
    let other = quoted(&case.home.join("other"));
    let in_home = quoted(&case.home.join("x"));
    let in_tmp = quoted(&case.tmp.join("x"));
    let denied = [
        ("link_home", format!("ln {held} {in_home}")),
        ("link_tmp", format!("ln {held} {in_tmp}")),
        ("remove", format!("rm -f {held}")),
        (
            "replace",
            format!("printf x > {other} && mv -f {other} {held}"),
        ),
    ];
    for (name, script) in denied {
        let output = run(&sandbox, &profile, &script);
        assert!(!output.status.success(), "{name} ran: {script}");
    }
    let metadata = fs::symlink_metadata(&lock).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.nlink(), 1);
    assert_eq!(metadata.len(), 0);
}
