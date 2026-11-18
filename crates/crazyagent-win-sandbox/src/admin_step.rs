use std::ffi::OsString;

/// The design version of the administrator step this build writes and reads
/// (V244-WIN-ADMIN-STEP): drive-root entries, the device grants and their boot task.
pub const VERSION: u32 = 1;

/// The one refusal for a machine record whose values are missing, of the wrong type, not
/// strict volume paths or canonical SIDs, repeated or too many.
pub const RECORD_MALFORMED: &str = "admin_record_malformed";

/// The refusal to count one more account in a record that already holds the most a
/// record keeps.
pub const RECORD_FULL: &str = "admin_record_full";

/// The one refusal for a command line that is not exactly one of the binary's roles.
pub const ARGUMENTS_REFUSED: &str = "admin_arguments_refused";

/// The most drives, and the most accounts, one record keeps: far more than one machine
/// has, few enough that a record is small.
const ITEMS_MAX: usize = 64;

const VERSION_VALUE: &str = "Version";
const DRIVES_VALUE: &str = "Drives";
const USERS_VALUE: &str = "Users";
const DEVICE_TASK_VALUE: &str = "DeviceTask";

/// One registry value as the record reads and writes it: a DWORD, or a REG_MULTI_SZ held as
/// its UTF-16 units, terminators included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Dword(u32),
    MultiSz(Vec<u16>),
}

/// What the step recorded under HKLM: its version, the volume GUID path of each drive root
/// it added the entry to, the canonical SID of each account that applied it, and whether
/// the device task is registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub version: u32,
    pub drives: Vec<String>,
    pub users: Vec<String>,
    pub device_task: bool,
}

impl Record {
    /// Reads the record through `value`, which returns each named value of the key or None
    /// when it is absent, refusing anything this build would not have written. Input: the
    /// reader. Output: the record, or [`RECORD_MALFORMED`].
    pub fn parse(value: impl Fn(&'static str) -> Option<Value>) -> Result<Record, &'static str> {
        let version = match value(VERSION_VALUE) {
            Some(Value::Dword(version)) if version >= 1 => version,
            _ => return Err(RECORD_MALFORMED),
        };
        let drives = listed(value(DRIVES_VALUE), volume_path)?;
        let users = listed(value(USERS_VALUE), canonical_sid)?;
        let device_task = match value(DEVICE_TASK_VALUE) {
            None => false,
            Some(Value::Dword(1)) => true,
            Some(_) => return Err(RECORD_MALFORMED),
        };
        let record = Record {
            version,
            drives,
            users,
            device_task,
        };
        record.assert_written_shape();
        Ok(record)
    }

    /// The named values this record is written as, an empty list or a false task left out.
    /// Output: the names and values in write order.
    pub fn values(&self) -> Vec<(&'static str, Value)> {
        self.assert_written_shape();
        let mut values = vec![(VERSION_VALUE, Value::Dword(self.version))];
        if !self.drives.is_empty() {
            values.push((DRIVES_VALUE, Value::MultiSz(multi_sz(&self.drives))));
        }
        if !self.users.is_empty() {
            values.push((USERS_VALUE, Value::MultiSz(multi_sz(&self.users))));
        }
        if self.device_task {
            values.push((DEVICE_TASK_VALUE, Value::Dword(1)));
        }
        values
    }

    /// Whether this record says the step is complete for `user`: this build's version or
    /// later, `user` counted and the device task registered. Input: a canonical SID.
    /// Output: true when it does.
    pub fn done_for(&self, user: &str) -> bool {
        assert!(canonical_sid(user));
        self.version >= VERSION && self.device_task && self.counts(user)
    }

    /// The record after `user` applied: counted once, and the version brought up to this
    /// build's. Input: a canonical SID. Output: the new record, or [`RECORD_FULL`].
    pub fn with_user(&self, user: &str) -> Result<Record, &'static str> {
        assert!(canonical_sid(user));
        let mut next = self.clone();
        next.version = self.version.max(VERSION);
        if !self.counts(user) {
            if self.users.len() >= ITEMS_MAX {
                return Err(RECORD_FULL);
            }
            next.users.push(user.to_owned());
        }
        assert_eq!(next.users.iter().filter(|known| *known == user).count(), 1);
        next.assert_written_shape();
        Ok(next)
    }

    /// The record with `user` no longer counted and everything else kept. Input: a
    /// canonical SID. Output: the new record.
    pub fn without_user(&self, user: &str) -> Record {
        assert!(canonical_sid(user));
        let mut next = self.clone();
        next.users.retain(|known| known != user);
        assert!(!next.counts(user));
        assert_eq!(
            next.users.len() + usize::from(self.counts(user)),
            self.users.len()
        );
        next
    }

    /// Whether `user` is one of the counted accounts. Input: a SID. Output: true when it is.
    fn counts(&self, user: &str) -> bool {
        self.users.iter().any(|known| known == user)
    }

    /// Asserts the shape [`Record::parse`] accepts and [`Record::values`] writes: a version,
    /// strict and distinct items, within [`ITEMS_MAX`].
    fn assert_written_shape(&self) {
        assert!(self.version >= 1);
        assert!(self.drives.len() <= ITEMS_MAX && self.users.len() <= ITEMS_MAX);
        assert!(self.drives.iter().all(|drive| volume_path(drive)));
        assert!(self.users.iter().all(|user| canonical_sid(user)));
        assert!(distinct(&self.drives) && distinct(&self.users));
    }
}

/// The items of an optional REG_MULTI_SZ value, each passing `strict`, none repeated and at
/// most [`ITEMS_MAX`]; an absent value is no items. Inputs: the value and the item check.
/// Output: the items, or [`RECORD_MALFORMED`].
fn listed(value: Option<Value>, strict: fn(&str) -> bool) -> Result<Vec<String>, &'static str> {
    let units = match value {
        None => return Ok(Vec::new()),
        Some(Value::MultiSz(units)) => units,
        Some(Value::Dword(_)) => return Err(RECORD_MALFORMED),
    };
    let items = parse_multi_sz(&units).ok_or(RECORD_MALFORMED)?;
    let fits = items.len() <= ITEMS_MAX && distinct(&items);
    if !fits || !items.iter().all(|item| strict(item)) {
        return Err(RECORD_MALFORMED);
    }
    Ok(items)
}

