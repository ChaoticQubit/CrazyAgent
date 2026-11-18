#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod admin_step;
pub mod dacl;
pub mod grants;
pub mod pipe;
pub mod plan;
pub mod private;
pub mod profile;
pub mod quote;
pub mod request;
pub mod snapshot;
#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{
    BoundaryError, ProcessTree, changeable_part, device_grant_held, file_id, final_name,
    listener_beside, missing_device_grant, owner_only, release, run, unreadable_drive_root,
    unreadable_read_root, unreadable_root,
};
