use super::*;
use crate::tui::frame::Tone;

fn event(result: Result<(), &str>) -> RevealEvent {
    RevealEvent {
        query: RevealQuery {
            project: ProjectId::new("alpha").unwrap(),
            target: RevealTarget::Project {
                path: "context/brief.md".into(),
            },
        },
        result: result.map_err(str::to_owned),
    }
}

#[test]
fn open_in_ide_without_a_running_desktop_says_so_in_one_plain_line() {
    let said = event(Err("ide_not_running")).message();
    assert_eq!(said.tone, Tone::Error);
    assert_eq!(said.lead, IDE_NOT_RUNNING);
    assert!(said.rest.contains("IDE tab"), "{}", said.rest);

    let shown = event(Ok(())).message();
    assert_eq!(shown.tone, Tone::Success);
    assert_eq!(shown.lead, "Opened in the desktop IDE.");

    for code in [
        "ide_reveal_unanswered",
        "ide_reveal_not_listed",
        "ide_reveal_missing",
        "ide_reveal_outside",
        "trusted_connection_unavailable",
        "something_new",
    ] {
        let said = event(Err(code)).message();
        assert_eq!(said.tone, Tone::Error, "{code}");
        assert!(
            !said.lead.contains('_'),
            "{code} shows a plain line, not a code"
        );
    }
    assert_eq!(
        RevealEvent::unavailable(event(Ok(())).query).message().lead,
        "The engine connection is not ready yet."
    );
}
