use super::*;

impl ProjectChanges {
    /// Draws the Changes tab: the change sets and the chosen one's details, or its diff.
    /// Takes the content width, the rows available, the time now and the look.
    /// Returns the rows, the commit message cursor as a column and row of those rows
    /// while it is edited, and the help keys.
    pub fn rows(
        &self,
        content: usize,
        room: usize,
        now_ms: i64,
        look: &Look,
    ) -> (Vec<Row>, Option<(usize, usize)>, Help) {
        let mut lines = Vec::new();
        if self.not_ready {
            lines.push(Row::of("Changes", look.heading));
            lines.push(Row::of(NOT_READY, look.fg));
            lines.push(Row::of(NOT_READY_LINE, look.muted));
            return (lines, None, vec![TABS, ("Esc", "Home")]);
        }
        if !self.loaded {
            lines.push(Row::of("Changes", look.heading));
            lines.push(Row::of("Loading changes...", look.muted));
            lines.extend(self.shown_notice().map(|notice| notice.row(look)));
            return (lines, None, vec![TABS, ("Esc", "Home")]);
        }
        let (footer, at) = self.footer(content, look);
        if let Some(diff) = &self.diff {
            let body = room.saturating_sub(footer.len());
            lines.extend(self.diff_rows(diff, content, body, now_ms, look));
        } else {
            lines.extend(self.list_rows(content, room.saturating_sub(footer.len()), now_ms, look));
        }
        let cursor = at
            .map(|(column, row)| (column, lines.len() + row))
            .filter(|(_, row)| *row < room);
        lines.extend(footer);
        (lines, cursor, self.help(now_ms))
    }

    fn list_rows(&self, content: usize, room: usize, now_ms: i64, look: &Look) -> Vec<Row> {
        let mut lines = vec![
            Row::of("Changes", look.heading).push(format!("  {}", self.sets.len()), look.muted),
        ];
        if self.sets.is_empty() {
            lines.extend(
                wrap_words(
                    "No changes yet. When the master or a sub-agent edits files, its changes wait here for you to review and accept.",
                    content,
                )
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
            );
            return lines;
        }
        let shown = LIST_ROWS_MAX.min(room.saturating_sub(8).max(1));
        let first = window(self.cursor, self.sets.len(), shown);
        let widths = [
            AGENT_WIDTH,
            STATE_WIDTH,
            content.saturating_sub(2 + AGENT_WIDTH + STATE_WIDTH + 4),
        ];
        for (index, set) in self.sets.iter().enumerate().skip(first).take(shown) {
            let cells = vec![
                Row::of(listed_name(set), look.fg),
                state_row(set, look),
                Row::of(size_line(set), look.muted),
            ];
            lines.push(list_row(
                columns(&widths, cells),
                content,
                index == self.cursor && self.selected.as_ref() == Some(&set.id),
                true,
                look,
            ));
        }
        lines.push(Row::default());
        if let Some(set) = self.chosen() {
            lines.extend(details(set, content, now_ms, look));
        } else if self.dropped {
            lines.push(Row::of(
                "The change set you picked left the list. Pick one with \u{2191}\u{2193}.",
                look.muted,
            ));
        }
        lines.truncate(room.max(1));
        lines
    }

