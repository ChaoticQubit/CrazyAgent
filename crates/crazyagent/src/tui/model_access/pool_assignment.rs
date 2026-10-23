use super::*;
use crate::protocol::ProjectId;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Load,
    Confirm,
    Saving,
    Readback,
    Review,
}

pub(super) struct Assignment {
    basis: Basis,
    pool: AccountPool,
    project: (ProjectId, String),
    stage: Stage,
}

impl Assignment {
    pub(super) fn render(&self, notice: &str, pending: Option<String>) -> String {
        let mut lines = vec![
            "Assign pool to project".into(),
            self.pool.id.as_str().to_owned(),
            self.project
                .1
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect(),
            "Role: Project master".into(),
            "Adds this project scope and master role; both scope lists apply together".into(),
            "Existing membership, policy and assignments are retained".into(),
            notice.chars().take(240).collect(),
        ];
        lines.push(
            match self.stage {
                Stage::Confirm => "Enter confirms assignment | Esc cancels",
                Stage::Saving => "Save pending; Ctrl-R checks original request",
                Stage::Readback => "Reading the saved pool from the owner",
                Stage::Load => "Reading the selected pool from the owner",
                Stage::Review => "Close and reopen to review current project and pool",
            }
            .into(),
        );
        if let Some(pending) = pending {
            lines.push(pending);
        }
        lines.join("\n")
    }

    pub(super) fn assert_consistent(&self, pending: Option<&Submission>) {
        if self.stage == Stage::Saving {
            assert!(pending.is_some_and(|submission| {
                matches!(&submission.edit, ModelAccessEdit::ReplacePool { pool }
                    if pool == &self.pool && submission.basis == self.basis)
            }));
        }
    }
}

impl State {
    pub(in crate::tui) fn select_assignment_project(
        &mut self,
        project: Option<(ProjectId, String)>,
    ) {
        self.assignment_project = project;
    }

    pub(super) fn begin_assignment(&mut self) -> Result<(), String> {
        let Some(project) = self.assignment_project.clone() else {
            self.notice = "Select a project before assigning a pool".into();
            return Ok(());
        };
        let basis = self.writable()?;
        if self.loading() || self.has_pending() {
            return Err("Wait for the current owner operation before assigning a pool".into());
        }
        let Some(ModelAccessRecord::Pool { pool }) = self.items.get(self.selected) else {
            return Err("Select a saved pool to assign".into());
        };
        self.assignment = Some(Assignment {
            basis,
            pool: pool.clone(),
            project,
            stage: Stage::Load,
        });
        self.request_assignment()
    }

    pub(super) fn request_assignment(&mut self) -> Result<(), String> {
        if self.loading() || self.has_pending() {
            return Ok(());
        }
        if let Some(assignment) = &self.assignment
            && matches!(assignment.stage, Stage::Load | Stage::Readback)
        {
            self.read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Pools,
                id: assignment.pool.id.clone(),
            })?;
        }
        Ok(())
    }

    pub(super) fn reduce_assignment(&mut self, event: &UiEvent) -> Result<bool, String> {
        if matches!(
            event,
            UiEvent::Resize(..) | UiEvent::Scroll(_) | UiEvent::FocusNext | UiEvent::FocusPrevious
        ) {
            return Ok(false);
        }
        if self.has_pending() {
            self.notice = "Assignment pending; Ctrl-R checks the original request".into();
            return Ok(true);
        }
        if matches!(event, UiEvent::Cancel) {
            let committed = self
                .assignment
                .as_ref()
                .is_some_and(|assignment| assignment.stage == Stage::Readback);
            self.assignment = None;
            self.flight = None;
            self.job = None;
            self.needs_page = true;
            self.notice = if committed {
                "Assignment view closed; committed save remains; no additional save requested"
            } else {
                "Assignment cancelled; no new save requested"
            }
            .into();
        } else if matches!(event, UiEvent::Submit)
            && !self.loading()
            && let Err(problem) = self.confirm_assignment()
        {
            self.notice = problem;
        }
        self.assert_consistent();
        Ok(true)
    }

    fn confirm_assignment(&mut self) -> Result<(), String> {
        let assignment = self.assignment.as_ref().unwrap();
        if assignment.stage != Stage::Confirm {
            return Ok(());
        }
        if self.assignment_project.as_ref() != Some(&assignment.project) {
            return Err("Selected project changed; close and reopen the assignment".into());
        }
        let mut pool = assignment.pool.clone();
        let project =
            CredentialRef::new(assignment.project.0.as_str()).map_err(|error| error.to_string())?;
        let role = CredentialRef::new("project_leader").map_err(|error| error.to_string())?;
        if !pool.projects.contains(&project) {
            pool.projects.push(project);
        }
        if !pool.roles.contains(&role) {
            pool.roles.push(role);
        }
        let basis = assignment.basis.clone();
        self.submit(basis, ModelAccessEdit::ReplacePool { pool: pool.clone() })?;
        let assignment = self.assignment.as_mut().unwrap();
        assignment.pool = pool;
        assignment.stage = Stage::Saving;
        Ok(())
    }

    pub(super) fn accept_assignment_read(
        &mut self,
        query: &OwnerRead,
        data: &OwnerData,
        basis: &Basis,
    ) -> Result<bool, String> {
        let Some(assignment) = &mut self.assignment else {
            return Ok(false);
        };
        if !matches!(assignment.stage, Stage::Load | Stage::Readback) {
            return Ok(false);
        }
        if query
            != &(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Pools,
                id: assignment.pool.id.clone(),
            })
        {
            return Err("Pool assignment read context mismatch".into());
        }
        let OwnerData::ModelAccessRecord {
            record: ModelAccessRecord::Pool { pool },
            ..
        } = data
        else {
            return Err("Pool assignment requires an actual owner pool record".into());
        };
        if pool != &assignment.pool || basis.identity != assignment.basis.identity {
            assignment.stage = Stage::Review;
            self.notice = "Owner or pool changed; no new assignment requested".into();
            return Ok(true);
        }
        self.basis = Some(basis.clone());
        if assignment.stage == Stage::Readback {
            self.notice = format!(
                "Assigned to project: {} | Project master",
                assignment.project.1
            );
            self.assignment = None;
            self.needs_page = true;
        } else if basis.writable {
            assignment.basis = basis.clone();
            assignment.stage = Stage::Confirm;
            self.notice = "Review the selected project before saving the assignment".into();
        } else {
            assignment.stage = Stage::Review;
            self.notice = "Assignment disabled: current pool is read-only".into();
        }
        Ok(true)
    }

    pub(super) fn settle_assignment(&mut self, applied: bool) {
        if let Some(assignment) = &mut self.assignment {
            assignment.stage = if applied {
                Stage::Readback
            } else {
                Stage::Review
            };
        }
    }
}
