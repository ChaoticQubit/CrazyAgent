use crate::filter::{Insn, PROGRAM_MAX};
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

const _: () = {
    assert!(size_of::<libc::seccomp_notif>() == 80);
    assert!(size_of::<libc::seccomp_notif_resp>() == 24);
    assert!(size_of::<libc::seccomp_notif_addfd>() == 24);
    assert!(size_of::<libc::seccomp_data>() == 64);
    assert!(size_of::<libc::sock_filter>() == 8);
    assert!(crate::filter::O_CREAT == libc::O_CREAT as u32);
    assert!(crate::filter::MSG_FASTOPEN == libc::MSG_FASTOPEN as u32);
    assert!(crate::filter::F_SETOWN == libc::F_SETOWN as u32);
    assert!(crate::filter::NOTIFY == libc::SECCOMP_RET_USER_NOTIF);
    assert!(crate::filter::ALLOW == libc::SECCOMP_RET_ALLOW);
    assert!(crate::filter::KILL_PROCESS == libc::SECCOMP_RET_KILL_PROCESS);
    assert!(crate::filter::ERRNO == libc::SECCOMP_RET_ERRNO);
};

/// A seccomp program in the kernel's own layout, made before the harness is forked so the
/// child allocates nothing. Input: the instructions from [`crate::filter::program`].
pub struct Program(Vec<libc::sock_filter>);

impl Program {
    /// Copies the instructions into the kernel's layout. Input: at most [`PROGRAM_MAX`]
    /// instructions, at least one. Output: the program.
    pub fn new(program: &[Insn]) -> Self {
        assert!(!program.is_empty() && program.len() <= PROGRAM_MAX);
        let kernel = |insn: &Insn| libc::sock_filter {
            code: insn.code,
            jt: insn.jt,
            jf: insn.jf,
            k: insn.k,
        };
        Self(program.iter().map(kernel).collect())
    }
}

/// One trapped call as the kernel hands it to the supervisor: the id to answer with, the
/// calling thread, the call's number and architecture, and its six register arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    pub id: u64,
    pub tid: i32,
    pub nr: i32,
    pub arch: u32,
    pub args: [u64; 6],
}

/// Turns a system call's return into a result. Input: the return value. Output: the
/// value, or the error the kernel left for this thread.
fn checked(told: libc::c_long) -> io::Result<libc::c_long> {
    if told < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(told)
}

/// Tells whether this kernel's user-notification structures have the sizes this crate was
/// built with. Output: Ok when they match, else the kernel's error or `Unsupported`.
pub fn sizes_fit() -> io::Result<()> {
    let mut sizes = libc::seccomp_notif_sizes {
        seccomp_notif: 0,
        seccomp_notif_resp: 0,
        seccomp_data: 0,
    };
    let asked = libc::SECCOMP_GET_NOTIF_SIZES as libc::c_ulong;
    let none: libc::c_ulong = 0;
    let told = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            asked,
            none,
            &mut sizes as *mut libc::seccomp_notif_sizes,
        )
    };
    checked(told)?;
    let fits = usize::from(sizes.seccomp_notif) == size_of::<libc::seccomp_notif>()
        && usize::from(sizes.seccomp_notif_resp) == size_of::<libc::seccomp_notif_resp>()
        && usize::from(sizes.seccomp_data) == size_of::<libc::seccomp_data>();
    fits.then_some(())
        .ok_or_else(|| io::ErrorKind::Unsupported.into())
}

