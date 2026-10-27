use crate::project_setup::ProjectContext;
use crate::team::{NodeKind, TeamGraph};

pub(in crate::tui) const PROMPT_LINES_MAX: usize = 8;
pub(in crate::tui) const MENU_SHOWN_MAX: usize = 6;
const HISTORY_MAX: usize = 50;
const TAG_CANDIDATES_MAX: usize = 256;
const MASTER_TAG: &str = "@master";
const FILES_QUERY_BYTES_MAX: usize = 256;

/// What one `/` command of the master console does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) enum Action {
    Help,
    Clear,
    Master,
    Agents,
    Changes,
    Usage,
    Process,
    Stop,
}

/// One `/` command: the word after the slash, its one-line description and its action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tui) struct Command {
    pub name: &'static str,
    pub about: &'static str,
    pub action: Action,
}

/// The console's commands in menu order; a new command is one more entry here.
pub(in crate::tui) const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        about: "Show the commands",
        action: Action::Help,
    },
    Command {
        name: "clear",
        about: "Clear this view; nothing is deleted",
        action: Action::Clear,
    },
    Command {
        name: "master",
        about: "Open the live master's terminal",
        action: Action::Master,
    },
    Command {
        name: "agents",
        about: "Open the Agents tab",
        action: Action::Agents,
    },
    Command {
        name: "changes",
        about: "Open the Changes tab",
        action: Action::Changes,
    },
    Command {
        name: "usage",
        about: "Open Usage",
        action: Action::Usage,
    },
    Command {
        name: "process",
        about: "Open this project's processes",
        action: Action::Process,
    },
    Command {
        name: "stop",
        about: "Stop the master's turn",
        action: Action::Stop,
    },
];

/// The menu a prompt opens while the whole prompt is one `/` word, as the desktop console.
/// Takes the whole prompt text.
/// Returns the commands whose names start with the typed word, or contain it when none
/// starts with it, or None when no menu is open.
pub(in crate::tui) fn menu(text: &str) -> Option<Vec<&'static Command>> {
    let word = text.strip_prefix('/')?.to_lowercase();
    if word.chars().any(char::is_whitespace) {
        return None;
    }
    let matching = |test: &dyn Fn(&str) -> bool| -> Vec<&'static Command> {
        COMMANDS
            .iter()
            .filter(|command| test(command.name))
            .collect()
    };
    let starts = matching(&|name| name.starts_with(&word));
    Some(if starts.is_empty() {
        matching(&|name| name.contains(&word))
    } else {
        starts
    })
}

/// Reads a prompt about to be sent: it is a command only when the whole trimmed prompt is one
/// word of `/`, a letter, then letters, digits or hyphens; anything else, a path or a
/// message that starts with a `/` word included, is a message.
/// Takes the whole prompt text.
/// Returns None for a message, the action of a known command, or the unknown `/word`.
pub(in crate::tui) fn command(text: &str) -> Option<Result<Action, String>> {
    let text = text.trim();
    if text.chars().any(char::is_whitespace) {
        return None;
    }
    let word = text.strip_prefix('/')?;
    let mut letters = word.chars();
    let named = letters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && letters.all(|rest| rest.is_ascii_alphanumeric() || rest == '-');
    if !named {
        return None;
    }
    let name = word.to_ascii_lowercase();
    let found = COMMANDS.iter().find(|command| command.name == name);
    Some(
        found
            .map(|command| command.action)
            .ok_or_else(|| format!("/{name}")),
    )
}

/// The `@` word being typed at the cursor: an `@` at line start or after a space, then no space.
/// Takes the prompt text before the cursor.
/// Returns the word after the `@`, possibly empty, or None when no tag is being typed.
pub(in crate::tui) fn tag_query(head: &str) -> Option<&str> {
    let start = head.rfind('@')?;
    let before = head[..start].chars().next_back();
    let word = &head[start + 1..];
    let opens = before.is_none_or(char::is_whitespace);
    (opens && !word.chars().any(char::is_whitespace)).then_some(word)
}

/// Reads the role handle out of a team node label such as `Ada (@page)`.
/// Takes the label.
/// Returns the tag with its `@`, or None for a label without one.
fn label_tag(label: &str) -> Option<&str> {
    let open = label.rfind("(@")?;
    let tag = label[open + 1..].strip_suffix(')')?;
    (tag.len() > 1 && !tag.chars().any(char::is_whitespace)).then_some(tag)
}

