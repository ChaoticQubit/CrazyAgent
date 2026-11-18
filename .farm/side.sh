set -euo pipefail
case "$SIDE" in
  base)
    git checkout "$PRE_FIX_CLEANUP" -- crates/crazyagent/src/team/cleanup.rs
    git checkout "$PRE_FIX_TOOLCHAINS" -- crates/crazyagent/src/sandbox/toolchains.rs
    ;;
  mutant)
    ruby .farm/mutant.rb crates/crazyagent/src/team/cleanup.rs
    ;;
  mutant-old)
    git checkout "$OLD_SWAP_TESTS" -- crates/crazyagent/src/team/cleanup/tests.rs
    ruby .farm/mutant.rb crates/crazyagent/src/team/cleanup.rs
    ;;
  tip) ;;
  *) echo "::error::unknown side $SIDE"; exit 1 ;;
esac
git log --oneline -1
git status --short
grep -c "Mode::RUSR | Mode::XUSR" crates/crazyagent/src/team/cleanup.rs || true
grep -c "agent_tree_gone" crates/crazyagent/src/sandbox/toolchains.rs || true
grep -c "from_mode(0o555)" crates/crazyagent/src/team/cleanup/tests.rs || true
grep -c "AtFlags::empty()" crates/crazyagent/src/team/cleanup.rs || true
