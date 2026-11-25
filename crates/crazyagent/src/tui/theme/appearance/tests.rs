use super::*;
use std::cell::RefCell;
use std::path::PathBuf;

const PERSONALIZE: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const REG_DARK: &str = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x0\r\n\r\n";
const REG_LIGHT: &str = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x1\r\n\r\n";

struct Asked {
    appearance: Appearance,
    spawned: Vec<Vec<String>>,
    read: Vec<PathBuf>,
}

/// Runs the reader for one system with a fake environment, a fake child answer and a fake
/// file, recording every program it starts and every file it reads.
/// Inputs: the system, the environment pairs, the child's stdout and the file text.
/// Output: the appearance, the started command lines and the read paths.
fn ask(os: Os, pairs: &[(&str, &str)], stdout: Option<&str>, file: Option<&str>) -> Asked {
    let mut spawned = Vec::new();
    let read = RefCell::new(Vec::new());
    let var = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    };
    let mut run = |command: &mut Command| {
        let mut line = vec![command.get_program().to_string_lossy().into_owned()];
        line.extend(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        spawned.push(line);
        stdout.map(|text| text.as_bytes().to_vec())
    };
    let open = |path: &Path| {
        read.borrow_mut().push(path.to_path_buf());
        file.map(str::to_owned)
    };
    let appearance = appearance_on(os, &mut run, &var, &open);
    Asked {
        appearance,
        spawned,
        read: read.into_inner(),
    }
}

