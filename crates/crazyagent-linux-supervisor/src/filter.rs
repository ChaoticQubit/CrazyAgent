#![forbid(unsafe_code)]

pub const ARCH_X86_64: u32 = 0xc000_003e;
pub const ARCH_AARCH64: u32 = 0xc000_00b7;
pub const ALLOW: u32 = 0x7fff_0000;
pub const NOTIFY: u32 = 0x7fc0_0000;
pub const KILL_PROCESS: u32 = 0x8000_0000;
pub const ERRNO: u32 = 0x0005_0000;
pub const X32_BIT: u32 = 0x4000_0000;
pub const ENOSYS: u32 = 38;
pub const EOPNOTSUPP: u32 = 95;
pub const EPERM: u32 = 1;
pub const O_CREAT: u32 = 0o100;
pub const O_WRITES: u32 = 0o1103;
pub const MSG_FASTOPEN: u32 = 0x2000_0000;
pub const F_SETOWN: u32 = 8;
pub const F_SETOWN_EX: u32 = 15;
pub const FIOSETOWN: u32 = 0x8901;
pub const SIOCSPGRP: u32 = 0x8902;
pub const PROGRAM_MAX: usize = 4096;

const LOAD: u16 = 0x20;
const JUMP_EQUAL: u16 = 0x15;
const JUMP_AT_LEAST: u16 = 0x35;
const JUMP_ANY_SET: u16 = 0x45;
const RETURN: u16 = 0x06;
const NR_AT: u32 = 0;
const ARCH_AT: u32 = 4;
const ARGS_AT: u32 = 16;

/// One classic BPF instruction as seccomp reads it: the operation, the two relative jumps
/// and the operand. The same layout as the kernel's `sock_filter`, kept apart from it so
/// the program is built and checked on every system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// A system call the supervisor's filter names, whichever number an architecture gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Connect,
    Sendto,
    Sendmsg,
    Sendmmsg,
    Open,
    Openat,
    Openat2,
    Creat,
    Mkdir,
    Mkdirat,
    Mknod,
    Mknodat,
    Symlink,
    Symlinkat,
    Link,
    Linkat,
    Rename,
    Renameat,
    Renameat2,
    Unlink,
    Unlinkat,
    Rmdir,
    Truncate,
    OpenByHandleAt,
    Kill,
    Tkill,
    Tgkill,
    RtSigqueueinfo,
    RtTgsigqueueinfo,
    PidfdSendSignal,
    Fcntl,
    Ioctl,
}

/// The calls that make a new name in a folder whatever their arguments say.
pub const CREATING: [Call; 12] = [
    Call::Creat,
    Call::Mkdir,
    Call::Mkdirat,
    Call::Mknod,
    Call::Mknodat,
    Call::Symlink,
    Call::Symlinkat,
    Call::Link,
    Call::Linkat,
    Call::Rename,
    Call::Renameat,
    Call::Renameat2,
];

/// The calls that remove an entry or cut a file by its name, which a held entry never
/// allows (DEC-185 S2).
pub const REMOVING: [Call; 4] = [Call::Unlink, Call::Unlinkat, Call::Rmdir, Call::Truncate];

/// The calls that send a signal to a process, a thread or a process group.
pub const SIGNALLING: [Call; 6] = [
    Call::Kill,
    Call::Tkill,
    Call::Tgkill,
    Call::RtSigqueueinfo,
    Call::RtTgsigqueueinfo,
    Call::PidfdSendSignal,
];

/// One architecture's system call numbers: its audit id, whether it has the x32 number
/// space beside its own, and the number of each [`Call`] it has. An architecture without
/// the calls that take no folder descriptor leaves them out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calls {
    pub arch: u32,
    pub x32: bool,
    pub numbers: &'static [(Call, u32)],
}

