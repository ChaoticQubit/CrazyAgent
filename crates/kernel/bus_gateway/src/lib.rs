#![forbid(unsafe_code)]

mod gateway;
mod grants;

#[cfg(any(test, feature = "test-support"))]
pub use gateway::AllOn;
pub use gateway::{Caller, Gateway, Refusals, Switches};
pub use grants::{Grants, PERMISSIONS_MAX, PLUGINS_MAX};

#[cfg(test)]
mod core_tests;
#[cfg(test)]
mod table_tests;
#[cfg(test)]
mod tests;
