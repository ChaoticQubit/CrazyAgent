use std::ffi::c_void;
use std::fmt;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ACCESS_DISABLED_BY_POLICY, ERROR_FILE_NOT_FOUND,
    ERROR_NOT_ALL_ASSIGNED, ERROR_SUCCESS, GetLastError, HANDLE, LUID, LocalFree,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
};
use windows_sys::Win32::System::Power::{
    ACCESS_AC_POWER_SETTING_INDEX, ACCESS_DC_POWER_SETTING_INDEX,
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, POWERBROADCAST_SETTING, PowerDeleteScheme,
    PowerDuplicateScheme, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex,
    PowerSetActiveScheme, PowerSettingAccessCheckEx, PowerSettingRegisterNotification,
    PowerWriteACValueIndex, PowerWriteDCValueIndex, SetSuspendState,
};
use windows_sys::Win32::System::Registry::KEY_WRITE;
use windows_sys::Win32::System::SystemServices::{
    GUID_LIDCLOSE_ACTION, GUID_LIDSWITCH_STATE_CHANGE, GUID_SYSTEM_BUTTON_SUBGROUP,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_POWERSETTINGCHANGE};
use windows_sys::core::GUID;

/// The last lid close action value Windows names: 0 Do nothing, 1 Sleep, 2 Hibernate,
/// 3 Shut down.
pub const ACTION_MAX: u32 = 3;

const TEXT_LEN: usize = 36;
const DIGITS: usize = 32;
const DASHES: [usize; 4] = [8, 13, 18, 23];
const LID_UNKNOWN: u8 = u8::MAX;
const LID_CLOSED: u8 = 0;
const LID_OPEN: u8 = 1;
const LID_DATA_BYTES: u32 = 4;

const _: () = assert!(DIGITS + DASHES.len() == TEXT_LEN);
const _: () = assert!(LID_DATA_BYTES as usize == size_of::<u32>());
const _: () = assert!(ACCESS_AC_POWER_SETTING_INDEX != ACCESS_DC_POWER_SETTING_INDEX);

static LID: AtomicU8 = AtomicU8::new(LID_UNKNOWN);
static LISTENING: OnceLock<bool> = OnceLock::new();

/// One Windows power plan, by its GUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheme(u128);

impl Scheme {
    /// Reads a plan from its GUID text, 32 hex digits in groups of 8, 4, 4, 4 and 12
    /// split by dashes, in either case. Input: the text. Output: the plan, or none for
    /// any other text.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != TEXT_LEN {
            return None;
        }
        let mut value: u128 = 0;
        let mut digits = 0;
        for (at, byte) in bytes.iter().enumerate() {
            if DASHES.contains(&at) {
                if *byte != b'-' {
                    return None;
                }
                continue;
            }
            let digit = char::from(*byte).to_digit(16)?;
            value = (value << 4) | u128::from(digit);
            digits += 1;
        }
        assert_eq!(digits, DIGITS);
        assert_eq!(
            bytes.iter().filter(|byte| **byte == b'-').count(),
            DASHES.len()
        );
        Some(Self(value))
    }

    /// The plan's GUID text, lower case, in groups of 8, 4, 4, 4 and 12 hex digits.
    /// Output: the 36 characters.
    pub fn text(&self) -> String {
        let value = self.0;
        let text = format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            value >> 96,
            (value >> 80) & 0xffff,
            (value >> 64) & 0xffff,
            (value >> 48) & 0xffff,
            value & 0xffff_ffff_ffff
        );
        assert_eq!(text.len(), TEXT_LEN);
        assert!(DASHES.iter().all(|at| text.as_bytes()[*at] == b'-'));
        text
    }

    fn guid(self) -> GUID {
        GUID::from_u128(self.0)
    }

    /// The plan a Windows GUID names, the inverse of `guid`. Input: the GUID. Output: the
    /// plan.
    fn of(guid: &GUID) -> Self {
        let value = (u128::from(guid.data1) << 96)
            | (u128::from(guid.data2) << 80)
            | (u128::from(guid.data3) << 64)
            | u128::from(u64::from_be_bytes(guid.data4));
        let scheme = Self(value);
        assert_eq!(scheme.guid().data1, guid.data1);
        assert_eq!(scheme.guid().data4, guid.data4);
        scheme
    }
}

