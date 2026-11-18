set -euo pipefail
if [ "$SIDE" = base ]; then
  git checkout "$PRE_FIX_CLEANUP" -- crates/crazyagent/src/team/cleanup.rs
  git checkout "$PRE_FIX_TOOLCHAINS" -- crates/crazyagent/src/sandbox/toolchains.rs
fi
git log --oneline -1
git status --short
grep -c "Mode::RUSR | Mode::XUSR" crates/crazyagent/src/team/cleanup.rs || true
grep -c "agent_tree_gone" crates/crazyagent/src/sandbox/toolchains.rs || true
