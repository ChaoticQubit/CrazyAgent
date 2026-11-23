/// A project's hidden folders and the running agents that can still open a folder hidden
/// after they started, kept in the engine database; the checks on them are in
/// [`kernel_sandbox::hidden_paths`].
pub mod hidden;
/// A project's extra read-only folders, kept in the engine database; the checks on them are
/// in [`kernel_sandbox::extra_paths`].
pub mod read;
