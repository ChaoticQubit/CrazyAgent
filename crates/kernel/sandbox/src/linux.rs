/// The Linux sandbox helper that runs inside each sandboxed launch: its request parse,
/// its Landlock and seccomp rules, its bridge and its egress.
pub mod helper;
/// The Linux sandbox as the engine launches it: the backend, the launch command, the
/// helper lookup and the host read.
pub mod launch;
