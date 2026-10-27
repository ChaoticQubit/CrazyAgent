use super::{
    Action, History, MENU_SHOWN_MAX, command, completions, menu, tag_parts, tag_query, tags,
};
use crate::project_setup::{ContextItem, ContextKind, ProjectContext};
use crate::protocol::ProjectId;
use crate::team::{GraphNode, NodeKind, TeamGraph};

fn node(id: &str, kind: NodeKind, label: &str) -> GraphNode {
    GraphNode {
        id: id.into(),
        kind,
        parent: None,
        label: label.into(),
        harness: "codex".into(),
        status: None,
        status_label: None,
        account: None,
        agents: None,
        terminal: None,
    }
}

fn team() -> TeamGraph {
    TeamGraph {
        project_id: ProjectId::new("notes-cli").unwrap(),
        nodes: vec![
            node("master", NodeKind::Master, "Master"),
            node("group-codex", NodeKind::HarnessGroup, "Codex (@group)"),
            node("agent-1", NodeKind::Agent, "Ada (@page)"),
            node("agent-2", NodeKind::Agent, "Grace"),
            node("agent-3", NodeKind::Agent, "Linus (@onboarding-fix)"),
        ],
        edges: vec![],
    }
}

fn context(paths: &[&str]) -> ProjectContext {
    ProjectContext {
        project_id: ProjectId::new("notes-cli").unwrap(),
        folder: "/private/tmp/ca-notes-cli".into(),
        items: paths
            .iter()
            .map(|path| ContextItem {
                path: (*path).into(),
                kind: ContextKind::Document,
                bytes: 10,
            })
            .collect(),
    }
}

#[test]
fn a_slash_at_line_start_opens_the_menu_filtered_by_the_typed_word() {
    let names =
        |head: &str| menu(head).map(|found| found.iter().map(|c| c.name).collect::<Vec<_>>());
    assert_eq!(
        names("/"),
        Some(vec![
            "help", "clear", "agents", "changes", "usage", "process", "stop"
        ])
    );
    assert_eq!(names("/c"), Some(vec!["clear", "changes"]));
    assert_eq!(names("/st"), Some(vec!["stop"]));
    assert_eq!(names("/zzz"), Some(vec![]));
    assert_eq!(names("first line\n/he"), Some(vec!["help"]));
    assert_eq!(names("say /he"), None, "a slash inside a line is text");
    assert_eq!(names("/stop now"), None, "the menu closes after a space");
    assert_eq!(names("hello"), None);
}

#[test]
fn a_prompt_starting_with_a_slash_is_a_command_and_an_unknown_one_is_named() {
    assert_eq!(command("/agents"), Some(Ok(Action::Agents)));
    assert_eq!(command("  /stop \n"), Some(Ok(Action::Stop)));
    assert_eq!(command("/clear"), Some(Ok(Action::Clear)));
    assert_eq!(command("/deploy"), Some(Err("/deploy".into())));
    assert_eq!(command("/stop now"), Some(Err("/stop now".into())));
    assert_eq!(command("Plan the page"), None);
}

#[test]
fn an_at_word_at_the_cursor_is_completed_from_master_handles_and_project_files() {
    assert_eq!(tag_query("@"), Some(""));
    assert_eq!(tag_query("ask @pa"), Some("pa"));
    assert_eq!(tag_query("line\n@ma"), Some("ma"));
    assert_eq!(
        tag_query("mail me@home"),
        None,
        "an @ inside a word is text"
    );
    assert_eq!(
        tag_query("ask @page now"),
        None,
        "the tag ended at the space"
    );

    let graph = team();
    let all = tags(
        Some(&graph),
        Some(&context(&["docs/brief.md", "my notes.md", "README.md"])),
    );
    assert_eq!(
        all,
        [
            "@master",
            "@page",
            "@onboarding-fix",
            "@docs/brief.md",
            "@README.md"
        ],
        "only sub-agents with a handle, never the group; paths with spaces are left out"
    );
    assert_eq!(tags(None, None), ["@master"]);
    assert_eq!(
        completions(&all, ""),
        all[..5].iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(completions(&all, "ON"), ["@onboarding-fix"]);
    assert_eq!(completions(&all, "re"), ["@README.md"]);
    assert_eq!(completions(&all, "x"), Vec::<&str>::new());

    let many: Vec<String> = (0..20).map(|n| format!("@file-{n}")).collect();
    assert_eq!(completions(&many, "file").len(), MENU_SHOWN_MAX);
}

#[test]
fn tags_in_a_line_are_split_out_for_highlighting() {
    assert_eq!(
        tag_parts("ask @page to read @docs/brief.md"),
        [
            ("ask ", false),
            ("@page", true),
            (" to read ", false),
            ("@docs/brief.md", true)
        ]
    );
    assert_eq!(tag_parts("@master hi"), [("@master", true), (" hi", false)]);
    assert_eq!(
        tag_parts("mail me@home or @"),
        [("mail me@home or @", false)]
    );
    assert_eq!(tag_parts(""), Vec::<(&str, bool)>::new());
}

#[test]
fn up_and_down_recall_sent_messages_only_from_an_empty_prompt() {
    let mut history = History::default();
    assert_eq!(history.step(true, ""), None, "nothing sent yet");
    history.push("first");
    history.push("second");
    history.push("second");
    assert_eq!(history.step(true, "draft"), None, "a typed prompt keeps Up");
    assert_eq!(history.step(true, "").as_deref(), Some("second"));
    assert_eq!(history.step(true, "second").as_deref(), Some("first"));
    assert_eq!(
        history.step(true, "first").as_deref(),
        Some("first"),
        "the oldest stays"
    );
    assert_eq!(history.step(false, "first").as_deref(), Some("second"));
    assert_eq!(
        history.step(false, "second").as_deref(),
        Some(""),
        "past the newest is empty again"
    );
    assert_eq!(history.step(false, ""), None);
    assert_eq!(history.step(true, "").as_deref(), Some("second"));
    assert_eq!(
        history.step(true, "second, edited"),
        None,
        "an edited recall stops recalling"
    );
    for n in 0..60 {
        history.push(&format!("message {n}"));
    }
    let mut oldest = history.step(true, "").unwrap();
    while let Some(older) = history.step(true, &oldest).filter(|older| *older != oldest) {
        oldest = older;
    }
    assert_eq!(oldest, "message 10", "only the newest 50 are kept");
}
