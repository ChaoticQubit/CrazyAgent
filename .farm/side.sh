set -euo pipefail
probe="$RUNNER_TEMP/rename-probe"
rm -rf "$probe"
mkdir -p "$probe/shut" "$probe/open"
chmod 000 "$probe/shut"
if mv "$probe/shut" "$probe/shut-aside" 2>/dev/null; then echo "PROBE rename of a 000 folder: ok"; else echo "PROBE rename of a 000 folder: refused"; fi
if mv "$probe/open" "$probe/open-aside" 2>/dev/null; then echo "PROBE rename of a 0755 folder: ok"; else echo "PROBE rename of a 0755 folder: refused"; fi
chmod -R 700 "$probe"
rm -rf "$probe"
tests=crates/crazyagent/src/team/cleanup/tests.rs
case "$SIDE" in
  base)
    git checkout "$RED_COMMIT" -- crates/crazyagent/src/team/cleanup.rs crates/crazyagent/src/projects_home.rs
    test "$(grep -c 'writable(&tree, Acls::Stuck);' "$tests")" = 1
    sed -i '' 's/writable(&tree, Acls::Stuck);/writable(\&tree, Flags::Kept, Acls::Stuck);/' "$tests"
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
grep -c "from_mode(0o000)).unwrap();" "$tests" || true