/// Installs the program on the calling thread with a listener for its trapped calls, and
/// has a call the supervisor already took finish before a signal handler runs. It makes
/// only system calls, so a forked child may call it. Input: the program. Output: the
/// listener, closed by the kernel when the thread starts another program.
fn install(program: &Program) -> io::Result<OwnedFd> {
    let filter = libc::sock_fprog {
        len: program.0.len() as libc::c_ushort,
        filter: program.0.as_ptr().cast_mut(),
    };
    let mode = libc::SECCOMP_SET_MODE_FILTER as libc::c_ulong;
    let flags: libc::c_ulong =
        libc::SECCOMP_FILTER_FLAG_NEW_LISTENER | libc::SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV;
    let told = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            mode,
            flags,
            &filter as *const libc::sock_fprog,
        )
    };
    let listener = checked(told)?;
    Ok(unsafe { OwnedFd::from_raw_fd(listener as RawFd) })
}

/// Sends a listener to the supervisor over the handover socket, without allocating or
/// panicking, so a forked child may call it. Inputs: the child's end of the socket and the
/// listener. Output: Ok once it is sent.
fn send_listener(socket: BorrowedFd<'_>, listener: BorrowedFd<'_>) -> io::Result<()> {
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    let carried = [listener];
    if !control.push(SendAncillaryMessage::ScmRights(&carried)) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let sent = rustix::net::sendmsg(
        socket,
        &[IoSlice::new(&[0])],
        &mut control,
        SendFlags::NOSIGNAL,
    )?;
    if sent != 1 {
        return Err(io::ErrorKind::WriteZero.into());
    }
    Ok(())
}

/// Takes the listener the harness sent before it started its program. Input: the
/// supervisor's end of the handover socket. Output: the listener, closed on exec here.
pub fn take_listener(socket: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let mut byte = [0u8; 1];
    let got = rustix::net::recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut byte)],
        &mut control,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    let listener = control.drain().find_map(|message| match message {
        RecvAncillaryMessage::ScmRights(mut carried) => carried.next(),
        _ => None,
    });
    let listener = listener.ok_or(io::ErrorKind::UnexpectedEof)?;
    assert_eq!(got.bytes, 1);
    Ok(listener)
}

/// Holds off, on the calling thread and every thread it starts later, the signals a launch
/// is stopped with, so they end the harness and the supervisor then ends with it. Output:
/// the signals the thread held off before.
fn block_stops() -> io::Result<libc::sigset_t> {
    let mut stops = MaybeUninit::<libc::sigset_t>::uninit();
    let mut before = MaybeUninit::<libc::sigset_t>::uninit();
    let told = unsafe {
        libc::sigemptyset(stops.as_mut_ptr());
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
            libc::sigaddset(stops.as_mut_ptr(), signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, stops.as_ptr(), before.as_mut_ptr())
    };
    if told != 0 {
        return Err(io::Error::from_raw_os_error(told));
    }
    Ok(unsafe { before.assume_init() })
}

/// Gives the calling thread back the signals it held off before [`block_stops`]. It makes
/// one system call, so a forked child may call it. Input: those signals.
fn restore(before: &libc::sigset_t) -> io::Result<()> {
    let told = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, before, std::ptr::null_mut()) };
    if told != 0 {
        return Err(io::Error::from_raw_os_error(told));
    }
    Ok(())
}

/// Starts a harness that dies with this thread, takes the stop signals as the thread took
/// them before, traps the program's calls from before it starts its own program, and sends
/// the listener over the handover socket; this thread holds the stop signals off from then
/// on. Inputs: the command, the program and the child's end of the socket. Output: the
/// started child.
pub fn spawn_trapped(
    command: &mut Command,
    program: Program,
    socket: OwnedFd,
) -> io::Result<Child> {
    let parent = rustix::process::getpid();
    let before = block_stops()?;
    let trap = move || {
        rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))?;
        if rustix::process::getppid() != Some(parent) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        restore(&before)?;
        let listener = install(&program)?;
        send_listener(socket.as_fd(), listener.as_fd())
    };
    unsafe { command.pre_exec(trap) };
    command.spawn()
}