impl Calls {
    pub const X86_64: Self = Self {
        arch: ARCH_X86_64,
        x32: true,
        numbers: &[
            (Call::Open, 2),
            (Call::Ioctl, 16),
            (Call::Connect, 42),
            (Call::Sendto, 44),
            (Call::Sendmsg, 46),
            (Call::Kill, 62),
            (Call::Fcntl, 72),
            (Call::Truncate, 76),
            (Call::Rename, 82),
            (Call::Mkdir, 83),
            (Call::Rmdir, 84),
            (Call::Creat, 85),
            (Call::Link, 86),
            (Call::Unlink, 87),
            (Call::Symlink, 88),
            (Call::RtSigqueueinfo, 129),
            (Call::Mknod, 133),
            (Call::Tkill, 200),
            (Call::Tgkill, 234),
            (Call::Openat, 257),
            (Call::Mkdirat, 258),
            (Call::Mknodat, 259),
            (Call::Unlinkat, 263),
            (Call::Renameat, 264),
            (Call::Linkat, 265),
            (Call::Symlinkat, 266),
            (Call::RtTgsigqueueinfo, 297),
            (Call::OpenByHandleAt, 304),
            (Call::Sendmmsg, 307),
            (Call::Renameat2, 316),
            (Call::PidfdSendSignal, 424),
            (Call::Openat2, 437),
        ],
    };

    pub const AARCH64: Self = Self {
        arch: ARCH_AARCH64,
        x32: false,
        numbers: &[
            (Call::Fcntl, 25),
            (Call::Ioctl, 29),
            (Call::Mknodat, 33),
            (Call::Mkdirat, 34),
            (Call::Unlinkat, 35),
            (Call::Symlinkat, 36),
            (Call::Linkat, 37),
            (Call::Renameat, 38),
            (Call::Truncate, 45),
            (Call::Openat, 56),
            (Call::Kill, 129),
            (Call::Tkill, 130),
            (Call::Tgkill, 131),
            (Call::RtSigqueueinfo, 138),
            (Call::Connect, 203),
            (Call::Sendto, 206),
            (Call::Sendmsg, 211),
            (Call::RtTgsigqueueinfo, 240),
            (Call::OpenByHandleAt, 265),
            (Call::Sendmmsg, 269),
            (Call::Renameat2, 276),
            (Call::PidfdSendSignal, 424),
            (Call::Openat2, 437),
        ],
    };

    /// The table of the architecture this program was built for. Output: the table, or
    /// none on an architecture the supervisor does not know, where no agent is supervised.
    pub fn host() -> Option<Self> {
        match std::env::consts::ARCH {
            "x86_64" => Some(Self::X86_64),
            "aarch64" => Some(Self::AARCH64),
            _ => None,
        }
    }

    /// The number this architecture gives a call. Input: the call. Output: its number, or
    /// none where the architecture has no such call.
    pub fn number(&self, call: Call) -> Option<u32> {
        let found = self.numbers.iter().find(|(named, _)| *named == call);
        found.map(|(_, number)| *number)
    }

    /// The call a number names here. Input: the number a notification carries. Output:
    /// the call, or none for a number the filter never traps.
    pub fn call(&self, number: u32) -> Option<Call> {
        let found = self.numbers.iter().find(|(_, own)| *own == number);
        found.map(|(call, _)| *call)
    }
}

/// Which guards an agent's filter carries: the connect guard (a port on this computer is
/// reached on loopback only), the create guard (no new `.git` entry), the signal guard
/// (signals stay inside the agent) and the held guard (no write, move, link or removal of
/// a held entry, DEC-185 S2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Guards {
    pub connect: bool,
    pub create: bool,
    pub signal: bool,
    pub held: bool,
}

/// What a rule asks of a call's arguments, read from the registers only: nothing, any bit
/// of a mask set in the low word of an argument, or that word equal to a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Test {
    Always,
    AnySet { arg: u8, mask: u32 },
    Equals { arg: u8, value: u32 },
}

/// One line of the filter: a call, what its registers must show, and what seccomp then
/// returns for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub call: Call,
    pub test: Test,
    pub action: u32,
}

