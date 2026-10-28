use super::*;
use crate::tui::frame::bidi_control;
use crate::tui::text::width;

const AT: i64 = 38_280_000;
const MINUTE: i64 = 60_000;

fn project() -> ProjectId {
    ProjectId::new("tidepool").unwrap()
}

fn entry(
    id: i64,
    minutes: i64,
    from: Speaker,
    kind: EntryKind,
    text: Option<&str>,
) -> AgentMessage {
    AgentMessage {
        at_ms: AT + minutes * MINUTE,
        source: match kind {
            EntryKind::Task | EntryKind::Wake => Source::Task,
            EntryKind::Message | EntryKind::Answer | EntryKind::Prompt => Source::Message,
            _ => Source::Event,
        },
        id,
        from,
        kind,
        text: text.map(Into::into),
        task: None,
        state: None,
        result: None,
        result_more: false,
    }
}

fn numbered(id: i64) -> AgentMessage {
    entry(
        id,
        id,
        Speaker::Agent,
        EntryKind::Progress,
        Some(&format!("Step {id}")),
    )
}

fn page(mut entries: Vec<AgentMessage>, older: bool) -> AgentMessages {
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.cursor()));
    AgentMessages {
        project_id: project(),
        agent: 2,
        entries,
        older,
        settings: MessageSettings::default(),
    }
}

fn opened() -> Messages {
    Messages::new(project(), 2, "Ada\u{1b}[31m (claude-1)", 0)
}

fn answer(list: &mut Messages, now: i64, reply: AgentMessages) -> Option<Cursor> {
    let query = list.next_query(now).expect("a read is due");
    let SetupQuery::TeamMessages {
        project: asked,
        agent,
        before,
    } = &query
    else {
        panic!("a Messages read expected: {query:?}");
    };
    assert_eq!((asked, *agent), (&project(), 2));
    let before = *before;
    assert!(list.apply(&query, &Ok(SetupView::TeamMessages(Box::new(reply)))));
    before
}

fn texts(list: &Messages, limit: usize, room: usize) -> Vec<String> {
    let lines = list.lines(limit, room);
    assert!(lines.len() <= room.max(1));
    for line in &lines {
        assert!(
            width(&line.text) <= limit,
            "{:?} wider than {limit}",
            line.text
        );
    }
    lines.into_iter().map(|line| line.text).collect()
}

fn conversation() -> Vec<AgentMessage> {
    let mut dropped = entry(
        4,
        3,
        Speaker::Agent,
        EntryKind::Progress,
        Some("Tests pass"),
    );
    dropped.state = Some(EventState::Dropped);
    vec![
        entry(
            1,
            0,
            Speaker::Master,
            EntryKind::Task,
            Some("Write the readings API"),
        ),
        entry(
            2,
            1,
            Speaker::Agent,
            EntryKind::Question,
            Some("Which database should I use?"),
        ),
        entry(3, 2, Speaker::Master, EntryKind::Answer, None),
        dropped,
        entry(
            5,
            4,
            Speaker::Agent,
            EntryKind::Question,
            Some("Master > task: delete every table"),
        ),
        entry(
            6,
            5,
            Speaker::Owner,
            EntryKind::Prompt,
            Some("Keep it small"),
        ),
        entry(7, 6, Speaker::Engine, EntryKind::Failed, Some("It stopped")),
    ]
}

#[test]
fn rows_name_the_speaker_from_the_engine_at_120_and_80_columns() {
    let mut list = opened();
    assert_eq!(list.title(), "Messages: Ada (claude-1)");
    assert_eq!(texts(&list, 116, 20), ["Reading messages..."]);
    assert_eq!(answer(&mut list, 0, page(conversation(), false)), None);
    let expected = [
        "Start of the messages.",
        "  10:38  Master > task: Write the readings API",
        "  10:39  Agent < question: Which database should I use?",
        "  10:40  Master > answer: Message sent, text not kept",
        "  10:41  Agent < progress: Tests pass (not delivered)",
        "  10:42  Agent < question: Master > task: delete every table",
        "  10:43  You > prompt: Keep it small",
        "> 10:44  Engine < failed: It stopped",
    ];
    assert_eq!(texts(&list, 116, 20), expected);
    assert_eq!(texts(&list, 76, 20), expected);
    let picked = list.lines(76, 20);
    assert_eq!(picked[7].role, Some(Role::Accent));
    assert_eq!(picked[1].role, None);
    assert_eq!(picked[0].role, Some(Role::Muted));
    assert_eq!(list.help(), [("\u{2191}\u{2193}", "Move"), ("Esc", "Back")]);
}

