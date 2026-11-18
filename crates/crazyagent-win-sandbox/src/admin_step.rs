use std::ffi::OsString;

/// The design version of the administrator step this build writes and reads
/// (V244-WIN-ADMIN-STEP): drive-root entries, the device grants and their boot task.
pub const VERSION: u32 = 1;

/// The one refusal for a machine record whose values are missing, of the wrong type, not
/// strict volume paths or canonical SIDs, repeated or too many.
pub const RECORD_MALFORMED: &str = "admin_record_malformed";

/// The one refusal for a command line that is not exactly one of the binary's roles.
pub const ARGUMENTS_REFUSED: &str = "admin_arguments_refused";

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
        let _ = value;
        todo!("B2")
    }

    /// The named values this record is written as, an empty list or a false task left out.
    /// Output: the names and values in write order.
    pub fn values(&self) -> Vec<(&'static str, Value)> {
        todo!("B2")
    }

    /// Whether this record says the step is complete for `user`: this build's version or
    /// later, `user` counted and the device task registered. Input: a canonical SID.
    /// Output: true when it does.
    pub fn done_for(&self, user: &str) -> bool {
        let _ = user;
        todo!("B2")
    }

    /// The record after `user` applied: counted once, and the version brought up to this
    /// build's. Input: a canonical SID. Output: the new record.
    pub fn with_user(&self, user: &str) -> Record {
        let _ = user;
        todo!("B2")
    }

    /// The record with `user` no longer counted and everything else kept. Input: a
    /// canonical SID. Output: the new record.
    pub fn without_user(&self, user: &str) -> Record {
        let _ = user;
        todo!("B2")
    }
}

/// Whether `text` is a volume GUID path as Windows names a volume root:
/// `\\?\Volume{8-4-4-4-12 hex digits}\`. Input: the text. Output: true when it is.
pub fn volume_path(text: &str) -> bool {
    let _ = text;
    todo!("B2")
}

/// Whether `text` is a SID in the canonical form Windows writes: `S-1-`, a decimal
/// authority, and one to fifteen decimal sub-authorities, each a u32 with no leading zero.
/// Input: the text. Output: true when it is.
pub fn canonical_sid(text: &str) -> bool {
    let _ = text;
    todo!("B2")
}

/// The REG_MULTI_SZ units of `items`: each item and its terminator, then one more. Input:
/// non-empty items with no NUL. Output: the units.
pub fn multi_sz(items: &[String]) -> Vec<u16> {
    let _ = items;
    todo!("B2")
}

/// The items of REG_MULTI_SZ `units`, only when they are exactly what [`multi_sz`] writes
/// for at least one item. Input: the units. Output: the items, or None.
pub fn parse_multi_sz(units: &[u16]) -> Option<Vec<String>> {
    let _ = units;
    todo!("B2")
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
    let _ = facts;
    todo!("B2")
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
    let _ = (record, user);
    todo!("B2")
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
    let _ = facts;
    todo!("B2")
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
    let _ = args;
    todo!("B2")
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
    let _ = args;
    todo!("B2")
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
        let added = older.with_user(OWNER);
        assert_eq!(added.users, strings(&[OTHER, OWNER]));
        assert_eq!(added.drives, older.drives);
        assert!(added.done_for(OWNER) && added.done_for(OTHER));
        assert_eq!(
            added.with_user(OWNER),
            added,
            "a second apply counts nothing more"
        );
        let mut newer = added.clone();
        newer.version = VERSION + 1;
        assert_eq!(newer.with_user(OWNER).version, VERSION + 1, "never lowered");
        let dropped = added.without_user(OWNER);
        assert_eq!(dropped.users, strings(&[OTHER]));
        assert_eq!(dropped.drives, added.drives);
        assert!(dropped.device_task);
        assert_eq!(dropped.without_user(OWNER), dropped);
        assert!(!dropped.done_for(OWNER));
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