    /// Draws the open diff under the chosen set's details, from the line the owner
    /// scrolled to, and records, in a painted drawing only, once the last line of a complete
    /// diff was drawn whole on screen, which Accept and Discard wait for after a move
    /// (RVA-1). Inputs: the diff, the content width, the rows available, the time now and the
    /// look. Output: the rows.
    fn diff_rows(
        &self,
        diff: &Diff,
        content: usize,
        room: usize,
        now_ms: i64,
        look: &Look,
    ) -> Vec<Row> {
        let mut lines = Vec::new();
        if let Some(set) = self.chosen() {
            lines.push(spread(
                content,
                Row::of(crate::landing::change_set_name(set), look.heading),
                state_row(set, look),
            ));
            lines.extend(details(set, content, now_ms, look).into_iter().skip(1));
        }
        let listed = listed_files(&diff.files);
        for file in listed.shown {
            lines.push(Row::of(format!("  {}", file_line(file)), look.fg));
            for mark in file_marks(file) {
                lines.push(Row::of(format!("    ! {mark}"), look.warning));
            }
        }
        if listed.marked_hidden > 0 {
            lines.push(Row::of(
                match listed.marked_hidden {
                    1 => "  1 more file needs checking before you accept.".to_owned(),
                    hidden => format!("  {hidden} more files need checking before you accept."),
                },
                look.warning,
            ));
        }
        if listed.plain_hidden > 0 {
            lines.push(Row::of(
                format!("  and {} more files", listed.plain_hidden),
                look.muted,
            ));
        }
        if diff.files_cut {
            lines.push(Row::of(
                format!(
                    "Only the first {FILES_MAX} changed files are listed. Files after them, and their marks, are not shown."
                ),
                look.warning,
            ));
        }
        if diff.truncated {
            lines.push(Row::of(
                "Part of this diff is not shown: it is longer than CrazyAgent shows at once.",
                look.warning,
            ));
        }
        lines.push(Row::default());
        if diff.lines.is_empty() {
            let waiting = if diff.next.is_some() || (diff.moved && !diff.complete) {
                "Loading changes..."
            } else {
                "No text changes to show."
            };
            diff.seen_all
                .set(diff.seen_all.get() || (self.painting.get() && diff.complete));
            lines.push(Row::of(waiting, look.muted));
            return lines;
        }
        let above = lines.len();
        let text_rows = room.saturating_sub(above).max(1);
        diff.width.set(content.max(1));
        let mut inside = diff
            .lines
            .iter()
            .take(diff.top)
            .rev()
            .find(|line| line.starts_with(DIFF_HEADER))
            .is_some_and(|header| diff.marked_header(header));
        let mut shown = 0;
        let mut drawn_to = diff.top;
        for (index, line) in diff.lines.iter().enumerate().skip(diff.top) {
            if shown >= text_rows {
                break;
            }
            if line.starts_with(DIFF_HEADER) {
                inside = diff.marked_header(line);
            }
            let style = diff_style(line, look);
            if inside {
                let pieces = crate::tui::text::wrap(&printable(line.clone()), content.max(1));
                let skip = match index == diff.top {
                    true => diff.skip.min(pieces.len().saturating_sub(1)),
                    false => 0,
                };
                let left = pieces.len() - skip;
                let taken = left.min(text_rows - shown);
                for piece in pieces.into_iter().skip(skip).take(taken) {
                    lines.push(Row::of(piece, style).pad(content));
                    shown += 1;
                }
                if line.is_empty() {
                    lines.push(Row::of(String::new(), style).pad(content));
                    shown += 1;
                }
                if taken == left {
                    drawn_to = index + 1;
                }
            } else {
                lines.push(Row::of(line.clone(), style).pad(content));
                shown += 1;
                drawn_to = index + 1;
            }
        }
        let whole = above + shown <= room.max(1);
        lines.truncate(room.max(1));
        diff.seen_all.set(
            diff.seen_all.get()
                || (self.painting.get() && diff.complete && whole && drawn_to == diff.lines.len()),
        );
        lines
    }

    fn footer(&self, content: usize, look: &Look) -> (Vec<Row>, Option<(usize, usize)>) {
        let Some(ask) = self.asking else {
            let rows = self
                .shown_notice()
                .into_iter()
                .map(|notice| notice.row(look))
                .collect();
            return (rows, None);
        };
        if ask == Ask::Accept {
            return self.message_rows(content, look);
        }
        let (question, size) = self.question(ask);
        let mut rows = vec![
            Row::of(question, look.warning)
                .push("   y", look.heading)
                .push(" Discard   ", look.fg)
                .push("n", look.heading)
                .push(" Keep reviewing", look.fg),
        ];
        rows.extend(size.map(|size| Row::of(size, look.fg)));
        let explain = "The agent's working copy goes back to your branch. You can restore these changes for 7 days.";
        rows.extend(
            wrap_words(explain, content)
                .into_iter()
                .map(|line| Row::of(line, look.muted)),
        );
        (rows, None)
    }

