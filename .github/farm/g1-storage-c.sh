set -uo pipefail
mode=$1
red=ea9048e77
out="$RUNNER_TEMP/g1-storage-c"
mkdir -p "$out"
bad=0
echo "tip $(git rev-parse --short=9 HEAD), red $red, mode $mode"

run() {
  label=$1
  shift
  echo "== $label: $*"
  "$@" > "$out/$label.log" 2>&1
  code=$?
  grep -E '^test .* (ok|FAILED)$|^test result:|^error' "$out/$label.log" | tail -40
  return $code
}

failed_names() {
  grep -E '^test .* FAILED$' "$out/$1.log" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/'
}

if [ "$mode" = engine ]; then
  pkg=crazyagent
  fix_files="crates/crazyagent/src/engine/owner/transport/ide_tab.rs crates/crazyagent/src/engine/owner/transport/process.rs crates/crazyagent/src/engine/owner/transport/master_resolve.rs crates/crazyagent/src/config.rs crates/crazyagent/src/tui/text.rs"
  rows="engine::owner::transport::ide_tab::tests::a_storage_answer_reaches_the_ide_tab_wire_by_name engine::owner::transport::process::tests::a_storage_answer_reaches_the_process_wire_by_name"
  suites="config:: tui::text:: engine::owner::transport::ide_tab:: engine::owner::transport::process::tests:: storage::owner_actor_tests::only_a_change"
else
  pkg=crazyagent-desktop
  fix_files=$(git diff --name-only "$red" HEAD -- apps/desktop/src-tauri/src/bridge)
  rows="bridge::tests::every_route_names_each_storage_answer_instead_of_its_unknown_outcome"
  suites="bridge::"
fi
echo "fix files:" $fix_files

echo "== GREEN at the tip"
cargo test -p "$pkg" --lib --locked --no-run > "$out/build-green.log" 2>&1 || { tail -60 "$out/build-green.log"; echo "::error::the tip does not build"; exit 1; }
for row in $rows; do
  for i in 1 2 3; do
    run "green-$i-${row##*::}" cargo test -p "$pkg" --lib --locked -- --exact "$row" || { echo "::error::$row failed at the tip, try $i"; bad=1; }
  done
done
run green-suites cargo test -p "$pkg" --lib --locked -- $suites
suite_code=$?
suite_failed=$(failed_names green-suites)
grep -E '^test result:' "$out/green-suites.log" | tail -1

echo "== RED: the fix files at $red"
git checkout "$red" -- $fix_files
cargo test -p "$pkg" --lib --locked --no-run > "$out/build-red.log" 2>&1 || { tail -60 "$out/build-red.log"; echo "::error::the red files do not build"; exit 1; }
for row in $rows; do
  if run "red-${row##*::}" cargo test -p "$pkg" --lib --locked -- --exact "$row"; then
    echo "::error::$row passed without the fix"
    bad=1
  elif grep -qE "^test $row \.\.\. FAILED$" "$out/red-${row##*::}.log"; then
    echo "RED as expected at $red: $row"
  else
    echo "::error::$row did not run at $red"
    bad=1
  fi
done
if [ "$suite_code" -ne 0 ]; then
  echo "== ATTRIBUTE: the suite reds rerun on the fix files at $red"
  [ -n "$suite_failed" ] || { echo "::error::the suites failed with no failed row named"; bad=1; }
  for name in $suite_failed; do
    case " $rows " in *" $name "*) continue ;; esac
    if run "attr-${name##*::}" cargo test -p "$pkg" --lib --locked -- --exact "$name"; then
      echo "::error::NEW red from the fix: $name"
      bad=1
    else
      echo "OLD red, also without the fix: $name"
    fi
  done
fi
git checkout HEAD -- $fix_files
git diff --quiet || { echo "::error::the tree is not clean after the red phase"; bad=1; }

echo "== CLIPPY at the tip"
cargo clippy -p "$pkg" --lib --locked -- -D warnings > "$out/clippy-lib.log" 2>&1 || { tail -40 "$out/clippy-lib.log"; echo "::error::clippy on the lib"; bad=1; }
cargo clippy -p "$pkg" --lib --profile test --locked -- -D warnings > "$out/clippy-test.log" 2>&1 || { tail -40 "$out/clippy-test.log"; echo "::error::clippy on the test build"; bad=1; }
exit $bad