/// Whether no two of `items` are equal, ignoring ASCII case as Windows does for volume
/// GUIDs. Input: the items. Output: true when they are distinct.
fn distinct(items: &[String]) -> bool {
    items.iter().enumerate().all(|(at, item)| {
        !items[..at]
            .iter()
            .any(|earlier| earlier.eq_ignore_ascii_case(item))
    })
}

/// Whether `text` is a volume GUID path as Windows names a volume root:
/// `\\?\Volume{8-4-4-4-12 hex digits}\`. Input: the text. Output: true when it is.
pub fn volume_path(text: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let Some(guid) = text
        .strip_prefix(r"\\?\Volume{")
        .and_then(|rest| rest.strip_suffix(r"}\"))
    else {
        return false;
    };
    let groups: Vec<&str> = guid.split('-').collect();
    groups.len() == GROUPS.len()
        && groups.iter().zip(GROUPS).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// Whether `text` is a SID in the canonical form Windows writes: `S-1-`, a decimal
/// authority, and one to fifteen decimal sub-authorities, each a u32 with no leading zero.
/// Input: the text. Output: true when it is.
pub fn canonical_sid(text: &str) -> bool {
    const SUB_AUTHORITIES_MAX: usize = 15;
    let Some(rest) = text.strip_prefix("S-1-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    (2..=SUB_AUTHORITIES_MAX + 1).contains(&parts.len()) && parts.iter().all(|part| decimal(part))
}

/// Whether `part` is a u32 written as Windows writes one: decimal digits only, no sign and
/// no leading zero. Input: the text. Output: true when it is.
fn decimal(part: &str) -> bool {
    let digits = !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits && (part == "0" || !part.starts_with('0')) && part.parse::<u32>().is_ok()
}

/// The REG_MULTI_SZ units of `items`: each item and its terminator, then one more. Input:
/// non-empty items with no NUL. Output: the units.
pub fn multi_sz(items: &[String]) -> Vec<u16> {
    assert!(!items.is_empty());
    let mut units = Vec::new();
    for item in items {
        assert!(!item.is_empty() && !item.contains('\0'));
        units.extend(item.encode_utf16());
        units.push(0);
    }
    units.push(0);
    assert!(units.ends_with(&[0, 0]));
    units
}

/// The items of REG_MULTI_SZ `units`, only when they are exactly what [`multi_sz`] writes
/// for at least one item. Input: the units. Output: the items, or None.
pub fn parse_multi_sz(units: &[u16]) -> Option<Vec<String>> {
    let body = units.strip_suffix(&[0, 0])?;
    let items = body
        .split(|unit| *unit == 0)
        .map(String::from_utf16)
        .collect::<Result<Vec<String>, _>>()
        .ok()?;
    if items.iter().any(String::is_empty) {
        return None;
    }
    let written = multi_sz(&items);
    (written == units).then_some(items)
}

/// How `offer` asks: through the UAC prompt, or with its own yes or no question when it is
/// already elevated and the UAC prompt would not appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Uac,
    Own,
}

/// What `offer` and `offer-remove` read, as the installing user, before deciding.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    pub record: Option<&'a Record>,
    pub user: &'a str,
    pub drives_covered: bool,
    pub payload_current: bool,
    pub declined: Option<u32>,
    pub update: bool,
    pub silent: bool,
    pub elevated: bool,
}

impl Facts<'_> {
    /// The prompt this process would raise. Output: [`Prompt::Own`] when it is already
    /// elevated, else [`Prompt::Uac`].
    fn prompt(&self) -> Prompt {
        if self.elevated {
            Prompt::Own
        } else {
            Prompt::Uac
        }
    }
}

/// What `offer` does at install: nothing (done), nothing on a silent install, nothing on an
/// updater run after this version was declined, or ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Done,
    Skipped,
    StillDeclined,
    Ask(Prompt),
}

