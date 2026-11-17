use super::*;

impl App {
    pub fn selected_project(&self) -> Option<&StoredProject> {
        self.selected.and_then(|index| self.projects.get(index))
    }

    pub fn project_tasks(&self) -> impl Iterator<Item = &ExecutionContext> {
        self.tasks.iter().filter(|task| {
            self.selected_project()
                .is_some_and(|p| p.project_id == task.project_id)
        })
    }

    pub fn selected_task(&self) -> Option<&ExecutionContext> {
        self.project_tasks().nth(self.task)
    }

    pub fn is_creating_project(&self) -> bool {
        self.creating_project || self.declaration.open
    }

    pub fn take_command(&mut self) -> Option<Command> {
        self.command.take()
    }

    pub fn bind_project_request(&mut self, request_id: RequestId) {
        assert!(self.pending_creation.is_none());
        assert!(self.command.is_none());
        let generation = self
            .submitted_generation
            .take()
            .expect("project dispatch requires submitted generation");
        self.pending_creation = Some((request_id, generation));
    }

    pub fn has_command(&self) -> bool {
        self.command.is_some()
    }

    pub fn refresh_command(&self) -> Command {
        if let Some(task) = self.selected_task() {
            if self.focus == Focus::Settings && self.config.is_none() {
                return Command::TaskConfig {
                    task_id: task.task_id.clone(),
                };
            }
            if matches!(self.focus, Focus::Plan | Focus::Supervision) && self.plan.is_none() {
                return Command::TaskPlan {
                    task_id: task.task_id.clone(),
                };
            }
        }
        Command::Snapshot
    }
}