/// The lid close action of one power plan on AC power and on battery, each 0 to
/// `ACTION_MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actions {
    pub ac: u32,
    pub dc: u32,
}

/// Why a lid setting call did not work: Group Policy sets the lid action, this account
/// may not change it, the power plan does not exist, or another Windows error at a
/// named step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LidError {
    Policy,
    Denied,
    Gone,
    Windows { step: &'static str, code: u32 },
}

impl fmt::Display for LidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Policy => write!(f, "Group Policy sets the lid action"),
            Self::Denied => write!(f, "this account may not change the lid action"),
            Self::Gone => write!(f, "the power plan does not exist"),
            Self::Windows { step, code } => write!(f, "could not {step} (Windows error {code})"),
        }
    }
}

/// The outcome of one power setting call. Inputs: the step it was and the code Windows
/// gave. Output: Ok on success, `Policy`, `Denied` and `Gone` for their codes, else the
/// error at that step.
fn checked(step: &'static str, code: u32) -> Result<(), LidError> {
    match code {
        ERROR_SUCCESS => Ok(()),
        ERROR_ACCESS_DISABLED_BY_POLICY => Err(LidError::Policy),
        ERROR_ACCESS_DENIED => Err(LidError::Denied),
        ERROR_FILE_NOT_FOUND => Err(LidError::Gone),
        code => Err(LidError::Windows { step, code }),
    }
}

/// Tells whether this account may write the lid close action on AC power and on battery
/// now, through the public access check, which asks for no rights. Output: Ok, `Policy`
/// when Group Policy sets it, `Denied` when only an administrator may change it. Relies
/// on the setting GUID being a constant that outlives the call.
pub fn writable() -> Result<(), LidError> {
    for accessor in [ACCESS_AC_POWER_SETTING_INDEX, ACCESS_DC_POWER_SETTING_INDEX] {
        let code = unsafe { PowerSettingAccessCheckEx(accessor, &GUID_LIDCLOSE_ACTION, KEY_WRITE) };
        checked("check the lid setting", code)?;
    }
    Ok(())
}

/// Copies one plan GUID Windows allocated for this caller, then frees it. Inputs: the
/// pointer and the step. Output: the plan; a null pointer is an error. Relies on the
/// pointer being null or a block from `LocalAlloc` holding one GUID this caller owns.
fn taken(found: *mut GUID, step: &'static str) -> Result<Scheme, LidError> {
    if found.is_null() {
        return Err(LidError::Windows { step, code: 0 });
    }
    let scheme = Scheme::of(unsafe { &*found });
    let freed = unsafe { LocalFree(found.cast()) };
    assert!(
        freed.is_null(),
        "LocalFree gives null back once the block is freed"
    );
    Ok(scheme)
}

/// The active power plan. Output: the plan, or the Windows error. Relies on Windows
/// writing null or a block it allocated for this caller.
pub fn active() -> Result<Scheme, LidError> {
    let mut found: *mut GUID = ptr::null_mut();
    let code = unsafe { PowerGetActiveScheme(ptr::null_mut(), &raw mut found) };
    checked("read the active power plan", code)?;
    taken(found, "read the active power plan")
}