/// The `offer` decision table (R6, Q3 "a"). Input: the facts. Output: the decision.
pub fn offer(facts: &Facts<'_>) -> Offer {
    let recorded = facts
        .record
        .is_some_and(|record| record.done_for(facts.user));
    if recorded && facts.drives_covered && facts.payload_current {
        Offer::Done
    } else if facts.silent {
        Offer::Skipped
    } else if facts.update && facts.declined == Some(VERSION) {
        Offer::StillDeclined
    } else {
        Offer::Ask(facts.prompt())
    }
}

/// What an elevated `unapply` takes away: nothing, only its account from the count, or
/// every change the step made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unapply {
    Nothing,
    DropUser,
    Everything,
}

/// The `unapply` decision (Q3 "a"): the changes stay until the last counted account goes.
/// Inputs: the record and the account, None for the payload's `remove`. Output: the plan.
pub fn unapply(record: Option<&Record>, user: Option<&str>) -> Unapply {
    let Some(record) = record else {
        return Unapply::Nothing;
    };
    let Some(user) = user else {
        return Unapply::Everything;
    };
    let others = record.users.iter().any(|known| known != user);
    match (record.counts(user), others) {
        (_, false) => Unapply::Everything,
        (true, true) => Unapply::DropUser,
        (false, true) => Unapply::Nothing,
    }
}

/// What `offer-remove` does at uninstall: nothing, nothing on a silent uninstall, or ask
/// for an `unapply` that removes `Unapply::DropUser` or `Unapply::Everything`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferRemove {
    Nothing,
    Skipped,
    Ask(Prompt, Unapply),
}

/// The `offer-remove` decision table (Q3 "a"). Input: the facts; only the record, user,
/// silent and elevated ones are read. Output: the decision.
pub fn offer_remove(facts: &Facts<'_>) -> OfferRemove {
    match unapply(facts.record, Some(facts.user)) {
        Unapply::Nothing => OfferRemove::Nothing,
        _ if facts.silent => OfferRemove::Skipped,
        removal => OfferRemove::Ask(facts.prompt(), removal),
    }
}

/// The setup binary's roles: `offer` and `offer-remove` unelevated, `apply` and `unapply`
/// elevated for one canonical SID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupRole {
    Offer { update: bool, silent: bool },
    OfferRemove { silent: bool },
    Apply { user: String },
    Unapply { user: String },
}

