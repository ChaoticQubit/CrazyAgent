use super::*;

const NONE: Tools = Tools {
    edits: false,
    reads: false,
    bash: false,
    tasks: false,
    search: false,
    mcp: false,
    skill: false,
    plan_mode: false,
    agent_spawn: false,
};
const EDITS: Tools = Tools {
    edits: true,
    ..NONE
};
const BASH: Tools = Tools { bash: true, ..NONE };
const READS: Tools = Tools {
    reads: true,
    ..NONE
};

fn of(message: &str, tools: Tools) -> Activity {
    classify(&words_of(message), &tools)
}

#[test]
fn each_category_is_reached() {
    for (message, tools, expected) in [
        ("do the thing", EDITS, Activity::Coding),
        ("fix the crash", EDITS, Activity::Debugging),
        ("rename the module", EDITS, Activity::Refactoring),
        ("add a page", EDITS, Activity::Feature),
        ("run it with pytest", BASH, Activity::Testing),
        ("git push it", BASH, Activity::Git),
        ("docker up", BASH, Activity::BuildDeploy),
        ("cargo add serde", BASH, Activity::BuildDeploy),
        ("where is it", READS, Activity::Exploration),
        ("why the error", READS, Activity::Debugging),
        (
            "do the thing",
            Tools {
                plan_mode: true,
                ..NONE
            },
            Activity::Planning,
        ),
        (
            "do the thing",
            Tools {
                tasks: true,
                ..NONE
            },
            Activity::Planning,
        ),
        (
            "do the thing",
            Tools {
                agent_spawn: true,
                ..NONE
            },
            Activity::Delegation,
        ),
        (
            "do the thing",
            Tools { mcp: true, ..NONE },
            Activity::Exploration,
        ),
        (
            "do the thing",
            Tools {
                search: true,
                ..NONE
            },
            Activity::Exploration,
        ),
        (
            "do the thing",
            Tools {
                skill: true,
                ..NONE
            },
            Activity::General,
        ),
        ("hello there", NONE, Activity::Conversation),
        ("what if we tried", NONE, Activity::Brainstorming),
        ("explain the flow", NONE, Activity::Exploration),
        ("open main.rs", NONE, Activity::Coding),
        ("curl it", NONE, Activity::Coding),
        ("see https://a.example", NONE, Activity::Exploration),
    ] {
        assert_eq!(of(message, tools), expected, "{message}");
    }
}

#[test]
fn the_earliest_word_wins() {
    assert_eq!(of("add error handling", EDITS), Activity::Feature);
    assert_eq!(
        of("the error came after we add it", EDITS),
        Activity::Debugging
    );
    assert_eq!(of("add error handling", NONE), Activity::Feature);
}

#[test]
fn words_are_whole_and_case_does_not_matter() {
    assert_eq!(of("contest", BASH), Activity::Coding);
    assert_eq!(of("FIX THE BUG", EDITS), Activity::Debugging);
    assert_eq!(of("run   it\twith\n pytest", BASH), Activity::Testing);
}

#[test]
fn a_bash_turn_with_reads_and_no_words_is_exploration_and_alone_is_coding() {
    assert_eq!(
        of(
            "do the thing",
            Tools {
                bash: true,
                reads: true,
                ..NONE
            }
        ),
        Activity::Exploration
    );
    assert_eq!(of("do the thing", BASH), Activity::Coding);
}

#[test]
fn a_very_long_message_is_cut_at_a_character_boundary() {
    let text = format!("{} fix", "é".repeat(9000));
    assert_eq!(of(&text, NONE), Activity::Conversation);
}

#[test]
fn every_activity_has_a_name_the_table_accepts() {
    for activity in ACTIVITIES {
        assert!(CATEGORY_CHECK.contains(&format!("'{}'", activity.name())));
    }
}
