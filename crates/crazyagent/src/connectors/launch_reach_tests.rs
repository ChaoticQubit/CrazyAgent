use super::reach;
use crate::coord::settings::HarnessKind;

/// DEC-141 parity for connectors (Task 14): every harness kind has one Connectors page row,
/// in the order settings list the kinds, each naming its harness; an RCLI master now starts
/// the gateway as its one MCP server, so no row says "not yet".
#[test]
fn every_harness_kind_has_a_connectors_mapping() {
    let rows: Vec<_> = HarnessKind::ALL.into_iter().map(reach).collect();
    assert_eq!(rows.len(), HarnessKind::ALL.len());
    assert_eq!(rows, ["Claude Code: yes", "Codex: yes", "RCLI: yes (master)"]);
    assert!(rows.iter().all(|row| !row.contains("not yet")));
}
