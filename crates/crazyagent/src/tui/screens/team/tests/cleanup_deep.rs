use super::*;

/// A removal kept for a tool cache folder too deep to remove names that folder in the owner's
/// sentence, under the plain lead when it removed no agent and the count lead when some went.
#[test]
fn a_folder_too_deep_to_remove_is_named_for_the_owner_to_delete() {
    use crate::tui::client::cleanup::{CleanupQuery, CleanupReply};
    let folder = "/Users/ada/Library/Application Support/CrazyAgent/tool-cache/ab12/agent-1";
    let sentence = format!(
        "An agent's folder has folders nested more than 64 deep, which CrazyAgent does not remove: {folder}. Delete it yourself, then try again."
    );
    for (removed, lead) in [
        (Vec::new(), "Cleanup kept an agent and stopped there."),
        (vec![4], "Removed 1 worktree, kept 1:"),
    ] {
        let mut shell = team();
        shell.key(Key::Char('c'));
        let read = CleanupQuery::Finished { project: project() };
        cleanup_sent(&mut shell);
        let agents = vec![
            finished(4, "Ada (@login-page)", 3 * 1024 * 1024),
            finished(3, "Lin (@docs)", 1024 * 1024),
        ];
        shell.arrive(cleanup_reply(read, Ok(CleanupReply::Finished(agents))));
        shell.key(Key::Char('y'));
        let sent = cleanup_sent(&mut shell);
        shell.arrive(cleanup_reply(
            sent[0].clone(),
            Ok(CleanupReply::Removed(crate::team::cleanup::Removal {
                agents: removed,
                kept: Some("cleanup_folder_too_deep".into()),
                folder: Some(folder.into()),
            })),
        ));
        let message = shell.ctx.message().unwrap();
        assert_eq!(message.lead, lead);
        assert_eq!(message.rest, sentence);
    }
}