/// Reads one plan's lid close action on AC power and on battery. Input: the plan.
/// Output: both values, `Gone` for a plan that does not exist, or the Windows error; a
/// value Windows does not name is an error too. Relies on every GUID and out value
/// living on this stack for the calls.
pub fn read(scheme: Scheme) -> Result<Actions, LidError> {
    let plan = scheme.guid();
    let (mut ac, mut dc) = (u32::MAX, u32::MAX);
    let subgroup = &GUID_SYSTEM_BUTTON_SUBGROUP;
    let setting = &GUID_LIDCLOSE_ACTION;
    let code = unsafe { PowerReadACValueIndex(ptr::null_mut(), &plan, subgroup, setting, &mut ac) };
    checked("read the lid action on AC power", code)?;
    let code = unsafe { PowerReadDCValueIndex(ptr::null_mut(), &plan, subgroup, setting, &mut dc) };
    checked("read the lid action on battery", code)?;
    if ac > ACTION_MAX || dc > ACTION_MAX {
        let step = "read the lid action, which has a value Windows does not name";
        let code = ac.max(dc);
        return Err(LidError::Windows { step, code });
    }
    assert!(ac <= ACTION_MAX);
    assert!(dc <= ACTION_MAX);
    Ok(Actions { ac, dc })
}

/// Writes one plan's lid close action on AC power and on battery, and applies it when
/// that plan is the active one, so the active plan never changes. Inputs: the plan and
/// both values, each at most `ACTION_MAX`. Output: Ok, or the first error. Relies on
/// every GUID living on this stack for the calls.
pub fn write(scheme: Scheme, actions: Actions) -> Result<(), LidError> {
    assert!(actions.ac <= ACTION_MAX);
    assert!(actions.dc <= ACTION_MAX);
    let plan = scheme.guid();
    let subgroup = &GUID_SYSTEM_BUTTON_SUBGROUP;
    let setting = &GUID_LIDCLOSE_ACTION;
    let ac = actions.ac;
    let code = unsafe { PowerWriteACValueIndex(ptr::null_mut(), &plan, subgroup, setting, ac) };
    checked("write the lid action on AC power", code)?;
    let dc = actions.dc;
    let code = unsafe { PowerWriteDCValueIndex(ptr::null_mut(), &plan, subgroup, setting, dc) };
    checked("write the lid action on battery", code)?;
    if active()? != scheme {
        return Ok(());
    }
    let code = unsafe { PowerSetActiveScheme(ptr::null_mut(), &plan) };
    checked("apply the active power plan", code)
}

/// Copies a power plan into a new plan, for rows that must never change the active one.
/// Input: the plan to copy. Output: the new plan, or the Windows error. Relies on Windows
/// allocating the new GUID for this caller when the out pointer starts null.
pub fn duplicate(scheme: Scheme) -> Result<Scheme, LidError> {
    let plan = scheme.guid();
    let mut made: *mut GUID = ptr::null_mut();
    let code = unsafe { PowerDuplicateScheme(ptr::null_mut(), &plan, &raw mut made) };
    checked("copy the power plan", code)?;
    let copy = taken(made, "copy the power plan")?;
    assert_ne!(copy, scheme, "a copy is a new plan");
    Ok(copy)
}

/// Deletes a power plan a row made; the active plan is never deleted. Input: the plan.
/// Output: Ok, or the error. Relies on the GUID living on this stack for the call.
pub fn delete(scheme: Scheme) -> Result<(), LidError> {
    if active()? == scheme {
        let step = "delete the active power plan";
        let code = ERROR_ACCESS_DENIED;
        return Err(LidError::Windows { step, code });
    }
    let plan = scheme.guid();
    let code = unsafe { PowerDeleteScheme(ptr::null_mut(), &plan) };
    checked("delete the power plan", code)
}

/// Whether the lid is closed, as Windows last said through its lid switch notice, which
/// this process starts listening to on the first call. Output: none until Windows said,
/// or when the notice cannot be listened to.
pub fn closed() -> Option<bool> {
    let listening = *LISTENING.get_or_init(listen);
    let lid = LID.load(Ordering::Acquire);
    assert!(matches!(lid, LID_UNKNOWN | LID_CLOSED | LID_OPEN));
    assert!(listening || lid == LID_UNKNOWN);
    match lid {
        LID_CLOSED => Some(true),
        LID_OPEN => Some(false),
        _ => None,
    }
}

