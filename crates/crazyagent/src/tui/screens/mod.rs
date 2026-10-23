pub(in crate::tui) mod new_project;
mod new_project_input;
pub(in crate::tui) mod new_project_view;
pub(in crate::tui) mod project;
pub(in crate::tui) mod project_markdown;
pub(in crate::tui) mod project_master;
pub(in crate::tui) mod project_pool;
pub(in crate::tui) mod project_time;
pub(in crate::tui) mod project_view;

#[cfg(test)]
mod new_project_frames;
#[cfg(test)]
mod new_project_tests;
#[cfg(test)]
mod project_frames;
#[cfg(test)]
mod project_tests;
