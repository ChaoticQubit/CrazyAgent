use super::super::native_auth::claude_code::{Event, Job};
use super::NativeOwner;

impl NativeOwner {
    pub(super) async fn claude_code(&mut self, job: Job) -> Event {
        let Job {
            context,
            submission,
        } = job;
        let request = submission.request;
        let result = if !self.ready {
            Err("native_auth_owner_unavailable".into())
        } else if request.query.scope != context.scope() {
            Err("native_auth_owner_identity_changed".into())
        } else if let Some(controller) = self.controller.as_mut() {
            if controller.identity() != &context.basis.identity {
                Err("native_auth_owner_identity_changed".into())
            } else {
                controller
                    .submit_claude_auth_code(request.clone(), submission.secret)
                    .await
                    .map_err(|_| "native_auth_claude_input_outcome_unknown".into())
            }
        } else {
            Err("native_auth_owner_unavailable".into())
        };
        Event {
            context,
            request,
            result,
        }
    }
}