/// Starts listening to Windows' lid switch notice for the rest of this process; Windows
/// sends the lid's position at once and on every change. Output: whether it listens.
/// Relies on the subscription outliving the registration, which a leaked box gives, and
/// on the callback storing only into a static.
fn listen() -> bool {
    let subscription = Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
        Callback: Some(lid_changed),
        Context: ptr::null_mut(),
    }));
    let recipient: HANDLE = ptr::from_mut(subscription).cast();
    let mut handle: *mut c_void = ptr::null_mut();
    let lid = &GUID_LIDSWITCH_STATE_CHANGE;
    let code = unsafe {
        PowerSettingRegisterNotification(lid, DEVICE_NOTIFY_CALLBACK, recipient, &raw mut handle)
    };
    if code != ERROR_SUCCESS {
        eprintln!("keep awake: the lid switch notice is not heard (Windows error {code})");
        return false;
    }
    assert!(!handle.is_null());
    assert!(!recipient.is_null());
    true
}

/// Windows' lid switch notice: stores whether the lid is open. It never panics, as it
/// runs on a Windows thread. Inputs: the context, the notice type and the setting.
/// Output: 0. Relies on the setting being null or a `POWERBROADCAST_SETTING` whose data
/// holds `DataLength` bytes.
unsafe extern "system" fn lid_changed(
    _context: *const c_void,
    kind: u32,
    setting: *const c_void,
) -> u32 {
    if kind != PBT_POWERSETTINGCHANGE || setting.is_null() {
        return 0;
    }
    let setting = setting.cast::<POWERBROADCAST_SETTING>();
    let length = unsafe { (&raw const (*setting).DataLength).read_unaligned() };
    if length < LID_DATA_BYTES {
        return 0;
    }
    let data = unsafe { &raw const (*setting).Data }.cast::<u32>();
    let open = unsafe { data.read_unaligned() };
    let lid = if open == 0 { LID_CLOSED } else { LID_OPEN };
    LID.store(lid, Ordering::Release);
    0
}

/// This process's token, opened to turn the shutdown privilege on and off, and closed
/// when dropped.
struct Token(HANDLE);

impl Token {
    /// Opens this process's token for privilege changes. Output: the token, or the
    /// Windows error. Relies on the out handle being written only on success.
    fn open() -> Result<Self, LidError> {
        let mut handle: HANDLE = ptr::null_mut();
        let process = unsafe { GetCurrentProcess() };
        let opened = unsafe { OpenProcessToken(process, TOKEN_ADJUST_PRIVILEGES, &raw mut handle) };
        if opened == 0 {
            let code = unsafe { GetLastError() };
            let step = "open this process's token";
            return Err(LidError::Windows { step, code });
        }
        assert!(!handle.is_null());
        assert!(!process.is_null());
        Ok(Self(handle))
    }

    /// Turns the shutdown privilege on or off in this token. Input: on or off. Output: Ok,
    /// or the Windows error; a token without the privilege is an error too. Relies on the
    /// token being open for privilege changes and the privilege name ending in a NUL.
    fn shutdown(&self, on: bool) -> Result<(), LidError> {
        let step = if on {
            "turn on the shutdown privilege"
        } else {
            "turn off the shutdown privilege"
        };
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        let found = unsafe { LookupPrivilegeValueW(ptr::null(), SE_SHUTDOWN_NAME, &raw mut luid) };
        if found == 0 {
            let code = unsafe { GetLastError() };
            return Err(LidError::Windows { step, code });
        }
        let wanted = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: if on { SE_PRIVILEGE_ENABLED } else { 0 },
            }],
        };
        let adjusted = unsafe {
            AdjustTokenPrivileges(self.0, 0, &wanted, 0, ptr::null_mut(), ptr::null_mut())
        };
        let code = unsafe { GetLastError() };
        if adjusted == 0 || code == ERROR_NOT_ALL_ASSIGNED {
            return Err(LidError::Windows { step, code });
        }
        assert_eq!(wanted.PrivilegeCount, 1);
        assert!(!self.0.is_null());
        Ok(())
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        if unsafe { CloseHandle(self.0) } == 0 {
            eprintln!("keep awake: this process's token was not closed");
        }
    }
}