/// Reads the setup binary's arguments after its own name (R12c): `offer [--update]
/// [--silent]`, `offer-remove [--silent]`, `apply --user <SID>` or `unapply --user <SID>`
/// with a canonical SID. Input: the arguments. Output: the role, or [`ARGUMENTS_REFUSED`].
pub fn setup_role(args: &[OsString]) -> Result<SetupRole, &'static str> {
    let words = unicode(args)?;
    let flags = |allowed: &[&str]| {
        let given = &words[1..];
        let known = given.iter().all(|word| allowed.contains(word));
        let once = given
            .iter()
            .enumerate()
            .all(|(at, word)| !given[..at].contains(word));
        (known && once).then(|| given.to_vec())
    };
    let role = match words.as_slice() {
        ["offer", ..] => flags(&["--update", "--silent"]).map(|given| SetupRole::Offer {
            update: given.contains(&"--update"),
            silent: given.contains(&"--silent"),
        }),
        ["offer-remove", ..] => flags(&["--silent"]).map(|given| SetupRole::OfferRemove {
            silent: given.contains(&"--silent"),
        }),
        ["apply", "--user", user] if canonical_sid(user) => Some(SetupRole::Apply {
            user: (*user).to_owned(),
        }),
        ["unapply", "--user", user] if canonical_sid(user) => Some(SetupRole::Unapply {
            user: (*user).to_owned(),
        }),
        _ => None,
    };
    role.ok_or(ARGUMENTS_REFUSED)
}

/// The payload binary's roles: `device-grant` for the boot task, `remove` for an
/// administrator's cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadRole {
    DeviceGrant,
    Remove,
}

/// Reads the payload binary's arguments after its own name: exactly `device-grant` or
/// `remove`. Input: the arguments. Output: the role, or [`ARGUMENTS_REFUSED`].
pub fn payload_role(args: &[OsString]) -> Result<PayloadRole, &'static str> {
    match unicode(args)?.as_slice() {
        ["device-grant"] => Ok(PayloadRole::DeviceGrant),
        ["remove"] => Ok(PayloadRole::Remove),
        _ => Err(ARGUMENTS_REFUSED),
    }
}

