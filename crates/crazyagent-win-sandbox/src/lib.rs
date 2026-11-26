#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod dacl;
pub mod grants;
pub mod name;
pub mod pipe;
pub mod plan;
/// Test only: lowered principals a test thread impersonates, a restricted token (R4) and the
/// user's medium duplicate (R6), behind the `test-principals` feature that only a
/// dev-dependency turns on, so no release build carries them.
#[cfg(all(windows, feature = "test-principals"))]
pub mod principals;
#[cfg(all(feature = "test-principals", not(debug_assertions)))]
compile_error!(
    "test-principals impersonates lowered tokens for tests only and never reaches a release build"
);
pub mod private;
pub mod profile;
pub mod quote;
pub mod request;
pub mod snapshot;
#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{
    BoundaryError, ProcessTree, changeable_part, container_sid, file_id, final_name,
    image_runs_under, listener_beside, owner_only, release, run, system_folder,
    unreadable_drive_root, unreadable_read_root, unreadable_root,
};
