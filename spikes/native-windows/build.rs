/// Runs the Tauri build step: the window config, capabilities and, on Windows, the
/// resource file with the icon. Input: the crate's tauri.conf.json. Output: the
/// generated context the binary embeds.
fn main() {
    tauri_build::build()
}
