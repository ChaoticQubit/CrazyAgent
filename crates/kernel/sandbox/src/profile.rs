/// The sandbox runner and the shims every confined agent starts through: the session and
/// pane shims, the pane keeper and its words, and the trait a boundary wraps a command with.
pub mod command;
/// The limits, the Seatbelt lists, the notices and refusal codes, and the variables the
/// engine itself sets in every agent's environment.
pub mod consts;
