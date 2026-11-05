#![cfg(unix)]

/// The variable that arms the trial pick seam, spelled backwards so that this test's own
/// binary never holds the forward spelling it looks for.
const VARIABLE_REVERSED: &str = "SKCIP_LAIRT_TNEGAYZARC";

/// The app binary that cargo built for this test, with this test's features: the shipped app
/// for a default build, the trial app for a native-webdriver build.
const APP: &str = env!("CARGO_BIN_EXE_crazyagent-desktop");

#[test]
fn only_a_native_webdriver_build_carries_the_trial_pick_seam() {
    let variable: String = VARIABLE_REVERSED.chars().rev().collect();
    let app = std::fs::read(APP).unwrap();
    assert!(app.len() > 1024 * 1024, "{APP} is not the app binary");
    let carried = app
        .windows(variable.len())
        .any(|window| window == variable.as_bytes());
    assert_eq!(
        carried,
        cfg!(feature = "native-webdriver"),
        "{APP} carries {variable}: {carried}; a default build must not hold the trial pick seam"
    );
}
