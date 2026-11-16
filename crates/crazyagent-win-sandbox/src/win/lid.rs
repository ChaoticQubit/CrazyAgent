use std::fmt;

/// The last lid close action value Windows names: 0 Do nothing, 1 Sleep, 2 Hibernate,
/// 3 Shut down.
pub const ACTION_MAX: u32 = 3;

/// One Windows power plan, by its GUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scheme(u128);

impl Scheme {
    /// Reads a plan from its GUID text, 32 hex digits in groups of 8, 4, 4, 4 and 12
    /// split by dashes, in either case. Input: the text. Output: the plan, or none for
    /// any other text.
    pub fn parse(_text: &str) -> Option<Self> {
        None
    }

    /// The plan's GUID text, lower case, in groups of 8, 4, 4, 4 and 12 hex digits.
    /// Output: the 36 characters.
    pub fn text(&self) -> String {
        String::new()
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

/// Tells whether this account may write the lid close action on AC power and on battery
/// now, through the public access check, which asks for no rights. Output: Ok, `Policy`
/// when Group Policy sets it, `Denied` when only an administrator may change it.
pub fn writable() -> Result<(), LidError> {
    Err(LidError::Windows {
        step: "check the lid setting",
        code: 0,
    })
}

/// The active power plan. Output: the plan, or the Windows error.
pub fn active() -> Result<Scheme, LidError> {
    Ok(Scheme(0))
}

/// Reads one plan's lid close action on AC power and on battery. Input: the plan.
/// Output: both values, `Gone` for a plan that does not exist, or the Windows error; a
/// value Windows does not name is an error too.
pub fn read(_scheme: Scheme) -> Result<Actions, LidError> {
    Ok(Actions { ac: 0, dc: 0 })
}

/// Writes one plan's lid close action on AC power and on battery, and applies it when
/// that plan is the active one, so the active plan never changes. Inputs: the plan and
/// both values, each at most `ACTION_MAX`. Output: Ok, or the first error.
pub fn write(_scheme: Scheme, _actions: Actions) -> Result<(), LidError> {
    Ok(())
}

/// Copies a power plan into a new plan, for rows that must never change the active one.
/// Input: the plan to copy. Output: the new plan, or the Windows error.
pub fn duplicate(scheme: Scheme) -> Result<Scheme, LidError> {
    Ok(scheme)
}

/// Deletes a power plan a row made; the active plan is never deleted. Input: the plan.
/// Output: Ok, or the error.
pub fn delete(_scheme: Scheme) -> Result<(), LidError> {
    Ok(())
}

/// Whether the lid is closed, as Windows last said through its lid switch notice, which
/// this process starts listening to on the first call. Output: none until Windows said,
/// or when the notice cannot be listened to.
pub fn closed() -> Option<bool> {
    None
}

/// Puts this computer to sleep, or hibernates it, with wake events left on. The shutdown
/// privilege is turned on in this process's token for the call and off again after it.
/// Input: whether to hibernate. Output: Ok once the computer woke, or why it did not
/// sleep.
pub fn suspend(_hibernate: bool) -> Result<(), LidError> {
    Err(LidError::Windows {
        step: "sleep",
        code: 0,
    })
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