/// Asks the listener for one of its calls: the next trapped one, an answer, a check or a
/// descriptor to add. Inputs: the listener, the request and what it reads or fills.
/// Output: the kernel's return value, or its error.
fn ask<T>(listener: BorrowedFd<'_>, request: libc::Ioctl, with: &mut T) -> io::Result<i32> {
    let told = unsafe { libc::ioctl(listener.as_raw_fd(), request, with as *mut T) };
    checked(libc::c_long::from(told)).map(|told| told as i32)
}

/// Waits for the next trapped call. Input: the listener. Output: the call, or the
/// kernel's error; `ENOENT` when the caller was killed before the call was taken.
pub fn receive(listener: BorrowedFd<'_>) -> io::Result<Notice> {
    let mut notif = libc::seccomp_notif {
        id: 0,
        pid: 0,
        flags: 0,
        data: libc::seccomp_data {
            nr: 0,
            arch: 0,
            instruction_pointer: 0,
            args: [0; 6],
        },
    };
    ask(listener, libc::SECCOMP_IOCTL_NOTIF_RECV, &mut notif)?;
    Ok(Notice {
        id: notif.id,
        tid: notif.pid as i32,
        nr: notif.data.nr,
        arch: notif.data.arch,
        args: notif.data.args,
    })
}

/// Tells whether a trapped call still waits for its answer, so what was read from the
/// caller's memory belongs to that caller. Inputs: the listener and the call's id.
pub fn still_valid(listener: BorrowedFd<'_>, id: u64) -> bool {
    let mut id = id;
    ask(listener, libc::SECCOMP_IOCTL_NOTIF_ID_VALID, &mut id).is_ok()
}

/// Answers a trapped call with a return value or an error number; the kernel does not run
/// the call. Inputs: the listener, the call's id and the result. Output: Ok once the
/// caller has its answer; `ENOENT` when it is gone.
pub fn respond(listener: BorrowedFd<'_>, id: u64, result: Result<i64, i32>) -> io::Result<()> {
    let (val, error) = match result {
        Ok(val) => (val, 0),
        Err(errno) => (0, -errno.abs()),
    };
    let mut resp = libc::seccomp_notif_resp {
        id,
        val,
        error,
        flags: 0,
    };
    ask(listener, libc::SECCOMP_IOCTL_NOTIF_SEND, &mut resp).map(|_| ())
}

/// Lets the kernel run a trapped call as the caller made it. Only for a call the
/// supervisor judged by its register arguments alone. Inputs: the listener and the id.
pub fn respond_continue(listener: BorrowedFd<'_>, id: u64) -> io::Result<()> {
    let mut resp = libc::seccomp_notif_resp {
        id,
        val: 0,
        error: 0,
        flags: libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32,
    };
    ask(listener, libc::SECCOMP_IOCTL_NOTIF_SEND, &mut resp).map(|_| ())
}

/// Answers a trapped open with a file the supervisor opened: the caller gets a descriptor
/// of its own for it as the call's return value, in one step. Inputs: the listener, the
/// call's id, the file and whether the caller asked for close-on-exec.
pub fn hand_descriptor(
    listener: BorrowedFd<'_>,
    id: u64,
    file: BorrowedFd<'_>,
    cloexec: bool,
) -> io::Result<()> {
    let mut addfd = libc::seccomp_notif_addfd {
        id,
        flags: libc::SECCOMP_ADDFD_FLAG_SEND as u32,
        srcfd: file.as_raw_fd() as u32,
        newfd: 0,
        newfd_flags: if cloexec { libc::O_CLOEXEC as u32 } else { 0 },
    };
    let placed = ask(listener, libc::SECCOMP_IOCTL_NOTIF_ADDFD, &mut addfd)?;
    assert!(placed >= 0);
    Ok(())
}

/// Names who gets a descriptor's signals (`F_SETOWN`): a process when positive, a process
/// group when negative, no one when zero. Inputs: the descriptor and the owner.
pub fn set_owner(file: BorrowedFd<'_>, owner: i32) -> io::Result<()> {
    let told = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETOWN, owner) };
    checked(libc::c_long::from(told)).map(|_| ())
}