/// The arguments as text, refusing any that is not valid Unicode. Input: the arguments.
/// Output: the words, or [`ARGUMENTS_REFUSED`].
fn unicode(args: &[OsString]) -> Result<Vec<&str>, &'static str> {
    args.iter()
        .map(|arg| arg.to_str().ok_or(ARGUMENTS_REFUSED))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRIVE_C: &str = r"\\?\Volume{6f2e0a1b-1c2d-11ef-9a3b-806e6f6e6963}\";
    const DRIVE_D: &str = r"\\?\Volume{0A1B2C3D-4E5F-6071-8293-A4B5C6D7E8F9}\";
    const OWNER: &str = "S-1-5-21-3623811015-3361044348-30300820-1013";
    const OTHER: &str = "S-1-12-1-1234567890-1234567890-1234567890-123456789";

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn record(users: &[&str]) -> Record {
        Record {
            version: VERSION,
            drives: strings(&[DRIVE_C, DRIVE_D]),
            users: strings(users),
            device_task: true,
        }
    }

    fn parsed(values: &[(&'static str, Value)]) -> Result<Record, &'static str> {
        Record::parse(|name| {
            values
                .iter()
                .find(|(found, _)| *found == name)
                .map(|(_, value)| value.clone())
        })
    }

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn facts<'a>(record: Option<&'a Record>) -> Facts<'a> {
        Facts {
            record,
            user: OWNER,
            drives_covered: true,
            payload_current: true,
            declined: None,
            update: false,
            silent: false,
            elevated: false,
        }
    }

    #[test]
    fn a_record_round_trips_through_its_registry_values() {
        let full = record(&[OWNER, OTHER]);
        let values = full.values();
        let names: Vec<&str> = values.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["Version", "Drives", "Users", "DeviceTask"]);
        assert_eq!(values[0].1, Value::Dword(VERSION));
        assert_eq!(values[3].1, Value::Dword(1));
        assert_eq!(parsed(&values), Ok(full));

        let bare = Record {
            version: VERSION,
            drives: Vec::new(),
            users: Vec::new(),
            device_task: false,
        };
        assert_eq!(bare.values(), [("Version", Value::Dword(VERSION))]);
        assert_eq!(parsed(&bare.values()), Ok(bare));
    }

    #[test]
    fn a_record_with_any_value_this_build_would_not_write_is_malformed() {
        let good = record(&[OWNER]).values();
        let with = |name: &'static str, value: Option<Value>| {
            let mut changed: Vec<(&'static str, Value)> = good
                .iter()
                .filter(|(found, _)| *found != name)
                .cloned()
                .collect();
            if let Some(value) = value {
                changed.push((name, value));
            }
            changed
        };
        let list = |items: &[&str]| Some(Value::MultiSz(multi_sz(&strings(items))));
        let mut too_many: Vec<String> = Vec::new();
        for n in 0..65 {
            too_many.push(format!("S-1-5-21-1-2-3-{}", 1000 + n));
        }
        for (values, why) in [
            (with("Version", None), "no version"),
            (with("Version", Some(Value::Dword(0))), "version 0"),
            (with("Version", list(&[OWNER])), "a version that is a list"),
            (
                with("Drives", Some(Value::Dword(1))),
                "drives that are a number",
            ),
            (
                with("Drives", list(&[r"C:\"])),
                "a drive letter, not a volume path",
            ),
            (
                with(
                    "Drives",
                    list(&[DRIVE_C, &DRIVE_C.replace("6f2e0a1b", "6F2E0A1B")]),
                ),
                "a drive twice, its hex in another case",
            ),
            (with("Users", list(&["BA"])), "an SDDL alias"),
            (with("Users", list(&[OWNER, OWNER])), "an account twice"),
            (
                with("Users", Some(Value::MultiSz(multi_sz(&too_many)))),
                "more accounts than any machine has",
            ),
            (
                with("DeviceTask", Some(Value::Dword(0))),
                "a task value of 0",
            ),
            (
                with("DeviceTask", Some(Value::Dword(2))),
                "a task value of 2",
            ),
            (
                with("DeviceTask", list(&["1"])),
                "a task value that is a list",
            ),
            (
                with(
                    "Users",
                    Some(Value::MultiSz([wide(OWNER), vec![0]].concat())),
                ),
                "a list missing its last terminator",
            ),
            (
                with("Users", Some(Value::MultiSz(vec![0, 0]))),
                "an empty list that the writer leaves out",
            ),
            (
                with(
                    "Users",
                    Some(Value::MultiSz(
                        [wide(OWNER), vec![0, 0], wide(OTHER), vec![0, 0]].concat(),
                    )),
                ),
                "an empty item inside a list",
            ),
        ] {
            assert_eq!(parsed(&values), Err(RECORD_MALFORMED), "{why}");
        }
        assert!(parsed(&with("Drives", None)).is_ok_and(|read| read.drives.is_empty()));
        assert!(parsed(&with("DeviceTask", None)).is_ok_and(|read| !read.device_task));
        assert!(parsed(&with("Version", Some(Value::Dword(VERSION + 1)))).is_ok());
    }

    #[test]
    fn volume_paths_and_sids_are_read_strictly() {
        assert!(volume_path(DRIVE_C));
        assert!(volume_path(DRIVE_D));
        for text in [
            r"\\?\Volume{6f2e0a1b-1c2d-11ef-9a3b-806e6f6e6963}",
            r"\\.\Volume{6f2e0a1b-1c2d-11ef-9a3b-806e6f6e6963}\",
            r"\\?\Volume{6f2e0a1b-1c2d-11ef-9a3b-806e6f6e696}\",
            r"\\?\Volume{6f2e0a1b-1c2d-11ef-9a3b-806e6f6e696g}\",
            r"\\?\Volume{6f2e0a1b1c2d-11ef-9a3b-806e6f6e69630}\",
            r"\\?\volume{6f2e0a1b-1c2d-11ef-9a3b-806e6f6e6963}\",
            r"\\?\Volume{6f2e0a1b-1c2d-11ef-9a3b-806e6f6e6963}\x",
            r"C:\",
            "",
        ] {
            assert!(!volume_path(text), "{text}");
        }
        for text in [
            OWNER,
            OTHER,
            "S-1-5-18",
            "S-1-0-0",
            "S-1-5-21-0-4294967295-7-500",
        ] {
            assert!(canonical_sid(text), "{text}");
        }
        for text in [
            "s-1-5-18",
            "S-1-5",
            "S-1-05-18",
            "S-1-5-018",
            "S-1-5-21-4294967296",
            "S-1-0x5-18",
            "S-2-5-18",
            "S-1-5--18",
            "S-1-5-18-",
            "S-1-5-18 ",
            "S-1-5-+18",
            "S-1-5-1-2-3-4-5-6-7-8-9-10-11-12-13-14-15-16",
            "BA",
            "",
        ] {
            assert!(!canonical_sid(text), "{text}");
        }
        assert!(canonical_sid("S-1-5-1-2-3-4-5-6-7-8-9-10-11-12-13-14-15"));
    }

    #[test]
    fn offer_asks_only_when_the_step_is_not_done_for_this_account() {
        let mine = record(&[OWNER]);
        let theirs = record(&[OTHER]);
        let mut old_task = record(&[OWNER]);
        old_task.device_task = false;
        assert_eq!(offer(&facts(Some(&mine))), Offer::Done);
        assert_eq!(
            offer(&Facts {
                silent: true,
                update: true,
                ..facts(Some(&mine))
            }),
            Offer::Done,
            "done is quiet under every mode"
        );
        for (state, why) in [
            (facts(None), "nothing recorded"),
            (
                facts(Some(&theirs)),
                "another account applied it, this one is not counted",
            ),
            (facts(Some(&old_task)), "the device task is not recorded"),
            (
                Facts {
                    drives_covered: false,
                    ..facts(Some(&mine))
                },
                "a drive lacks the entry",
            ),
            (
                Facts {
                    payload_current: false,
                    ..facts(Some(&mine))
                },
                "the payload is stale",
            ),
            (
                Facts {
                    declined: Some(VERSION),
                    ..facts(None)
                },
                "declined before, but this is a manual install",
            ),
            (
                Facts {
                    declined: Some(VERSION - 1),
                    update: true,
                    ..facts(None)
                },
                "declined an older version",
            ),
        ] {
            assert_eq!(offer(&state), Offer::Ask(Prompt::Uac), "{why}");
            assert_eq!(
                offer(&Facts {
                    elevated: true,
                    ..state
                }),
                Offer::Ask(Prompt::Own),
                "{why}, already elevated"
            );
            assert_eq!(
                offer(&Facts {
                    silent: true,
                    ..state
                }),
                Offer::Skipped,
                "{why}, silent"
            );
        }
        assert_eq!(
            offer(&Facts {
                declined: Some(VERSION),
                update: true,
                ..facts(Some(&theirs))
            }),
            Offer::StillDeclined,
            "a declined add is not asked again on an update"
        );
    }

    #[test]
    fn the_changes_stay_until_the_last_counted_account_uninstalls() {
        let alone = record(&[OWNER]);
        let shared = record(&[OTHER, OWNER]);
        let theirs = record(&[OTHER]);
        let nobody = record(&[]);
        assert_eq!(unapply(None, Some(OWNER)), Unapply::Nothing);
        assert_eq!(unapply(Some(&theirs), Some(OWNER)), Unapply::Nothing);
        assert_eq!(unapply(Some(&shared), Some(OWNER)), Unapply::DropUser);
        assert_eq!(unapply(Some(&alone), Some(OWNER)), Unapply::Everything);
        assert_eq!(unapply(Some(&nobody), Some(OWNER)), Unapply::Everything);
        assert_eq!(unapply(Some(&shared), None), Unapply::Everything, "remove");
        assert_eq!(unapply(None, None), Unapply::Nothing);

        assert_eq!(offer_remove(&facts(None)), OfferRemove::Nothing);
        assert_eq!(offer_remove(&facts(Some(&theirs))), OfferRemove::Nothing);
        assert_eq!(
            offer_remove(&facts(Some(&shared))),
            OfferRemove::Ask(Prompt::Uac, Unapply::DropUser)
        );
        assert_eq!(
            offer_remove(&facts(Some(&alone))),
            OfferRemove::Ask(Prompt::Uac, Unapply::Everything)
        );
        assert_eq!(
            offer_remove(&Facts {
                elevated: true,
                ..facts(Some(&alone))
            }),
            OfferRemove::Ask(Prompt::Own, Unapply::Everything)
        );
        assert_eq!(
            offer_remove(&Facts {
                silent: true,
                ..facts(Some(&shared))
            }),
            OfferRemove::Skipped
        );
        assert_eq!(
            offer_remove(&Facts {
                silent: true,
                ..facts(Some(&theirs))
            }),
            OfferRemove::Nothing
        );
    }

    #[test]
    fn an_account_is_counted_once_and_dropped_alone() {
        let mut older = record(&[OTHER]);
        older.version = VERSION;
        let added = older.with_user(OWNER).unwrap();
        assert_eq!(added.users, strings(&[OTHER, OWNER]));
        assert_eq!(added.drives, older.drives);
        assert!(added.done_for(OWNER) && added.done_for(OTHER));
        assert_eq!(
            added.with_user(OWNER),
            Ok(added.clone()),
            "a second apply counts nothing more"
        );
        let mut newer = added.clone();
        newer.version = VERSION + 1;
        assert_eq!(
            newer.with_user(OWNER).unwrap().version,
            VERSION + 1,
            "never lowered"
        );
        let dropped = added.without_user(OWNER);
        assert_eq!(dropped.users, strings(&[OTHER]));
        assert_eq!(dropped.drives, added.drives);
        assert!(dropped.device_task);
        assert_eq!(dropped.without_user(OWNER), dropped);
        assert!(!dropped.done_for(OWNER));

        let mut full = record(&[]);
        for n in 0..64 {
            full = full
                .with_user(&format!("S-1-5-21-1-2-3-{}", 1000 + n))
                .unwrap();
        }
        assert_eq!(full.users.len(), 64);
        assert_eq!(parsed(&full.values()), Ok(full.clone()));
        assert_eq!(full.with_user(OWNER), Err(RECORD_FULL));
        assert_eq!(full.with_user("S-1-5-21-1-2-3-1000"), Ok(full.clone()));
    }

    #[test]
    fn each_binary_takes_exactly_its_roles() {
        for (line, role) in [
            (
                vec!["offer"],
                SetupRole::Offer {
                    update: false,
                    silent: false,
                },
            ),
            (
                vec!["offer", "--update"],
                SetupRole::Offer {
                    update: true,
                    silent: false,
                },
            ),
            (
                vec!["offer", "--silent"],
                SetupRole::Offer {
                    update: false,
                    silent: true,
                },
            ),
            (
                vec!["offer", "--silent", "--update"],
                SetupRole::Offer {
                    update: true,
                    silent: true,
                },
            ),
            (
                vec!["offer-remove"],
                SetupRole::OfferRemove { silent: false },
            ),
            (
                vec!["offer-remove", "--silent"],
                SetupRole::OfferRemove { silent: true },
            ),
            (
                vec!["apply", "--user", OWNER],
                SetupRole::Apply { user: OWNER.into() },
            ),
            (
                vec!["unapply", "--user", OTHER],
                SetupRole::Unapply { user: OTHER.into() },
            ),
        ] {
            assert_eq!(setup_role(&args(&line)), Ok(role), "{line:?}");
        }
        for line in [
            vec![],
            vec!["Offer"],
            vec!["offer", "--update", "--update"],
            vec!["offer", "--user", OWNER],
            vec!["offer-remove", "--update"],
            vec!["apply"],
            vec!["apply", "--user"],
            vec!["apply", OWNER, "--user"],
            vec!["apply", "--user", OWNER, "--user", OWNER],
            vec!["apply", "--user", OWNER, "--silent"],
            vec!["apply", "--user", "BA"],
            vec!["apply", "--user", "s-1-5-18"],
            vec!["apply", "--user=S-1-5-18"],
            vec!["unapply", "--user", "S-1-5-018"],
            vec!["device-grant"],
            vec!["remove"],
        ] {
            assert_eq!(setup_role(&args(&line)), Err(ARGUMENTS_REFUSED), "{line:?}");
        }
        assert_eq!(
            payload_role(&args(&["device-grant"])),
            Ok(PayloadRole::DeviceGrant)
        );
        assert_eq!(payload_role(&args(&["remove"])), Ok(PayloadRole::Remove));
        for line in [
            vec![],
            vec!["nul-grant"],
            vec!["device-grant", "--user", OWNER],
            vec!["remove", "now"],
            vec!["offer"],
            vec!["apply", "--user", OWNER],
        ] {
            assert_eq!(
                payload_role(&args(&line)),
                Err(ARGUMENTS_REFUSED),
                "{line:?}"
            );
        }
        let unpaired = {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;
                OsString::from_vec(vec![0x6f, 0xff])
            }
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;
                OsString::from_wide(&[0x6f, 0xd800])
            }
        };
        assert_eq!(setup_role(&[unpaired.clone()]), Err(ARGUMENTS_REFUSED));
        assert_eq!(
            setup_role(&[
                OsString::from("apply"),
                OsString::from("--user"),
                unpaired.clone()
            ]),
            Err(ARGUMENTS_REFUSED)
        );
        assert_eq!(payload_role(&[unpaired]), Err(ARGUMENTS_REFUSED));
    }
}
