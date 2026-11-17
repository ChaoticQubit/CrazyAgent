#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod dacl;
pub mod grants;
pub mod plan;
pub mod profile;
pub mod quote;
pub mod request;
pub mod snapshot;
#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::{
    BoundaryError, ProcessTree, file_id, final_name, listener_beside, owner_only, release, run,
};
