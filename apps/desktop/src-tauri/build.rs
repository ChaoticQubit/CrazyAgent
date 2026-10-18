fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "desktop_connect",
        "desktop_status",
        "desktop_baselines",
        "desktop_inspect_baseline",
        "desktop_approve_baseline",
        "desktop_revoke_baseline",
        "desktop_reconcile",
        "desktop_retry",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("desktop build configuration failed");
}