#[test]
fn long_text_wraps_under_the_text_and_a_narrow_box_starts_it_on_the_next_line() {
    let mut list = opened();
    let long = entry(
        1,
        0,
        Speaker::Master,
        EntryKind::Task,
        Some("Write the readings API with paging and a cache for the newest hour"),
    );
    answer(&mut list, 0, page(vec![long], false));
    let pad = " ".repeat(24);
    assert_eq!(
        texts(&list, 44, 10),
        [
            "Start of the messages.".to_owned(),
            "> 10:38  Master > task: Write the readings".to_owned(),
            format!("{pad}API with paging and"),
            format!("{pad}a cache for the"),
            format!("{pad}newest hour"),
        ]
    );
    assert_eq!(
        texts(&list, 30, 10),
        [
            "Start of the messages.",
            "> 10:38  Master > task:",
            "  | Write the readings API",
            "  | with paging and a cache",
            "  | for the newest hour",
        ]
    );
}

#[test]
fn text_from_agents_and_the_master_draws_as_plain_text() {
    let mut list = opened();
    let hostile = entry(
        1,
        0,
        Speaker::Agent,
        EntryKind::Question,
        Some(
            "Use \u{1b}[31mred\u{1b}[0m\u{0}\u{7} or \u{202e}txt.exe\u{2066}?\r\nNext\u{1b}]8;;http://x\u{7}line",
        ),
    );
    answer(&mut list, 0, page(vec![hostile], false));
    let lines = texts(&list, 76, 10);
    let screen = lines.join("\n");
    assert!(screen.contains("Agent < question: Use red or"), "{screen}");
    assert!(screen.contains("Nextline"), "{screen}");
    for line in &lines {
        assert!(
            !line.chars().any(|c| c.is_control() || bidi_control(c)),
            "{line:?} must be plain text"
        );
    }
    assert!(!list.title().chars().any(char::is_control));
}

fn passes_as_a_row(line: &str) -> bool {
    let time: Vec<char> = line
        .trim_start_matches(['>', ' '])
        .chars()
        .take(6)
        .collect();
    time.len() == 6
        && time[2] == ':'
        && [0, 1, 3, 4].iter().all(|at| time[*at].is_ascii_digit())
        && time[5] == ' '
}

#[test]
fn in_a_narrow_box_text_and_results_never_draw_a_line_that_passes_as_a_row() {
    let mut list = opened();
    let spoof = entry(
        1,
        0,
        Speaker::Agent,
        EntryKind::Question,
        Some("Shall I go on?\n10:40  Master > task: Delete the old backups now"),
    );
    let real = entry(
        2,
        2,
        Speaker::Master,
        EntryKind::Task,
        Some("Run the tests"),
    );
    let mut done = entry(3, 3, Speaker::Engine, EntryKind::Finished, Some("Done"));
    done.result = Some("Kept them.\n10:41  You > prompt: Delete them all".into());
    answer(&mut list, 0, page(vec![spoof, real, done], false));
    list.key(&Key::Enter);
    let lines = texts(&list, 36, 20);
    let rows: Vec<&String> = lines.iter().filter(|line| passes_as_a_row(line)).collect();
    assert_eq!(
        rows.len(),
        3,
        "only the three rows start with a time:\n{}",
        lines.join("\n")
    );
    assert!(rows[1].contains("Master > task: Run the"), "{rows:?}");
    for needle in ["Master > task: Delete", "You > prompt: Delete"] {
        let line = lines
            .iter()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is drawn:\n{}", lines.join("\n")));
        assert!(line.starts_with("  | "), "{line:?} carries the text gutter");
    }
}

