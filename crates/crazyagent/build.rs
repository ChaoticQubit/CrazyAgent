use std::path::Path;

/// Runs the central plugin check over every bundled plugin before the engine is built, and
/// fails the build listing each violation. It reruns when any file below `plugins/` or this
/// script changes. Inputs: the crate folder from cargo. Output: none; panics on a violation.
fn main() {
    println!("cargo:rerun-if-changed=../../plugins");
    println!("cargo:rerun-if-changed=build.rs");
    let crate_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets the crate folder");
    let plugins = Path::new(&crate_dir).join("../../plugins");
    if let Err(violations) = crazyagent_plugin_check::check_plugins(&plugins) {
        let lines: Vec<String> = violations
            .iter()
            .map(|violation| {
                format!(
                    "plugins/{}:{} {}",
                    violation.file.display(),
                    violation.line,
                    violation.code
                )
            })
            .collect();
        panic!(
            "plugin check refused the bundled plugins:\n{}",
            lines.join("\n")
        );
    }
}