    /// The question an open Accept or Discard asks, naming the change set it is about, with
    /// that set's size and the short head it was reviewed at. Input: the question.
    /// Output: the question line and the size line, or a plain question if the set is gone.
    fn question(&self, ask: Ask) -> (String, Option<String>) {
        let verb = match ask {
            Ask::Accept => "Accept",
            Ask::Discard => "Discard",
        };
        let asked = self.target.as_ref().and_then(|(id, head)| {
            let set = self.sets.iter().find(|set| &set.id == id)?;
            let short = head.get(..SHORT_COMMIT).unwrap_or(head);
            Some((agent_name(set), format!("{}, at {short}.", size_line(set))))
        });
        match asked {
            Some((name, size)) => (format!("{verb} the changes of {name}?"), Some(size)),
            None => (format!("{verb} these changes?"), None),
        }
    }

    fn message_rows(&self, content: usize, look: &Look) -> (Vec<Row>, Option<(usize, usize)>) {
        let (question, size) = self.question(Ask::Accept);
        let mut rows = vec![Row::of(question, look.warning)];
        rows.extend(size.map(|size| Row::of(size, look.fg)));
        let landing = match (self.unpicked_problem(), self.landing_branch()) {
            (Some(problem), _) => problem.to_owned(),
            (None, None) => {
                "They land as one commit on your project's Land on branch, under your own Git name."
                    .to_owned()
            }
            (None, Some((name, true))) => format!(
                "They land as one commit on {name}, the branch checked out in your project folder, under your own Git name."
            ),
            (None, Some((name, false))) => format!(
                "They land as one commit on {name}, under your own Git name. That branch isn't checked out, so the files in your project folder stay as they are."
            ),
        };
        rows.extend(
            wrap_words(
                &format!(
                    "{landing} CrazyAgent did not check them. Edit the commit message if you like."
                ),
                content,
            )
            .into_iter()
            .map(|line| Row::of(line, look.muted)),
        );
        rows.extend(self.shown_notice().map(|notice| notice.row(look)));
        if let Some((name, _)) = self.landing_branch() {
            rows.push(Row::of("Branch", look.heading).push(format!("  {name}"), look.fg));
        }
        rows.push(Row::of("Commit message", look.heading));
        let (lines, (line, column)) = self.message.wrap(content.saturating_sub(2).max(1));
        let first = window(line, lines.len(), MESSAGE_ROWS);
        let cursor = (2 + column, rows.len() + line - first);
        for text in lines.into_iter().skip(first).take(MESSAGE_ROWS) {
            rows.push(Row::of(format!("  {text}"), look.fg).pad(content));
        }
        if self.message.value().trim().is_empty() {
            rows.push(Row::of("Write a commit message to accept.", look.warning));
        }
        if self.unpicked_problem().is_some() {
            rows.push(Row::of(
                "Press Tab to pick the branch they land on.",
                look.warning,
            ));
        }
        (rows, Some(cursor))
    }

    fn help(&self, now_ms: i64) -> Help {
        match self.asking {
            Some(Ask::Accept) => {
                let mut help = vec![
                    ("Enter", "Accept"),
                    ("Ctrl-J", "New line"),
                    ("Esc", "Keep reviewing"),
                ];
                if self.branches.as_ref().is_some_and(|(_, listed)| {
                    listed.branches.len() > 1
                        || (listed.land_on_problem.is_some() && !listed.branches.is_empty())
                }) {
                    help.insert(1, ("Tab", "Branch"));
                }
                return help;
            }
            Some(Ask::Discard) => return vec![("y", "Discard"), ("n", "Keep reviewing")],
            None => {}
        }
        let mut help = if self.diff.is_some() {
            vec![("\u{2191}\u{2193}", "Scroll")]
        } else {
            vec![("\u{2191}\u{2193}", "Move"), ("Enter", "Show changes")]
        };
        if let Some(actions) = self.actions(now_ms) {
            if actions.accept {
                help.push(("a", "Accept"));
            }
            if actions.discard {
                help.push(("d", "Discard"));
            }
            if actions.update {
                help.push(("u", "Ask agent to update"));
            }
            if actions.restore {
                help.push(("r", "Restore"));
            }
        }
        if !self.sets.is_empty() {
            help.push(("O", "Open in IDE"));
        }
        if self.diff.is_some() {
            help.push(("v", "Edit your copy"));
            help.push(("Esc", "Back to the list"));
        } else {
            help.extend([TABS, ("Esc", "Home")]);
        }
        help
    }
}