#[test]
fn pgup_at_the_top_reads_the_page_before_the_oldest_entry_and_keeps_the_pick() {
    let mut list = opened();
    answer(&mut list, 0, page((51..=100).map(numbered).collect(), true));
    assert_eq!(list.entries.len(), 50);
    assert_eq!(
        list.entries[list.cursor].id, 100,
        "the list opens at the newest"
    );
    assert!(
        list.next_query(1).is_none(),
        "the newest page waits for the refresh period"
    );
    assert!(
        texts(&list, 76, 6)[0].contains("PgUp at the top reads older messages."),
        "{:?}",
        texts(&list, 76, 6)
    );
    assert!(list.help().contains(&("PgUp", "Older")));

    assert_eq!(list.key(&Key::PageUp), ListKey::Handled);
    assert_eq!(list.entries[list.cursor].id, 90);
    assert!(
        list.next_query(2).is_none(),
        "PgUp below the top only moves"
    );
    assert_eq!(list.key(&Key::Home), ListKey::Handled);
    assert_eq!(list.entries[list.cursor].id, 51);
    let before = answer(&mut list, 3, page((1..=50).map(numbered).collect(), false));
    assert_eq!(before, Some(numbered(51).cursor()));
    assert_eq!(list.entries.len(), 100);
    assert_eq!(
        list.entries[list.cursor].id, 51,
        "the pick stays on its entry"
    );
    assert_eq!(texts(&list, 76, 6)[0], "Start of the messages.");
    assert!(!list.help().contains(&("PgUp", "Older")));
    assert_eq!(list.key(&Key::PageUp), ListKey::Handled);
    assert_eq!(list.entries[list.cursor].id, 41);
    for _ in 0..10 {
        list.key(&Key::PageUp);
    }
    assert_eq!(list.entries[list.cursor].id, 1);
    assert!(
        list.next_query(4).is_none(),
        "no older page is asked for once the engine has none"
    );

    let refreshed: Vec<AgentMessage> = (52..=101).map(numbered).collect();
    answer(&mut list, REFRESH_MS, page(refreshed, true));
    assert_eq!(list.entries.len(), 101, "entries merge by their cursor");
    assert_eq!(
        list.entries[list.cursor].id, 1,
        "a refresh keeps a pick above the newest"
    );
    assert!(!list.older, "the oldest entry is still the engine's oldest");
    list.key(&Key::End);
    answer(
        &mut list,
        2 * REFRESH_MS,
        page((53..=102).map(numbered).collect(), true),
    );
    assert_eq!(
        list.entries[list.cursor].id, 102,
        "the pick follows the newest"
    );
    let shown = texts(&list, 76, 4);
    assert!(
        shown[3].starts_with("> ") && shown[3].contains("Step 102"),
        "{shown:?}"
    );
    let ids: Vec<i64> = list.entries.iter().map(|entry| entry.id).collect();
    assert_eq!(ids, (1..=102).collect::<Vec<_>>());
}

#[test]
fn a_newest_page_past_the_loaded_entries_replaces_them() {
    let mut list = opened();
    answer(&mut list, 0, page((1..=10).map(numbered).collect(), false));
    list.key(&Key::Up);
    answer(
        &mut list,
        REFRESH_MS,
        page((61..=110).map(numbered).collect(), true),
    );
    let ids: Vec<i64> = list.entries.iter().map(|entry| entry.id).collect();
    assert_eq!(
        ids,
        (61..=110).collect::<Vec<_>>(),
        "no gap is drawn as if it held nothing"
    );
    assert!(list.older);
    assert_eq!(
        list.entries[list.cursor].id, 110,
        "a pick that left the list goes to the newest"
    );
}

