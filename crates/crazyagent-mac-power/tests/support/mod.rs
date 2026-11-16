#![allow(dead_code)]

use crazyagent_mac_power::helper::Config;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The battery text `pmset -g batt` prints on AC at 100 %, as K0-12 recorded it.
pub const BATTERY_AC: &str = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; \
charged; 0:00 remaining present: true\n";

/// A stand-in for `/usr/bin/pmset` in a temp folder: a shell program that appends each call's
/// arguments to `calls`, keeps `SleepDisabled` in a file, prints `-g` and `-g batt` from it,
/// and copies the journal aside at each `disablesleep` so a row can see its order. Owns the
/// folder the helper keeps its state in, so every row runs on real files.
pub struct StandIn {
    root: TempDir,
}

impl StandIn {
    /// Makes the stand-in with `SleepDisabled` at `sleep_disabled` (0 or 1), the battery on AC
    /// and a console file owned by the user running the row. Output: the stand-in.
    pub fn new(sleep_disabled: u8) -> Self {
        assert!(sleep_disabled <= 1);
        let root = tempfile::tempdir().expect("temp folder");
        let stand_in = Self { root };
        fs::write(
            stand_in.path("sleep_disabled"),
            format!("{sleep_disabled}\n"),
        )
        .unwrap();
        fs::write(stand_in.path("batt"), BATTERY_AC).unwrap();
        fs::write(stand_in.path("calls"), "").unwrap();
        fs::write(stand_in.path("console"), "").unwrap();
        stand_in.write_program();
        assert!(stand_in.program().is_file());
        stand_in
    }

    fn write_program(&self) {
        let folder = self.root.path().display().to_string();
        assert!(!folder.contains('\''));
        let text = format!(
            "#!/bin/sh\nD='{folder}'\nprintf '%s\\n' \"$*\" >> \"$D/calls\"\n\
             if [ \"$1\" = \"-a\" ] && [ \"$2\" = \"disablesleep\" ]; then\n\
             cp \"$D/state/journal.json\" \"$D/journal-when-set-$3\" 2>/dev/null \
             || : > \"$D/journal-when-set-$3-missing\"\n\
             printf '%s\\n' \"$3\" > \"$D/sleep_disabled\"\n\
             elif [ \"$1\" = \"-g\" ] && [ \"$2\" = \"batt\" ]; then\ncat \"$D/batt\"\n\
             elif [ \"$1\" = \"-g\" ]; then\n\
             printf ' SleepDisabled\\t\\t%s\\n lowpowermode         0\\n' \"$(cat \"$D/sleep_disabled\")\"\n\
             fi\nexit 0\n"
        );
        fs::write(self.program(), text).unwrap();
        fs::set_permissions(self.program(), fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A path inside the stand-in's folder. Input: the name. Output: the path.
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The stand-in program's path.
    pub fn program(&self) -> PathBuf {
        self.path("pmset")
    }

    /// The folder the helper keeps its journal and pin in (made by the helper, not here).
    pub fn folder(&self) -> PathBuf {
        self.path("state")
    }

    /// The uid of the user running the row, read from a file it made.
    pub fn my_uid(&self) -> u32 {
        fs::metadata(self.path("console")).unwrap().uid()
    }

    /// The helper's configuration for this stand-in: its folder, owned by the row's user, the
    /// stand-in program, and a console owned by the row's user (or `console` if given).
    pub fn config(&self) -> Config {
        self.config_with_console(&self.path("console"))
    }

    /// As `config`, with another console path: one owned by root plays the login window.
    pub fn config_with_console(&self, console: &Path) -> Config {
        Config {
            folder: self.folder(),
            owner_uid: self.my_uid(),
            pmset: self.program(),
            console: console.to_path_buf(),
        }
    }

    /// Every call the stand-in got, one argument string per call, in order.
    pub fn calls(&self) -> Vec<String> {
        let text = fs::read_to_string(self.path("calls")).unwrap();
        text.lines().map(str::to_string).collect()
    }

    /// The calls that wrote `SleepDisabled`, in order.
    pub fn sleep_writes(&self) -> Vec<String> {
        let calls = self.calls();
        calls
            .into_iter()
            .filter(|call| call.starts_with("-a disablesleep"))
            .collect()
    }

    /// Sets `SleepDisabled` as another tool would, behind the helper's back.
    pub fn set_by_another_tool(&self, value: u8) {
        assert!(value <= 1);
        fs::write(self.path("sleep_disabled"), format!("{value}\n")).unwrap();
    }

    /// The `SleepDisabled` value the stand-in holds now.
    pub fn sleep_disabled(&self) -> u8 {
        let text = fs::read_to_string(self.path("sleep_disabled")).unwrap();
        text.trim().parse().unwrap()
    }

    /// Replaces the text `-g batt` prints.
    pub fn set_battery(&self, text: &str) {
        fs::write(self.path("batt"), text).unwrap();
    }

    /// The journal file's text, or `None` when there is none.
    pub fn journal_text(&self) -> Option<String> {
        fs::read_to_string(self.folder().join("journal.json")).ok()
    }

    /// The journal as it was when the stand-in got `disablesleep <value>`, or `None` if there
    /// was no journal file then.
    pub fn journal_when_set(&self, value: u8) -> Option<String> {
        fs::read_to_string(self.path(&format!("journal-when-set-{value}"))).ok()
    }

    /// The pin file's text, or `None` when there is none.
    pub fn pin_text(&self) -> Option<String> {
        fs::read_to_string(self.folder().join("uid")).ok()
    }
}
