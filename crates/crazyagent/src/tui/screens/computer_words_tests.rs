use super::THIS_COMPUTER;
use super::harness_setup::{INSTALLED, READING};
use super::home::HARNESSES_HERE;
use super::intake::AddWarning;
use super::settings::profile::{ARROWS_LOST, COLORS_LOST, ENTER_LOST};
use super::settings_accounts::plain;
use super::settings_add_account::{url_problem, words::CHECK_LINE};
use super::settings_harnesses::{SUBTITLE, path_copy, update_copy};
use super::settings_plugins::plugin_plain;
use crate::accounts::Provider;
use crate::harnesses::finding;

#[test]
fn harness_sentences_name_the_computer_the_build_runs_on() {
    let here = THIS_COMPUTER;
    for (shown, expected) in [
        (
            path_copy(finding::PATH_NOT_EXECUTABLE),
            format!("That file isn't a program {here} can run."),
        ),
        (
            path_copy(finding::PATH_CHANGEABLE),
            format!(
                "Another user on {here} can change that file or a folder above it, so CrazyAgent won't run it."
            ),
        ),
        (
            path_copy("native_auth_host_executable_unsafe"),
            format!(
                "Another user on {here} can change that file, or it isn't a plain program file, so CrazyAgent won't run it."
            ),
        ),
        (
            update_copy("harness_update_tool_missing"),
            format!("The tool that updates it is no longer on {here}."),
        ),
        (
            update_copy("harness_update_not_installed"),
            format!("It isn't installed on {here}."),
        ),
        (
            update_copy("harness_install_already"),
            format!("It's already installed on {here}."),
        ),
        (
            update_copy("harness_install_needs_node"),
            format!("CrazyAgent installs this one with npm, and Node.js isn't on {here} yet."),
        ),
        (
            SUBTITLE,
            if cfg!(windows) {
                "Agents run inside these tools. CrazyAgent finds them on this computer. It can install one with npm and update one with that harness's own updater, each only when you choose it.".to_owned()
            } else {
                format!(
                    "Agents run inside these tools. CrazyAgent finds them on {here}. It can install one with Homebrew or npm and update one with that harness's own updater, each only when you choose it."
                )
            },
        ),
        (INSTALLED, format!("Already on {here}.")),
        (READING, format!("Reading what's on {here}.")),
        (HARNESSES_HERE, format!("on {here}.")),
    ] {
        assert_eq!(shown, expected);
    }
}

#[test]
fn account_sentences_name_the_computer_the_build_runs_on() {
    let here = THIS_COMPUTER;
    assert_eq!(
        CHECK_LINE,
        format!(
            "Checking the sign-in this account already has in CrazyAgent. Nothing else on {here} is read."
        )
    );
    let https = format!("Use an https address. Plain http works only on {here}.");
    assert_eq!(plain("account_url_invalid"), https);
    assert_eq!(
        url_problem(Provider::OpenaiCompatible, "http://example.com:8080/v1"),
        Some(https.as_str())
    );
    let http = format!("Use an http address on {here}, such as http://127.0.0.1:11434/v1.");
    assert_eq!(
        url_problem(Provider::Local, "http://192.168.1.5:11434/v1"),
        Some(http.as_str())
    );
}

#[test]
fn plugin_and_profile_sentences_name_the_computer_the_build_runs_on() {
    let here = THIS_COMPUTER;
    let https = format!(
        "Use an https address. Plain http works only for {here}, and the address can't hold a password."
    );
    assert_eq!(
        crate::plugins::plain_problem("server_url_invalid"),
        Some(https.as_str())
    );
    assert_eq!(
        plugin_plain("server_url_invalid"),
        format!("Use an https address. Plain http works only for {here}.")
    );
    assert_eq!(
        plugin_plain("server_command_not_found"),
        format!("That command is not on {here} or cannot run.")
    );
    for (shown, choice) in [
        (COLORS_LOST, "color"),
        (ARROWS_LOST, "Shift-arrows"),
        (ENTER_LOST, "Shift-Enter"),
    ] {
        assert_eq!(
            shown,
            format!("Profile saved, but the {choice} choice could not be kept on {here}.")
        );
    }
}

#[test]
fn intake_warnings_name_the_computer_the_build_runs_on() {
    let here = THIS_COMPUTER;
    for warning in [AddWarning::AcrossDisks, AddWarning::CopyAll] {
        let text = warning.text();
        assert!(
            text.contains(&format!(" If {here} sleeps, loses power ")),
            "{warning:?}: {text}"
        );
        assert!(!text.contains("the Mac"), "{warning:?}: {text}");
        if !cfg!(target_os = "macos") {
            assert!(!text.contains("Mac"), "{warning:?}: {text}");
        }
    }
}