/// Every tag the prompt can complete: `@master`, then each sub-agent's role handle from the
/// team graph, then the project files the engine listed, then each project context file,
/// without repeats or paths with spaces and at most `TAG_CANDIDATES_MAX`.
/// Takes the team graph, the listed files and the project context, when read.
/// Returns the tags in that order.
pub(in crate::tui) fn tags(
    graph: Option<&TeamGraph>,
    files: Option<&[String]>,
    context: Option<&ProjectContext>,
) -> Vec<String> {
    let handles = graph
        .into_iter()
        .flat_map(|graph| &graph.nodes)
        .filter(|node| node.kind == NodeKind::Agent)
        .filter_map(|node| label_tag(&node.label))
        .map(str::to_owned);
    let listed = files.into_iter().flatten().map(String::as_str);
    let documents = context
        .into_iter()
        .flat_map(|context| &context.items)
        .map(|item| item.path.as_str());
    let files = listed
        .chain(documents)
        .filter(|path| !path.is_empty() && !path.chars().any(char::is_whitespace))
        .map(|path| format!("@{path}"));
    let mut tags: Vec<String> = Vec::new();
    for tag in std::iter::once(MASTER_TAG.to_owned())
        .chain(handles)
        .chain(files)
    {
        if tags.len() == TAG_CANDIDATES_MAX {
            break;
        }
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    assert!(tags.len() <= TAG_CANDIDATES_MAX);
    tags
}

/// The tags that complete the typed `@` word, case-insensitively, as the desktop console:
/// tags starting with it first, then tags containing it, at most `MENU_SHOWN_MAX`.
/// Takes the candidates and the word after the `@`.
/// Returns the matching tags in that order.
pub(in crate::tui) fn completions<'a>(tags: &'a [String], query: &str) -> Vec<&'a str> {
    let query = query.to_lowercase();
    let lower: Vec<String> = tags.iter().map(|tag| tag[1..].to_lowercase()).collect();
    let starts = (0..tags.len()).filter(|&at| lower[at].starts_with(&query));
    let contains =
        (0..tags.len()).filter(|&at| !lower[at].starts_with(&query) && lower[at].contains(&query));
    starts
        .chain(contains)
        .take(MENU_SHOWN_MAX)
        .map(|at| tags[at].as_str())
        .collect()
}

/// The query one project file read sends for the typed `@` word: the word cut at a character
/// edge to the engine's `FILES_QUERY_BYTES_MAX`.
/// Takes the word after the `@`.
/// Returns the query.
pub(in crate::tui) fn files_query(word: &str) -> String {
    let end = word
        .char_indices()
        .map(|(at, c)| at + c.len_utf8())
        .take_while(|end| *end <= FILES_QUERY_BYTES_MAX)
        .last()
        .unwrap_or(0);
    let query = word[..end].to_owned();
    assert!(query.len() <= FILES_QUERY_BYTES_MAX);
    query
}

/// Splits one line into plain text and `@` tags, a tag being an `@` word at line start or
/// after a space.
/// Takes the line.
/// Returns the parts in order, each with whether it is a tag.
pub(in crate::tui) fn tag_parts(line: &str) -> Vec<(&str, bool)> {
    let mut parts = Vec::new();
    let mut plain = 0;
    let mut index = 0;
    while let Some(offset) = line[index..].find('@') {
        let start = index + offset;
        let opens = line[..start]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        let end = line[start..]
            .find(char::is_whitespace)
            .map_or(line.len(), |length| start + length);
        if opens && end > start + 1 {
            if plain < start {
                parts.push((&line[plain..start], false));
            }
            parts.push((&line[start..end], true));
            plain = end;
        }
        index = end.max(start + 1);
    }
    if plain < line.len() {
        parts.push((&line[plain..], false));
    }
    assert_eq!(
        parts.iter().map(|(part, _)| part.len()).sum::<usize>(),
        line.len()
    );
    parts
}

/// The prompt's sent messages for Up and Down, newest last, at most `HISTORY_MAX`.
/// Holds the messages and which one the prompt shows while recalling.
#[derive(Debug, Default)]
pub(in crate::tui) struct History {
    sent: Vec<String>,
    at: Option<usize>,
}

impl History {
    /// Keeps one sent message and ends any recall.
    /// Takes the message; an empty one or a repeat of the newest is not kept again.
    /// Returns nothing.
    pub fn push(&mut self, text: &str) {
        self.at = None;
        if text.is_empty() || self.sent.last().is_some_and(|last| last == text) {
            return;
        }
        if self.sent.len() == HISTORY_MAX {
            self.sent.remove(0);
        }
        self.sent.push(text.to_owned());
        assert!(self.sent.len() <= HISTORY_MAX);
    }

    /// Steps the recall one message older (Up) or newer (Down).
    /// Takes the direction and the prompt text; recall starts only from an empty prompt and
    /// stops once the recalled text was edited.
    /// Returns the text the prompt shows now, or None when the key does not recall.
    pub fn step(&mut self, older: bool, prompt: &str) -> Option<String> {
        let showing = match self.at {
            None if prompt.is_empty() => None,
            Some(at) if self.sent[at] == prompt => Some(at),
            _ => {
                self.at = None;
                return None;
            }
        };
        let next = match (showing, older) {
            (None, true) => self.sent.len().checked_sub(1),
            (None, false) => return None,
            (Some(at), true) => Some(at.saturating_sub(1)),
            (Some(at), false) if at + 1 < self.sent.len() => Some(at + 1),
            (Some(_), false) => {
                self.at = None;
                return Some(String::new());
            }
        };
        self.at = next;
        next.map(|at| self.sent[at].clone())
    }
}

#[cfg(test)]
#[path = "console/tests.rs"]
mod tests;