/// Puts this computer to sleep, or hibernates it, with wake events left on. The shutdown
/// privilege is turned on in this process's token for the call and off again after it.
/// Input: whether to hibernate. Output: Ok once the computer woke, or why it did not
/// sleep.
pub fn suspend(hibernate: bool) -> Result<(), LidError> {
    let token = Token::open()?;
    token.shutdown(true)?;
    let slept = unsafe { SetSuspendState(hibernate, false, false) };
    let code = if slept {
        ERROR_SUCCESS
    } else {
        unsafe { GetLastError() }
    };
    let back = token.shutdown(false);
    if !slept {
        let step = "sleep";
        return Err(LidError::Windows { step, code });
    }
    back
}

#[cfg(test)]
mod tests {
    use super::{
        ACTION_MAX, Actions, LidError, Scheme, active, closed, delete, duplicate, read,
        writable, write,
    };

    #[test]
    fn a_plan_reads_back_from_the_text_it_gives() {
        let text = "381b4222-f694-41f0-9685-ff5bb260df2e";
        let scheme = Scheme::parse(text).expect("the Balanced plan's GUID");
        assert_eq!(scheme.text(), text);
        assert_eq!(Scheme::parse(&text.to_uppercase()), Some(scheme));
        let other = Scheme::parse("a1841308-3541-4fab-bc81-f71556f20b4a").unwrap();
        assert_ne!(other, scheme);
        let refused = [
            "",
            "381b4222f69441f09685ff5bb260df2e",
            "381b4222-f694-41f0-9685-ff5bb260df2",
            "381b4222-f694-41f0-9685-ff5bb260df2ee",
            "381b4222-f694-41f0-9685-ff5bb260df2g",
            "+81b4222-f694-41f0-9685-ff5bb260df2e",
            "381b4222-f694-41f0-9685_ff5bb260df2e",
            "{381b4222-f694-41f0-9685-ff5bb260df2}",
        ];
        for text in refused {
            assert_eq!(Scheme::parse(text), None, "{text}");
        }
    }

    /// Plan Task 14: a row changes a scratch copy of the active plan, never the active plan.
    #[test]
    fn a_scratch_plan_takes_both_lid_values_and_gives_them_back_and_the_active_plan_stays() {
        let owner = active().unwrap();
        let before = read(owner).unwrap();
        let scratch = duplicate(owner).unwrap();
        assert_ne!(scratch, owner, "the copy is a new plan");
        let wrote = (|| {
            write(scratch, Actions { ac: 2, dc: 1 })?;
            let first = read(scratch)?;
            write(scratch, Actions { ac: 0, dc: 0 })?;
            Ok::<_, LidError>((first, read(scratch)?))
        })();
        delete(scratch).unwrap();
        let (first, second) = wrote.unwrap();
        assert_eq!(first, Actions { ac: 2, dc: 1 });
        assert_eq!(second, Actions { ac: 0, dc: 0 });
        assert_eq!(active().unwrap(), owner, "the owner's plan stays active");
        assert_eq!(read(owner).unwrap(), before, "and keeps its lid values");
        assert!(before.ac <= ACTION_MAX);
        assert!(before.dc <= ACTION_MAX);
        assert_eq!(read(scratch), Err(LidError::Gone), "a deleted plan is gone");
        assert!(delete(owner).is_err(), "the active plan is never deleted");
    }

    #[test]
    fn the_lid_check_names_a_policy_or_missing_rights() {
        let checked = writable();
        assert!(
            matches!(checked, Ok(()) | Err(LidError::Policy | LidError::Denied)),
            "{checked:?}"
        );
        assert_ne!(closed(), Some(true), "rows run with the lid open");
    }
}
