use super::*;
use crate::plugins::TOOL_NAME_BYTES_MAX;
use crate::protocol::ProjectId;

fn refused(action: Action) -> String {
    action.validate().unwrap_err().0
}

#[test]
fn tool_actions_check_the_server_id_and_tool_name_before_the_engine() {
    let project = Some(ProjectId::new("app").unwrap());
    let set = |server: Option<&str>, tool: &str| Action::SetTool {
        project: project.clone(),
        server: server.map(str::to_owned),
        tool: tool.into(),
        switch: Switch::Off,
    };
    for action in [
        Action::Tools {
            project: project.clone(),
            server: Some("Bad_Id".into()),
        },
        Action::RestoreTools {
            project: None,
            server: Some("Bad_Id".into()),
        },
        set(Some("Bad_Id"), "echo"),
        set(Some(""), "echo"),
        set(Some(&"a".repeat(65)), "echo"),
    ] {
        assert_eq!(refused(action), "server_not_found");
    }
    for tool in [
        String::new(),
        "t".repeat(TOOL_NAME_BYTES_MAX + 1),
        "a\nb".into(),
    ] {
        assert_eq!(refused(set(None, &tool)), "tool_unknown", "{tool:?}");
    }
    set(None, &"t".repeat(TOOL_NAME_BYTES_MAX))
        .validate()
        .unwrap();
    set(Some("server-1"), "echo").validate().unwrap();
    Action::Tools {
        project: None,
        server: None,
    }
    .validate()
    .unwrap();
}

#[test]
fn reading_tools_is_not_a_change_and_setting_or_restoring_them_is() {
    let project = Some(ProjectId::new("app").unwrap());
    assert!(
        !Action::Tools {
            project: project.clone(),
            server: None,
        }
        .changes()
    );
    assert!(
        Action::SetTool {
            project: project.clone(),
            server: None,
            tool: "start_work".into(),
            switch: Switch::Off,
        }
        .changes()
    );
    assert!(
        Action::RestoreTools {
            project,
            server: None,
        }
        .changes()
    );
}