/// The rules the chosen guards need. A call whose deciding argument lies behind a pointer
/// is never tested here: it is trapped whole, or refused whole where a trap could not be
/// served (`openat2`, `open_by_handle_at`, a send that opens a connection). Under the held
/// guard every open that may write is trapped, not only one that may create. Input: the
/// guards. Output: the rules.
pub fn rules(guards: Guards) -> Vec<Rule> {
    let mut rules = Vec::new();
    let mut add = |call, test, action| rules.push(Rule { call, test, action });
    if guards.connect {
        let opens = |arg| Test::AnySet {
            arg,
            mask: MSG_FASTOPEN,
        };
        add(Call::Connect, Test::Always, NOTIFY);
        add(Call::Sendto, opens(3), ERRNO | EOPNOTSUPP);
        add(Call::Sendmsg, opens(2), ERRNO | EOPNOTSUPP);
        add(Call::Sendmmsg, opens(3), ERRNO | EOPNOTSUPP);
    }
    if guards.create || guards.held {
        let mask = if guards.held { O_WRITES } else { O_CREAT };
        let opens = |arg| Test::AnySet { arg, mask };
        for call in CREATING {
            add(call, Test::Always, NOTIFY);
        }
        add(Call::Open, opens(1), NOTIFY);
        add(Call::Openat, opens(2), NOTIFY);
        add(Call::Openat2, Test::Always, ERRNO | ENOSYS);
    }
    if guards.held {
        for call in REMOVING {
            add(call, Test::Always, NOTIFY);
        }
        add(Call::OpenByHandleAt, Test::Always, ERRNO | EPERM);
    }
    if guards.signal {
        for call in SIGNALLING {
            add(call, Test::Always, NOTIFY);
        }
        for value in [F_SETOWN, F_SETOWN_EX] {
            add(Call::Fcntl, Test::Equals { arg: 1, value }, NOTIFY);
        }
        for value in [FIOSETOWN, SIOCSPGRP] {
            add(Call::Ioctl, Test::Equals { arg: 1, value }, NOTIFY);
        }
    }
    rules
}

