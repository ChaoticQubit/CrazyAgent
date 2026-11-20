mod clip;
mod keys;
#[cfg(test)]
pub(super) mod rows;
mod signals;
mod surface;
mod term;
mod view;

pub(super) use clip::Boards;
#[cfg(test)]
pub(super) use surface::RECT_MAX;
pub(super) use surface::{Rect, SCALE_MAX, rect};
pub(super) use term::Term;
