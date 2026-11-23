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