/// The seccomp program for one architecture and one set of guards: a call from another
/// architecture ends the process, an x32 call is answered as missing, each rule's call is
/// trapped or refused, and every other call runs. Inputs: the architecture's numbers and
/// the guards. Output: the instructions, at most [`PROGRAM_MAX`].
pub fn program(calls: &Calls, guards: Guards) -> Vec<Insn> {
    let load = |k| Insn {
        code: LOAD,
        jt: 0,
        jf: 0,
        k,
    };
    let done = |k| Insn {
        code: RETURN,
        jt: 0,
        jf: 0,
        k,
    };
    let jump = |code, k, jt, jf| Insn { code, jt, jf, k };
    let mut program = vec![
        load(ARCH_AT),
        jump(JUMP_EQUAL, calls.arch, 1, 0),
        done(KILL_PROCESS),
        load(NR_AT),
    ];
    if calls.x32 {
        program.push(jump(JUMP_AT_LEAST, X32_BIT, 0, 1));
        program.push(done(ERRNO | ENOSYS));
    }
    for rule in rules(guards) {
        let Some(number) = calls.number(rule.call) else {
            continue;
        };
        let (code, arg, operand) = match rule.test {
            Test::Always => {
                program.push(jump(JUMP_EQUAL, number, 0, 1));
                program.push(done(rule.action));
                continue;
            }
            Test::AnySet { arg, mask } => (JUMP_ANY_SET, arg, mask),
            Test::Equals { arg, value } => (JUMP_EQUAL, arg, value),
        };
        assert!(arg < 6);
        program.extend([
            jump(JUMP_EQUAL, number, 0, 4),
            load(ARGS_AT + 8 * u32::from(arg)),
            jump(code, operand, 0, 1),
            done(rule.action),
            load(NR_AT),
        ]);
    }
    program.push(done(ALLOW));
    assert!(program.len() <= PROGRAM_MAX);
    program
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct Data {
        nr: u32,
        arch: u32,
        args: [u64; 6],
    }

    fn word(data: &Data, at: u32) -> u32 {
        match at {
            0 => data.nr,
            4 => data.arch,
            16..=63 if at.is_multiple_of(4) => {
                let arg = data.args[((at - 16) / 8) as usize];
                if at.is_multiple_of(8) {
                    arg as u32
                } else {
                    (arg >> 32) as u32
                }
            }
            _ => panic!("the filter reads offset {at}"),
        }
    }

    fn run(program: &[Insn], data: &Data) -> u32 {
        let (mut at, mut held) = (0usize, 0u32);
        for _ in 0..program.len() {
            let insn = program[at];
            let taken = match insn.code {
                0x20 => {
                    held = word(data, insn.k);
                    None
                }
                0x06 => return insn.k,
                0x15 => Some(held == insn.k),
                0x35 => Some(held >= insn.k),
                0x45 => Some(held & insn.k != 0),
                code => panic!("the filter uses instruction {code:#x}"),
            };
            at += 1 + match taken {
                Some(true) => usize::from(insn.jt),
                Some(false) => usize::from(insn.jf),
                None => 0,
            };
        }
        panic!("the filter never returns");
    }

    const ALL: Guards = Guards {
        connect: true,
        create: true,
        signal: true,
        held: false,
    };

    const HELD: Guards = Guards { held: true, ..ALL };

    fn every_guard_set() -> Vec<Guards> {
        (0..16u8)
            .map(|bits| Guards {
                connect: bits & 1 != 0,
                create: bits & 2 != 0,
                signal: bits & 4 != 0,
                held: bits & 8 != 0,
            })
            .collect()
    }

    fn told(calls: &Calls, guards: Guards, nr: u32, args: [u64; 6]) -> u32 {
        let data = Data {
            nr,
            arch: calls.arch,
            args,
        };
        run(&program(calls, guards), &data)
    }

    fn plain(calls: &Calls, guards: Guards, call: Call) -> u32 {
        told(calls, guards, calls.number(call).unwrap(), [0; 6])
    }

    #[test]
    fn a_call_from_another_architecture_ends_the_process_and_an_x32_call_is_not_there() {
        for guards in every_guard_set() {
            let x86 = program(&Calls::X86_64, guards);
            let other = Data {
                nr: 42,
                arch: ARCH_AARCH64,
                args: [0; 6],
            };
            assert_eq!(run(&x86, &other), KILL_PROCESS, "{guards:?}");
            let compat = Data {
                nr: 42,
                arch: 0x4000_0003,
                args: [0; 6],
            };
            assert_eq!(run(&x86, &compat), KILL_PROCESS, "{guards:?}");
            let x32 = told(&Calls::X86_64, guards, X32_BIT | 42, [0; 6]);
            assert_eq!(x32, ERRNO | ENOSYS, "{guards:?}");
            let arm = program(&Calls::AARCH64, guards);
            let foreign = Data {
                nr: 203,
                arch: ARCH_X86_64,
                args: [0; 6],
            };
            assert_eq!(run(&arm, &foreign), KILL_PROCESS, "{guards:?}");
        }
        let high = told(&Calls::AARCH64, ALL, X32_BIT | 203, [0; 6]);
        assert_eq!(high, ALLOW, "no x32 numbers on aarch64, and no such call");
    }

    #[test]
    fn each_guard_traps_its_calls_and_leaves_every_other_call_alone() {
        for calls in [Calls::X86_64, Calls::AARCH64] {
            for guards in every_guard_set() {
                let on = |held: bool, action| if held { action } else { ALLOW };
                for (call, _) in calls.numbers {
                    let names = guards.create || guards.held;
                    let wanted = match call {
                        Call::Connect => on(guards.connect, NOTIFY),
                        Call::Openat2 => on(names, ERRNO | ENOSYS),
                        Call::OpenByHandleAt => on(guards.held, ERRNO | EPERM),
                        call if CREATING.contains(call) => on(names, NOTIFY),
                        call if REMOVING.contains(call) => on(guards.held, NOTIFY),
                        call if SIGNALLING.contains(call) => on(guards.signal, NOTIFY),
                        _ => ALLOW,
                    };
                    assert_eq!(plain(&calls, guards, *call), wanted, "{call:?} {guards:?}");
                }
                for nr in [0, 1, 3, 41, 59, 221, 435, 500] {
                    if calls.call(nr).is_none() {
                        assert_eq!(told(&calls, guards, nr, [0; 6]), ALLOW, "{nr} {guards:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_open_is_trapped_only_when_its_flags_register_asks_for_a_new_file() {
        let x86 = &Calls::X86_64;
        let create = u64::from(O_CREAT);
        let open = |args| told(x86, ALL, 2, args);
        assert_eq!(open([0, create | 1, 0o644, 0, 0, 0]), NOTIFY);
        assert_eq!(
            open([0, 0o2000 | 1, 0, 0, 0, 0]),
            ALLOW,
            "append, no new file"
        );
        assert_eq!(
            open([create, 0, create, 0, 0, 0]),
            ALLOW,
            "not the flags register"
        );
        assert_eq!(
            open([0, create << 32, 0, 0, 0, 0]),
            ALLOW,
            "the high word is not read"
        );
        assert_eq!(
            open([0, 0xdead_0000_0000_0000 | create, 0, 0, 0, 0]),
            NOTIFY
        );
        for calls in [Calls::X86_64, Calls::AARCH64] {
            let nr = calls.number(Call::Openat).unwrap();
            let openat = |args| told(&calls, ALL, nr, args);
            assert_eq!(openat([0, 0, create | 2, 0o600, 0, 0]), NOTIFY);
            assert_eq!(
                openat([0, create, 0, 0, 0, 0]),
                ALLOW,
                "not the flags register"
            );
            assert_eq!(
                openat([0, 0, 0o20200000 | 2, 0o600, 0, 0]),
                ALLOW,
                "no name yet"
            );
            let unguarded = Guards {
                create: false,
                ..ALL
            };
            assert_eq!(told(&calls, unguarded, nr, [0, 0, create, 0, 0, 0]), ALLOW);
        }
    }

    #[test]
    fn a_held_guard_traps_every_open_that_may_write_and_lets_a_read_run() {
        let writes = [0o1, 0o2, 0o1000, u64::from(O_CREAT), 0o2000 | 0o1];
        let reads = [0, 0o2000, 0o200000, 0o4000, 0o10000000];
        for calls in [Calls::X86_64, Calls::AARCH64] {
            let nr = calls.number(Call::Openat).unwrap();
            let openat = |guards, flags| told(&calls, guards, nr, [0, 0, flags, 0, 0, 0]);
            for flags in writes {
                assert_eq!(openat(HELD, flags), NOTIFY, "{flags:#o}");
                assert_eq!(
                    openat(
                        Guards {
                            held: false,
                            ..HELD
                        },
                        flags
                    ),
                    on_create(flags),
                    "{flags:#o}"
                );
            }
            for flags in reads {
                assert_eq!(openat(HELD, flags), ALLOW, "{flags:#o}");
            }
            let handle = calls.number(Call::OpenByHandleAt).unwrap();
            assert_eq!(told(&calls, HELD, handle, [0; 6]), ERRNO | EPERM);
            assert_eq!(told(&calls, ALL, handle, [0; 6]), ALLOW);
            for call in REMOVING {
                if let Some(nr) = calls.number(call) {
                    assert_eq!(told(&calls, HELD, nr, [0; 6]), NOTIFY, "{call:?}");
                    assert_eq!(told(&calls, ALL, nr, [0; 6]), ALLOW, "{call:?}");
                }
            }
        }
        let open = |guards, flags| told(&Calls::X86_64, guards, 2, [0, flags, 0, 0, 0, 0]);
        assert_eq!(
            open(HELD, 0o1),
            NOTIFY,
            "open's flags are its second register"
        );
        assert_eq!(open(HELD, 0), ALLOW);
        let held_only = Guards {
            held: true,
            ..Guards::default()
        };
        for call in CREATING {
            if let Some(nr) = Calls::X86_64.number(call) {
                assert_eq!(
                    told(&Calls::X86_64, held_only, nr, [0; 6]),
                    NOTIFY,
                    "{call:?}"
                );
            }
        }
    }

    fn on_create(flags: u64) -> u32 {
        if flags & u64::from(O_CREAT) != 0 {
            NOTIFY
        } else {
            ALLOW
        }
    }

    #[test]
    fn the_held_calls_carry_the_numbers_the_kernel_gives_them() {
        let pinned = [
            (Call::Unlink, Some(87), None),
            (Call::Unlinkat, Some(263), Some(35)),
            (Call::Rmdir, Some(84), None),
            (Call::Truncate, Some(76), Some(45)),
            (Call::OpenByHandleAt, Some(304), Some(265)),
        ];
        for (call, x86, arm) in pinned {
            assert_eq!(Calls::X86_64.number(call), x86, "{call:?}");
            assert_eq!(Calls::AARCH64.number(call), arm, "{call:?}");
        }
    }

    #[test]
    fn a_send_that_would_open_a_connection_is_refused_and_a_plain_send_runs() {
        let fast = u64::from(MSG_FASTOPEN);
        for calls in [Calls::X86_64, Calls::AARCH64] {
            for (call, flags) in [(Call::Sendto, 3), (Call::Sendmsg, 2), (Call::Sendmmsg, 3)] {
                let nr = calls.number(call).unwrap();
                let mut args = [0u64; 6];
                args[flags] = fast | 0x4000;
                assert_eq!(told(&calls, ALL, nr, args), ERRNO | EOPNOTSUPP, "{call:?}");
                args[flags] = 0x4000;
                assert_eq!(told(&calls, ALL, nr, args), ALLOW, "{call:?}");
                let mut elsewhere = [fast; 6];
                elsewhere[flags] = 0;
                assert_eq!(told(&calls, ALL, nr, elsewhere), ALLOW, "{call:?}");
                let unguarded = Guards {
                    connect: false,
                    ..ALL
                };
                args[flags] = fast;
                assert_eq!(told(&calls, unguarded, nr, args), ALLOW, "{call:?}");
            }
        }
    }

    #[test]
    fn naming_who_gets_a_descriptors_signals_is_trapped_and_other_commands_run() {
        for calls in [Calls::X86_64, Calls::AARCH64] {
            let fcntl = calls.number(Call::Fcntl).unwrap();
            let ioctl = calls.number(Call::Ioctl).unwrap();
            let trapped = [(fcntl, 8), (fcntl, 15), (ioctl, 0x8901), (ioctl, 0x8902)];
            for (nr, command) in trapped {
                let args = [3, command, 4242, 0, 0, 0];
                assert_eq!(told(&calls, ALL, nr, args), NOTIFY, "{nr} {command:#x}");
                let unguarded = Guards {
                    signal: false,
                    ..ALL
                };
                assert_eq!(
                    told(&calls, unguarded, nr, args),
                    ALLOW,
                    "{nr} {command:#x}"
                );
            }
            let free = [
                (fcntl, 0),
                (fcntl, 4),
                (fcntl, 9),
                (ioctl, 0x5401),
                (ioctl, 0x8903),
            ];
            for (nr, command) in free {
                let args = [3, command, 8, 15, 0x8901, 0x8902];
                assert_eq!(told(&calls, ALL, nr, args), ALLOW, "{nr} {command:#x}");
            }
        }
    }

    #[test]
    fn aarch64_has_only_the_calls_that_take_a_folder_descriptor() {
        let arm = &Calls::AARCH64;
        let absent = [
            Call::Open,
            Call::Creat,
            Call::Mkdir,
            Call::Mknod,
            Call::Symlink,
            Call::Link,
            Call::Rename,
            Call::Unlink,
            Call::Rmdir,
        ];
        for call in absent {
            assert_eq!(arm.number(call), None, "{call:?}");
            assert!(Calls::X86_64.number(call).is_some(), "{call:?}");
        }
        assert_eq!(
            arm.numbers.len() + absent.len(),
            Calls::X86_64.numbers.len()
        );
        assert_eq!(told(arm, ALL, 38, [0; 6]), NOTIFY, "renameat");
        assert_eq!(told(arm, ALL, 276, [0; 6]), NOTIFY, "renameat2");
    }

    #[test]
    fn each_table_names_every_call_once_and_finds_it_back() {
        for calls in [Calls::X86_64, Calls::AARCH64] {
            for (at, (call, number)) in calls.numbers.iter().enumerate() {
                assert_eq!(calls.call(*number), Some(*call), "{call:?}");
                assert_eq!(calls.number(*call), Some(*number), "{call:?}");
                for (other, own) in &calls.numbers[at + 1..] {
                    assert!(other != call && own != number, "{call:?} and {other:?}");
                }
                assert!(*number < X32_BIT, "{call:?}");
            }
            assert_eq!(calls.call(0), None, "read is never trapped");
        }
    }

    #[test]
    fn every_jump_lands_inside_the_program_and_the_last_word_lets_a_call_run() {
        for calls in [Calls::X86_64, Calls::AARCH64] {
            for guards in every_guard_set() {
                let program = program(&calls, guards);
                assert!(program.len() <= PROGRAM_MAX);
                let last = program[program.len() - 1];
                assert_eq!((last.code, last.k), (0x06, ALLOW), "{guards:?}");
                for (at, insn) in program.iter().enumerate() {
                    let far = usize::from(insn.jt.max(insn.jf));
                    assert!(at + 1 + far < program.len() || insn.code == 0x06, "{at}");
                }
            }
            let bare = program(&calls, Guards::default());
            assert_eq!(
                bare.len(),
                if calls.x32 { 7 } else { 5 },
                "no rule without a guard"
            );
        }
    }
}
