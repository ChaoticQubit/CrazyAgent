#![forbid(unsafe_code)]

mod gateway;
mod grants;

pub use gateway::{Bound, Gateway, Refusals};
pub use grants::{Grants, PERMISSIONS_MAX, PLUGINS_MAX};

#[cfg(test)]
mod overhead_tests;
#[cfg(test)]
mod table_tests;
#[cfg(test)]
mod tests;
