#![cfg(unix)]

use crazyagent::isolation::run_resource_probe;
use std::fs;
use std::path::PathBuf;

#[test]
fn inherited_resource_limits_have_real_observations() {
    let scratch =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local-state/resource-tests");
    fs::create_dir_all(&scratch).unwrap();
    let root = tempfile::tempdir_in(scratch).unwrap().keep();
    eprintln!("retained resource artifacts: {}", root.display());
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_crazyagent-boundary-child"));
    let evidence = run_resource_probe(&executable, &root).unwrap();
    assert_eq!(evidence.len(), 7);
    let cpu = &evidence["limit-cpu"];
    assert!(matches!(cpu.signal, Some(24 | 9)), "{cpu:?}");
    assert!(
        cpu.stdout
            .contains("inherited Cpu soft=Some(2) hard=Some(2)"),
        "{cpu:?}"
    );
    assert!(cpu.elapsed_ms < 5000, "{cpu:?}");
    assert!(
        evidence["memory-baseline"]
            .stdout
            .contains("allocated-and-touched=25165824")
    );
    assert!(
        evidence["process-baseline"]
            .stdout
            .contains("children-started=6")
    );
    for name in ["limit-as", "limit-rss", "limit-nproc"] {
        assert!(
            evidence[name].stdout.contains("set-limit"),
            "{:?}",
            evidence[name]
        );
    }
}
