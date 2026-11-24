/// The Linux sandbox helper that runs inside each sandboxed launch: its request parse,
/// its Landlock and seccomp rules, its bridge and its egress.
pub mod helper;
