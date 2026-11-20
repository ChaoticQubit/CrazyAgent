#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod dacl;
pub mod grants;
pub mod pipe;
pub mod plan;
/// Test only: lowered principals a test thread impersonates, a restricted token (R4) and the
/// user's medium duplicate (R6), behind the `test-principals` feature that only a
/// dev-dependency turns on, so no release build carries them.
#[cfg(all(windows, feature = "test-principals"))]
pub mod principals;
/// Test only: file states the N1 pin rows set up and no safe API reaches, a writable mapped
/// view, a supersede through delete sharing and a NULL access list, behind the same
/// `test-principals` feature, so no release build carries them.
#[cfg(all(windows, feature = "test-principals"))]
pub mod test_files;
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
    BoundaryError, ProcessTree, changeable_part, file_id, final_name, listener_beside,
    output_not_inherited, owner_only, release, run, system_folder, take_stdin_pipe,
    unreadable_drive_root, unreadable_read_root, unreadable_root,
};