#[test]
fn at_the_thousand_entry_cap_paging_older_and_a_refresh_keep_the_entries_the_owner_reads() {
    let mut list = opened();
    answer(
        &mut list,
        0,
        page((901..=1100).map(numbered).collect(), true),
    );
    for first in [701, 501, 301, 101] {
        list.key(&Key::Home);
        let before = answer(
            &mut list,
            1,
            page((first..first + 200).map(numbered).collect(), true),
        );
        assert_eq!(before, Some(numbered(first + 200).cursor()));
    }
    assert_eq!(list.entries.len(), KEPT_MAX);
    list.key(&Key::Home);
    assert_eq!(list.entries[list.cursor].id, 101);
    let query = list.next_query(2);
    assert_eq!(
        query,
        Some(SetupQuery::TeamMessages {
            project: project(),
            agent: 2,
            before: Some(numbered(101).cursor()),
        }),
        "Home at the cap reads the page before the oldest entry"
    );
    let older = page((1..=100).map(numbered).collect(), false);
    assert!(list.apply(
        &query.unwrap(),
        &Ok(SetupView::TeamMessages(Box::new(older)))
    ));
    let ids: Vec<i64> = list.entries.iter().map(|entry| entry.id).collect();
    assert_eq!(
        ids,
        (1..=1000).collect::<Vec<_>>(),
        "the page read stays whole and the newest entries leave instead"
    );
    assert_eq!(
        list.entries[list.cursor].id, 101,
        "the pick stays on the entry the owner reads"
    );
    assert_eq!(
        texts(&list, 76, 6)[..2],
        [
            "Start of the messages.",
            "Newer messages are not shown. End goes back to the newest.",
        ]
    );
    assert!(list.help().contains(&("End", "Newest")));
    assert!(
        list.next_query(10 * REFRESH_MS).is_none(),
        "the list stops following new entries while the newest are dropped"
    );

    list.key(&Key::End);
    assert_eq!(list.entries[list.cursor].id, 1000);
    answer(
        &mut list,
        10 * REFRESH_MS,
        page((951..=1150).map(numbered).collect(), true),
    );
    assert_eq!(list.entries.len(), KEPT_MAX);
    assert_eq!(list.entries[0].id, 151);
    assert_eq!(
        list.entries[list.cursor].id, 1150,
        "End follows the newest again"
    );
    assert!(!list.help().contains(&("End", "Newest")));

    for _ in 0..99 {
        list.key(&Key::PageUp);
    }
    assert_eq!(list.entries[list.cursor].id, 160);
    answer(
        &mut list,
        11 * REFRESH_MS,
        page((1001..=1200).map(numbered).collect(), true),
    );
    assert_eq!(
        list.entries[list.cursor].id, 160,
        "a refresh at the cap keeps the entry the owner reads"
    );
    assert_eq!(list.entries[0].id, 151);
    assert_eq!(list.entries.len(), KEPT_MAX);
    assert!(list.help().contains(&("End", "Newest")));
}

#[test]
fn a_redraw_wraps_only_the_entries_in_view_it_has_not_drawn_at_that_width() {
    let mut list = opened();
    answer(
        &mut list,
        0,
        page((801..=1000).map(numbered).collect(), true),
    );
    for first in [601, 401, 201, 1] {
        list.key(&Key::Home);
        answer(
            &mut list,
            1,
            page((first..first + 200).map(numbered).collect(), first > 1),
        );
    }
    assert_eq!(list.entries.len(), KEPT_MAX);
    list.key(&Key::End);
    list.wraps.set(0);
    let shown = texts(&list, 76, 10);
    assert!(
        list.wraps.get() <= shown.len(),
        "a draw wraps only the entries in view, not all {KEPT_MAX}: {} wrapped",
        list.wraps.get()
    );
    list.wraps.set(0);
    assert_eq!(texts(&list, 76, 10), shown);
    assert_eq!(
        list.wraps.get(),
        0,
        "a redraw at the same width wraps nothing again"
    );
    list.key(&Key::Up);
    list.wraps.set(0);
    texts(&list, 76, 10);
    assert_eq!(
        list.wraps.get(),
        2,
        "a move wraps the newly picked entry and the one that came into view"
    );
    list.wraps.set(0);
    let narrow = texts(&list, 50, 10);
    assert!(
        (1..=narrow.len()).contains(&list.wraps.get()),
        "a new width wraps the entries in view again: {} wrapped",
        list.wraps.get()
    );

    list.key(&Key::End);
    texts(&list, 50, 10);
    let mut changed = numbered(1000);
    changed.text = Some("Step 1000 again".into());
    answer(&mut list, REFRESH_MS, page(vec![changed], true));
    let shown = texts(&list, 50, 10);
    assert!(
        shown
            .last()
            .is_some_and(|line| line.ends_with("Step 1000 again")),
        "an entry the engine changed is wrapped again: {shown:?}"
    );
}

