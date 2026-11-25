use super::*;

/// V243-SETUP-TUI-FIX-PENDING (#381 F3): on first run, a setup run that ends while this
/// screen's own permission fix still runs, or while its tool is checked again, keeps Setup
/// open; once the fix ends the screen says how the tool reads and only then leaves for the
/// Harnesses step, as `quiet` requires for the 'c' key.
#[test]
fn first_run_leaves_only_after_its_own_fix_ends_when_the_run_ends_first() {
    let mut shell = first_run(120, 36);
    shell.arrive(event(AppSetupEvent::Status(Ok(refused_status(
        crate::setup::RefusedTool::FixButton,
    )))));
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, AppSetupJob::Run { .. }))
    );
    shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
    jobs(&mut shell);
    shell.key(Key::Char('p'));
    shell.key(Key::Char('y'));
    fix_sent(&mut shell);
    shell.arrive(event(AppSetupEvent::FixSet(Ok(()))));
    jobs(&mut shell);
    let mut fixing = refused_status(crate::setup::RefusedTool::FixButton);
    fixing.dependencies[2].busy = true;
    shell.arrive(event(AppSetupEvent::Status(Ok(fixing))));
    assert_eq!(
        shell.place(),
        Some(Place::Setup { first_run: true }),
        "the run ended while the fix runs: {}",
        lead(&shell)
    );
    let mut checking = installed_status();
    checking.dependencies[2].state = DependencyState::Checking;
    shell.arrive(event(AppSetupEvent::Status(Ok(checking))));
    assert_eq!(
        shell.place(),
        Some(Place::Setup { first_run: true }),
        "the fixed tool is checked again: {}",
        lead(&shell)
    );
    shell.arrive(event(AppSetupEvent::Status(Ok(installed_status()))));
    assert_eq!(
        lead(&shell),
        "Permissions fixed. Graphify command can run now."
    );
    assert_eq!(shell.place(), Some(Place::HarnessSetup));
}

/// V243-SETUP-TUI-HELD-OUTCOME (#412 rv-code4 F1): on first run, a run that ended while this
/// screen's own fix ran is held; when the fix then ends with its tool Missing or Failed, the
/// fix's outcome stays the lead, never replaced by the earlier run's end, and Setup stays.
#[test]
fn first_run_keeps_its_own_fix_outcome_when_the_held_run_ends_with_a_problem() {
    for ended in [
        DependencyState::Missing {
            line: String::new(),
        },
        DependencyState::Failed {
            reason: "no network".into(),
        },
    ] {
        let mut shell = first_run(120, 36);
        shell.arrive(event(AppSetupEvent::Status(Ok(refused_status(
            crate::setup::RefusedTool::FixButton,
        )))));
        shell.key(Key::Enter);
        jobs(&mut shell);
        shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
        jobs(&mut shell);
        shell.key(Key::Char('p'));
        shell.key(Key::Char('y'));
        fix_sent(&mut shell);
        shell.arrive(event(AppSetupEvent::FixSet(Ok(()))));
        jobs(&mut shell);
        let mut fixing = refused_status(crate::setup::RefusedTool::FixButton);
        fixing.dependencies[2].busy = true;
        shell.arrive(event(AppSetupEvent::Status(Ok(fixing))));
        assert_eq!(shell.place(), Some(Place::Setup { first_run: true }));
        let mut done = installed_status();
        done.dependencies[2].state = ended.clone();
        shell.arrive(event(AppSetupEvent::Status(Ok(done))));
        assert_eq!(
            lead(&shell),
            "Permissions changed, but Graphify command is still not used.",
            "{ended:?}"
        );
        assert_eq!(shell.place(), Some(Place::Setup { first_run: true }));
    }
}

/// V243-SETUP-TUI-HELD-OUTCOME (#521 rv-code F1): when one read ends both the run and this
/// screen's own fix, the fix's outcome is the lead, never replaced by the run's end, in first
/// run and in Settings, whether the tool can run now or is still not used.
#[test]
fn a_run_and_its_own_fix_ending_in_one_read_lead_with_the_fix_outcome() {
    for first in [true, false] {
        for (ended, line) in [
            (
                DependencyState::Missing {
                    line: String::new(),
                },
                "Permissions changed, but Graphify command is still not used.",
            ),
            (
                installed_status().dependencies[2].state.clone(),
                "Permissions fixed. Graphify command can run now.",
            ),
        ] {
            let mut shell = if first {
                first_run(120, 36)
            } else {
                settings(120, 36)
            };
            shell.arrive(event(AppSetupEvent::Status(Ok(refused_status(
                crate::setup::RefusedTool::FixButton,
            )))));
            if !first {
                for _ in 0..5 {
                    shell.key(Key::Down);
                }
                shell.key(Key::Char(' '));
            }
            shell.key(Key::Enter);
            assert!(
                jobs(&mut shell)
                    .iter()
                    .any(|job| matches!(job, AppSetupJob::Run { .. })),
                "first run {first}"
            );
            shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
            jobs(&mut shell);
            shell.key(Key::Char('p'));
            shell.key(Key::Char('y'));
            fix_sent(&mut shell);
            shell.arrive(event(AppSetupEvent::FixSet(Ok(()))));
            jobs(&mut shell);
            let mut done = installed_status();
            done.dependencies[2].state = ended.clone();
            shell.arrive(event(AppSetupEvent::Status(Ok(done))));
            assert_eq!(lead(&shell), line, "first run {first}");
        }
    }
}