fn line(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn macos_reads_defaults_at_its_absolute_path() {
    let asked = ask(Os::Macos, &[("PATH", "/tmp/planted")], Some("Dark\n"), None);
    assert_eq!(asked.appearance, Appearance::Dark);
    assert_eq!(
        asked.spawned,
        vec![line(&[
            "/usr/bin/defaults",
            "read",
            "-g",
            "AppleInterfaceStyle"
        ])]
    );
    assert!(asked.read.is_empty());
    for answer in [Some("Light\n"), Some(""), None] {
        assert_eq!(
            ask(Os::Macos, &[], answer, None).appearance,
            Appearance::Light
        );
    }
}

#[test]
fn windows_reads_reg_exe_under_system_root() {
    let asked = ask(
        Os::Windows,
        &[("SystemRoot", r"D:\Win")],
        Some(REG_DARK),
        None,
    );
    assert_eq!(asked.appearance, Appearance::Dark);
    assert_eq!(
        asked.spawned,
        vec![line(&[
            r"D:\Win\System32\reg.exe",
            "query",
            PERSONALIZE,
            "/v",
            "AppsUseLightTheme"
        ])]
    );
    assert!(asked.read.is_empty());
}

#[test]
fn windows_without_a_drive_system_root_uses_c_windows() {
    for pairs in [
        &[][..],
        &[("SystemRoot", "Windows")][..],
        &[("SystemRoot", r"\\server\share")][..],
        &[("SystemRoot", r"D:\..\Temp")][..],
        &[("SystemRoot", "")][..],
    ] {
        let asked = ask(Os::Windows, pairs, Some(REG_DARK), None);
        assert_eq!(
            asked.spawned[0][0], r"C:\Windows\System32\reg.exe",
            "{pairs:?}"
        );
    }
    let asked = ask(Os::Windows, &[("SystemRoot", r"E:\Windows\")], None, None);
    assert_eq!(asked.spawned[0][0], r"E:\Windows\System32\reg.exe");
}

#[test]
fn windows_light_or_unknown_answers_read_light() {
    for answer in [
        Some(REG_LIGHT),
        Some("ERROR: unable to find"),
        Some(""),
        None,
    ] {
        assert_eq!(
            ask(Os::Windows, &[], answer, None).appearance,
            Appearance::Light,
            "{answer:?}"
        );
    }
}

#[test]
fn linux_gnome_reads_gsettings_at_its_absolute_path_never_from_path() {
    let pairs = [
        ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
        ("PATH", "/tmp/planted"),
    ];
    let asked = ask(Os::Linux, &pairs, Some("'prefer-dark'\n"), None);
    assert_eq!(asked.appearance, Appearance::Dark);
    assert_eq!(
        asked.spawned,
        vec![line(&[
            "/usr/bin/gsettings",
            "get",
            "org.gnome.desktop.interface",
            "color-scheme"
        ])]
    );
    assert!(
        asked
            .spawned
            .iter()
            .all(|command| command[0] != "gsettings")
    );
    assert!(asked.read.is_empty());
}

#[test]
fn linux_gnome_default_light_or_failure_reads_light() {
    for answer in [
        Some("'default'\n"),
        Some("'prefer-light'\n"),
        Some(""),
        None,
    ] {
        let asked = ask(Os::Linux, &[("XDG_CURRENT_DESKTOP", "GNOME")], answer, None);
        assert_eq!(asked.appearance, Appearance::Light, "{answer:?}");
    }
}

#[test]
fn linux_unknown_desktop_asks_gsettings_and_reads_light_when_it_fails() {
    let asked = ask(Os::Linux, &[], None, None);
    assert_eq!(asked.appearance, Appearance::Light);
    assert_eq!(asked.spawned[0][0], "/usr/bin/gsettings");
}

#[test]
fn linux_kde_reads_kdeglobals_without_starting_a_program() {
    let pairs = [
        ("XDG_CURRENT_DESKTOP", "KDE"),
        ("XDG_CONFIG_HOME", "/home/mira/.cfg"),
        ("HOME", "/home/mira"),
    ];
    let asked = ask(
        Os::Linux,
        &pairs,
        None,
        Some("[General]\nColorScheme=BreezeDark\n"),
    );
    assert_eq!(asked.appearance, Appearance::Dark);
    assert!(asked.spawned.is_empty());
    assert_eq!(
        asked.read,
        vec![Path::new("/home/mira/.cfg").join("kdeglobals")]
    );
}

#[test]
fn linux_kde_reads_the_general_section_under_home_config() {
    let pairs = [
        ("XDG_CURRENT_DESKTOP", "KDE"),
        ("XDG_CONFIG_HOME", "relative/cfg"),
        ("HOME", "/home/mira"),
    ];
    let text = "[Colors:View]\nColorScheme=BreezeDark\n\n[General]\nColorScheme=BreezeLight\n";
    let asked = ask(Os::Linux, &pairs, None, Some(text));
    assert_eq!(asked.appearance, Appearance::Light);
    assert_eq!(
        asked.read,
        vec![Path::new("/home/mira").join(".config").join("kdeglobals")]
    );
    for file in [None, Some(""), Some("[General]\n")] {
        assert_eq!(
            ask(Os::Linux, &pairs, None, file).appearance,
            Appearance::Light
        );
    }
    let homeless = ask(Os::Linux, &[("XDG_CURRENT_DESKTOP", "KDE")], None, None);
    assert_eq!(homeless.appearance, Appearance::Light);
    assert!(homeless.read.is_empty() && homeless.spawned.is_empty());
}

#[test]
#[should_panic(expected = "absolute")]
fn a_program_that_is_not_an_absolute_path_is_never_started() {
    let _ = command("gsettings", &[]);
}

#[cfg(target_os = "macos")]
#[test]
fn this_build_reads_the_macos_setting() {
    assert_eq!(THIS_OS, Os::Macos);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn this_build_reads_the_linux_desktop_setting() {
    assert_eq!(THIS_OS, Os::Linux);
}

#[cfg(not(unix))]
#[test]
fn this_build_reads_the_windows_setting() {
    assert_eq!(THIS_OS, Os::Windows);
}

#[test]
fn a_real_read_answers_within_five_seconds() {
    let start = std::time::Instant::now();
    let appearance = system_appearance();
    let elapsed = start.elapsed();
    println!(
        "system_appearance ms {} appearance {appearance:?} os {THIS_OS:?}",
        elapsed.as_millis()
    );
    assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
}
