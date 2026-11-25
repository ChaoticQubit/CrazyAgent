use super::*;

/// DEC-180 Task 16: a manifest harness's pane runs its program with the manifest's
/// arguments, a resumed run with its resume list, and follows the record its settings name.
#[test]
fn a_manifest_command_runs_its_arguments_and_follows_the_record_its_settings_name() {
    let case = Case::new();
    let settings = standin_settings();
    let request = case.request("standin-jsonl-append", keyless(&case), true);
    let fresh = command_with(&request, None, &run(None), Some(&settings));
    assert!(fresh.is_ok(), "{fresh:?}");
    let fresh = fresh.unwrap();
    let home = case.home().to_str().unwrap().to_owned();
    let arguments = ["--home", home.as_str(), "--session", SESSION].map(str::to_owned);
    assert!(fresh.argv.ends_with(&arguments), "{:?}", fresh.argv);
    assert_eq!(fresh.argv[0], SANDBOX_EXEC);
    let records = settings.records(&case.home(), SESSION).unwrap();
    assert_eq!(fresh.transcript, Transcript::Records(records));
    let again = Run {
        resume: true,
        ..run(None)
    };
    let resumed = command_with(&request, None, &again, Some(&settings)).unwrap();
    let arguments = ["--home", home.as_str(), "--resume", SESSION].map(str::to_owned);
    assert!(resumed.argv.ends_with(&arguments), "{:?}", resumed.argv);
}

/// DEC-180 Task 16 r2 (T16-2): a manifest harness's session id fills its arguments only
/// as a plain word that starts with a letter or digit, so no id adds an option, fresh or
/// resumed.
#[test]
fn a_manifest_command_refuses_a_session_id_that_is_no_plain_word() {
    let case = Case::new();
    let settings = standin_settings();
    let request = case.request("standin-jsonl-append", keyless(&case), true);
    let long = "a".repeat(65);
    for session_id in ["--exec", "-x", "_x", "", "a b", "a/b", long.as_str()] {
        for resume in [false, true] {
            let given = Run {
                session_id,
                key_file: None,
                resume,
            };
            let refused = command_with(&request, None, &given, Some(&settings));
            let invalid = Some(failure("launch_session_id_invalid"));
            assert_eq!(refused.err(), invalid, "{session_id:?} {resume}");
        }
    }
    let plain = Run {
        session_id: "standin-1",
        ..run(None)
    };
    assert!(command_with(&request, None, &plain, Some(&settings)).is_ok());
}