/// Runs setup with this screen's own Graphify permission fix and ends both in one status
/// read, the fixed tool reading `ended`. Inputs: first run or Settings, whether serena (never
/// installed by the run) is ticked first, and the tool's state at the end. Output: the shell
/// after that read.
fn run_and_fix_in_one_read(first: bool, serena: bool, ended: DependencyState) -> Shell {
    let mut shell = if first {
        first_run(80, 24)
    } else {
        settings(80, 24)
    };
    shell.arrive(event(AppSetupEvent::Status(Ok(refused_status(
        crate::setup::RefusedTool::FixButton,
    )))));
    if serena {
        for _ in 0..5 {
            shell.key(Key::Down);
        }
        shell.key(Key::Char(' '));
    }
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, AppSetupJob::Run { .. })),
        "first run {first}, serena {serena}"
    );
    shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
    jobs(&mut shell);
    shell.key(Key::Char('p'));
    shell.key(Key::Char('y'));
    fix_sent(&mut shell);
    shell.arrive(event(AppSetupEvent::FixSet(Ok(()))));
    jobs(&mut shell);
    let mut done = installed_status();
    done.dependencies[2].state = ended;
    shell.arrive(event(AppSetupEvent::Status(Ok(done))));
    shell
}

/// V243-SETUP-TUI-SAME-READ (#523 rv-code r2 F1): when one read ends this screen's own fix and
/// a run that left items uninstalled, the fix's line leads and the run's count of failed items
/// follows it on the one message row, whole at 80 columns, in a tone that does not clear: a
/// warning whether the fix worked or the tool is still not used, an error when the fix failed.
#[test]
fn a_fix_ending_with_a_run_that_left_items_keeps_the_run_failure_under_its_line() {
    use crate::tui::frame::Tone;
    let missing = DependencyState::Missing {
        line: String::new(),
    };
    let fixed = installed_status().dependencies[2].state.clone();
    let mut refused = refused_status(crate::setup::RefusedTool::FixButton).dependencies[2]
        .state
        .clone();
    let DependencyState::Refused { fix_failed, .. } = &mut refused else {
        panic!("the refused fixture reads Refused");
    };
    *fix_failed = Some("The fix could not change /h/py.".into());
    let changed = "Permissions changed, but Graphify command is still not used.";
    let works = "Permissions fixed. Graphify command can run now.";
    let broke = "The fix could not change /h/py.";
    for (first, ended, line, rest, tone) in [
        (
            true,
            missing.clone(),
            changed,
            "1 item failed.",
            Tone::Warning,
        ),
        (false, missing, changed, "2 items failed.", Tone::Warning),
        (false, fixed, works, "1 item failed.", Tone::Warning),
        (false, refused, broke, "1 item failed.", Tone::Error),
    ] {
        let mut shell = run_and_fix_in_one_read(first, !first, ended);
        let said = shell.ctx.message().expect("a message shows").clone();
        assert_eq!(
            (said.lead.as_str(), said.rest.as_str(), said.tone),
            (line, rest, tone),
            "first run {first}"
        );
        let rows = render(&mut shell, 80, 24);
        assert!(
            rows.iter()
                .any(|row| row.contains(&format!("{line} {rest}"))),
            "first run {first}:\n{}",
            rows.join("\n")
        );
    }
}

/// V243-SETUP-TUI-SAME-READ (#523 rv-code r2 F2): in Settings, when one read ends this screen's
/// own working fix and a run that installed everything, the fix's line is the last word and
/// "Setup finished." never replaces it.
#[test]
fn in_settings_a_fix_ending_with_a_run_that_installed_everything_keeps_its_line() {
    let fixed = installed_status().dependencies[2].state.clone();
    let shell = run_and_fix_in_one_read(false, false, fixed);
    assert_eq!(
        lead(&shell),
        "Permissions fixed. Graphify command can run now."
    );
}

/// V243-SETUP-TUI-FIX-PENDING (#381 F3 r2): in Settings a run that ends while this screen's
/// own permission fix runs says so at once, and the fix's outcome, said when it ends, is the
/// last word, never overwritten by the run's end.
#[test]
fn settings_says_the_run_end_then_its_own_fix_outcome_last() {
    let mut shell = settings(120, 36);
    shell.arrive(event(AppSetupEvent::Status(Ok(refused_status(
        crate::setup::RefusedTool::FixButton,
    )))));
    for _ in 0..5 {
        shell.key(Key::Down);
    }
    shell.key(Key::Char(' '));
    shell.key(Key::Enter);
    assert!(
        jobs(&mut shell)
            .iter()
            .any(|job| matches!(job, AppSetupJob::Run { .. }))
    );
    shell.arrive(event(AppSetupEvent::Ran(Ok(()))));
    jobs(&mut shell);
    shell.key(Key::Char('p'));
    shell.key(Key::Char('y'));
    fix_sent(&mut shell);
    shell.arrive(event(AppSetupEvent::FixSet(Ok(()))));
    jobs(&mut shell);
    let mut fixing = refused_status(crate::setup::RefusedTool::FixButton);
    fixing.dependencies[2].busy = true;
    shell.arrive(event(AppSetupEvent::Status(Ok(fixing))));
    assert!(
        lead(&shell).starts_with("Setup finished"),
        "{}",
        lead(&shell)
    );
    shell.arrive(event(AppSetupEvent::Status(Ok(installed_status()))));
    assert_eq!(
        lead(&shell),
        "Permissions fixed. Graphify command can run now."
    );
}