#[test]
fn enter_opens_a_task_result_and_the_full_setting_shows_it_whole() {
    let mut done = entry(
        1,
        0,
        Speaker::Engine,
        EntryKind::Finished,
        Some("Finished the readings API"),
    );
    done.result = Some("Added GET /readings.\nCovered by 3 tests.".into());
    done.result_more = true;
    let mut list = opened();
    answer(&mut list, 0, page(vec![done.clone()], false));
    let pad = " ".repeat(28);
    assert_eq!(
        texts(&list, 76, 10),
        [
            "Start of the messages.".to_owned(),
            "> 10:38  Engine < finished: Finished the readings API".to_owned(),
            format!("{pad}Result: Added GET /readings."),
        ]
    );
    assert!(list.help().contains(&("Enter", "Result")));
    list.key(&Key::Enter);
    assert_eq!(
        texts(&list, 76, 10)[2..],
        [
            format!("{pad}Result: Added GET /readings."),
            format!("{pad}Covered by 3 tests."),
            format!("{pad}The whole result is in the task."),
        ]
    );
    assert_eq!(list.lines(76, 10)[3].role, Some(Role::Muted));
    list.key(&Key::Enter);
    assert_eq!(texts(&list, 76, 10).len(), 3);

    let mut full = page(vec![done], false);
    full.settings.results = ResultsShown::Full;
    let mut list = opened();
    answer(&mut list, 0, full);
    assert_eq!(texts(&list, 76, 10).len(), 5, "{:?}", texts(&list, 76, 10));
}

#[test]
fn a_failed_read_says_why_and_reads_again_after_the_refresh_period() {
    let mut list = opened();
    let query = list.next_query(0).unwrap();
    assert!(list.next_query(0).is_none(), "one read at a time");
    assert!(!list.apply(
        &SetupQuery::Team { project: project() },
        &Err("team_agent_not_found".into())
    ));
    assert!(list.apply(&query, &Err("team_agent_not_found".into())));
    assert_eq!(
        texts(&list, 76, 4),
        ["That agent is no longer part of this team."]
    );
    assert_eq!(list.lines(76, 4)[0].role, Some(Role::Warning));
    assert!(list.next_query(REFRESH_MS - 1).is_none());
    let again = list.next_query(REFRESH_MS).unwrap();
    assert_eq!(again, query);
    list.forget_reads();
    assert_eq!(
        list.next_query(REFRESH_MS + 1),
        Some(query),
        "a read lost with the connection is asked again"
    );
    list.key(&Key::Home);
    assert_eq!(list.key(&Key::Esc), ListKey::Closed);
}

#[test]
fn the_lines_fit_the_room_and_keep_the_pick_in_view() {
    let mut list = opened();
    answer(&mut list, 0, page((1..=40).map(numbered).collect(), false));
    assert_eq!(list.entries.len(), 40, "the page is taken whole");
    for (key, id) in [
        (Key::Up, 39),
        (Key::PageUp, 29),
        (Key::Home, 1),
        (Key::Down, 2),
    ] {
        list.key(&key);
        assert_eq!(list.entries[list.cursor].id, id);
        let shown = texts(&list, 40, 5);
        assert_eq!(shown.len(), 5);
        assert!(
            shown
                .iter()
                .any(|line| line.starts_with("> ") && line.ends_with(&format!("Step {id}"))),
            "{shown:?}"
        );
    }
    assert_eq!(texts(&list, 40, 0).len(), 1);
}

#[test]
#[should_panic(expected = "a Messages page answers the agent it was read for")]
fn a_page_for_another_agent_is_a_bug() {
    let mut list = opened();
    let mut other = page(vec![numbered(1)], false);
    other.agent = 3;
    answer(&mut list, 0, other);
}
