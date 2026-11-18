/// Generates the Tauri context for the spike window. Inputs: tauri.conf.json and the
/// capabilities folder. Output: the build script's generated files.
fn main() {
    tauri_build::build()
}
