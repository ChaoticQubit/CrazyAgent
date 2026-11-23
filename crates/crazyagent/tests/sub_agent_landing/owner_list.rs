use super::*;

/// While Lands is Lead, the owner's Changes list leaves out a lead's workers' sets, since
/// the lead merges them; with Lands set to Owner it shows them (fix/hand-merged-set
/// 8f98740a).
#[test]
fn owner_list_leaves_out_lead_workers() {
    let team = Team::new();
    let fix = team.worker(2, &team.lead, "fix");
    team.commit(&fix, "fix.txt", "fixed\n", "Fix it");
    let worker_set = team.turn_end(&fix);
    team.commit(&team.lead, "lead.txt", "lead\n", "Lead work");
    let lead_set = team.turn_end(&team.lead);
    assert_eq!(team.owner_listed(), vec![lead_set.clone()]);
    assert_eq!(landing::list(&team.db, &team.id).unwrap().len(), 2);
    team.lands("owner");
    let mut both = vec![lead_set, worker_set];
    both.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(team.owner_listed(), both);
}
