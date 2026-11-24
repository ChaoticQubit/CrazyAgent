use super::*;

impl TeamRuntime {
    /// Tells who should act when a whole turn of an agent working in its own clone landed
    /// nothing new while the clone holds uncommitted edits: the lead of a worker, and the
    /// owner in a project note unless the lead lands that worker's work. The agent itself is
    /// not told here; its next task turn is ([`Self::uncommitted_line`]). Inputs: whether the
    /// turn ended, the project, the workspace, the lead with its landing and what ended.
    /// Output: any store failure.
    pub(super) fn note_uncommitted(
        &mut self,
        whole: bool,
        project: &ProjectId,
        workspace: &crate::landing::Workspace,
        (lead, lands): (Option<i64>, Option<crate::team::ChildLanding>),
        what: &str,
    ) -> Result<(), EngineError> {
        if !whole {
            return Ok(());
        }
        let Some(held) = self.held_uncommitted(
            &workspace.repository,
            &workspace.worktree,
            &workspace.git_dir,
        ) else {
            return Ok(());
        };
        let note = crate::landing::retire::uncommitted::note_line(what, &held);
        if let Some(lead) = lead {
            self.note_lead(project, lead, note.clone())?;
        }
        if lands != Some(crate::team::ChildLanding::Lead) {
            self.note(project, &note)?;
        }
        Ok(())
    }

    /// What a sub-agent's task turn reads about the edits its own clone holds uncommitted,
    /// while the project's Uncommitted edits reminder is On, its default. Takes the project
    /// and the agent. Returns the line, empty for a linked worktree, a clean clone, the
    /// reminder Off or a clone that cannot be read.
    pub(super) fn uncommitted_line(&self, project: &ProjectId, agent: i64) -> String {
        let on = crate::coord::settings::read(&self.connection, project).is_ok_and(|settings| {
            settings.uncommitted_reminder == crate::coord::settings::UncommittedReminder::On
        });
        let row: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![agent, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((repository, worktree, git_dir)) = row.filter(|_| on) else {
            return String::new();
        };
        if recorded_layout(&worktree, &git_dir) != Layout::Clone {
            return String::new();
        }
        self.held_uncommitted(
            Path::new(&repository),
            Path::new(&worktree),
            Path::new(&git_dir),
        )
        .map(|held| crate::landing::retire::uncommitted::agent_line(&held))
        .unwrap_or_default()
    }

    /// Lists what an agent's own clone holds beyond its branch tip, logging a clone it
    /// cannot read by its plain code. Takes the three paths. Returns the listing, or None
    /// for a linked worktree, a clean clone or one it cannot read.
    fn held_uncommitted(
        &self,
        repository: &Path,
        worktree: &Path,
        git_dir: &Path,
    ) -> Option<crate::landing::retire::uncommitted::Uncommitted> {
        let held = crate::landing::open(&self.root).and_then(|db| {
            crate::landing::retire::uncommitted::uncommitted(&db, repository, worktree, git_dir)
        });
        match held {
            Ok(held) => held,
            Err(problem) => {
                eprintln!(
                    "landing_uncommitted_unread cause={}",
                    crate::protocol::plain_code(&problem.0)
                );
                None
            }
        }
    }

    /// What a harness reads about its git branch with each task (DEC-100): its task branch,
    /// and for a native lead in its own clone (DEC-111 Q10) where its sub-agents work: each
    /// teammate in `.worktrees/<name>` on `<task branch>--<name>`, merged before it reports
    /// done, or, for a Codex lead with tools, its sub-agents in its own worktree (PA-7).
    /// Takes the project and the agent. Returns the line, empty with no task branch.
    pub(super) fn branch_line(&self, project: &ProjectId, agent: i64) -> String {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![agent, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((worktree, git_dir)) = row else {
            return String::new();
        };
        let branch = crate::landing::open(&self.root)
            .and_then(|db| {
                crate::landing::agent_branch(&db, Path::new(&worktree), Path::new(&git_dir))
            })
            .ok()
            .flatten();
        let Some(branch) = branch.filter(|branch| crate::agent_git::is_agent_branch(branch)) else {
            return String::new();
        };
        if recorded_layout(&worktree, &git_dir) == Layout::Linked {
            return format!("\nYour branch is {branch}; the engine commits your edits there.");
        }
        if self.has_lead(agent) {
            return format!(
                "\nYour branch is {branch}, checked out in your own worktree and cut from your lead's commit: commit your work there. Your lead merges it into its task branch after you call {finish}().",
                finish = crate::team::tool::FINISH_WORK_TOOL
            );
        }
        let plain = format!(
            "\nYour task branch is {branch}, checked out in your worktree: commit your work there."
        );
        if !matches!(
            crate::team::lead_mode(&self.connection, project, agent),
            Ok(crate::coord::settings::SubagentMode::Native)
        ) {
            return plain;
        }
        match self.threads_lead_tools(agent) {
            Some(false) => plain,
            Some(true) => format!(
                "{plain} Your Codex sub-agents work in this same worktree on {branch}, so give each its own files and never the same file to two of them. They must not call the team tools, because their calls count as yours: wait for each one, close it, then commit and report done yourself."
            ),
            None => format!(
                "{plain} Each teammate of your agent team works only in its own worktree {folder}/<name> inside your worktree, on its own branch {branch}--<name>, which CrazyAgent makes for it when it starts. Merge each finished teammate branch into {branch} before you report done; nothing under {folder} lands by itself.",
                folder = crate::agent_git::TEAMMATES_FOLDER
            ),
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "uncommitted_note_tests.rs"]
mod uncommitted_note_tests;
