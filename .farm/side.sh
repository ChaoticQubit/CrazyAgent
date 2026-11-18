set -euo pipefail
case "$SIDE" in
  base)
    git checkout "$RED_COMMIT" -- crates/crazyagent/src
    ;;
  mutant)
    ruby .farm/mutant.rb crates/crazyagent/src/team/cleanup.rs
    ;;
  tip) ;;
  *) echo "::error::unknown side $SIDE"; exit 1 ;;
esac
git log --oneline -1
git status --short
grep -c "cleanup_open_up_swapped" crates/crazyagent/src/team/cleanup.rs || true
grep -c "pub(crate) fn remove_tree" crates/crazyagent/src/team/cleanup.rs || true
grep -c "remove_agent_tree" crates/crazyagent/src/projects_home.rs || true
