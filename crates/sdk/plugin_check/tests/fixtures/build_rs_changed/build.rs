fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    crazyagent_plugin_check::enforce_here();
}
