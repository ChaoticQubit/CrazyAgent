set -uo pipefail
out="$RUNNER_TEMP/g1-routes-desktop"
mkdir -p "$out"
bad=0
pkg=crazyagent-desktop
row=bridge::tests::every_route_names_each_storage_answer_instead_of_its_unknown_outcome
echo "tip $(git rev-parse --short=9 HEAD)"

run() {
  label=$1
  shift
  echo "== $label: $*"
  "$@" > "$out/$label.log" 2>&1
  code=$?
  grep -E '^test .* (ok|FAILED)$|^test result:|^error' "$out/$label.log" | tail -40
  return $code
}

echo "== GREEN at the tip"
cargo test -p "$pkg" --lib --locked --no-run > "$out/build-green.log" 2>&1 || { tail -60 "$out/build-green.log"; echo "::error::the tip does not build"; exit 1; }
for i in 1 2 3; do
  run "green-row-$i" cargo test -p "$pkg" --lib --locked -- --exact "$row" || { echo "::error::the route row failed at the tip, try $i"; bad=1; }
  grep -qE "^test $row \.\.\. ok$" "$out/green-row-$i.log" || { echo "::error::the route row did not pass at the tip, try $i"; bad=1; }
done
run green-bridge cargo test -p "$pkg" --lib --locked -- bridge::
bridge_code=$?
grep -E '^test .* FAILED$' "$out/green-bridge.log" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u > "$out/green-bridge-failed.txt"

echo "== MUTANT: kept_code without the storage answers"
perl -0pi -e 's/\n\s*\.chain\(crazyagent::storage::STORAGE_ANSWERS\.iter\(\)\.copied\(\)\)//' apps/desktop/src-tauri/src/bridge.rs
[ "$(git diff --name-only | wc -l)" -eq 1 ] || { echo "::error::the mutation did not apply"; exit 1; }
git diff | cat
cargo test -p "$pkg" --lib --locked --no-run > "$out/build-mutant.log" 2>&1 || { tail -60 "$out/build-mutant.log"; echo "::error::the mutant does not build"; exit 1; }
if run mutant-row cargo test -p "$pkg" --lib --locked -- --exact "$row"; then
  echo "::error::the route row passed against the mutant"
  bad=1
elif grep -qE "^test $row \.\.\. FAILED$" "$out/mutant-row.log"; then
  echo "RED as expected against the mutant: $row"
else
  echo "::error::the route row did not run against the mutant"
  bad=1
fi
if [ "$bridge_code" -ne 0 ]; then
  echo "== ATTRIBUTE: bridge suite reds rerun at the merge base 6bd700abd bridge files"
  git checkout HEAD -- apps/desktop/src-tauri/src/bridge.rs
  echo "bridge suite failed rows:"
  cat "$out/green-bridge-failed.txt"
  [ -s "$out/green-bridge-failed.txt" ] || { echo "::error::the bridge suite failed with no failed row named"; bad=1; }
  base_files=$(git diff --name-only 6bd700abd HEAD -- apps/desktop/src-tauri/src crates/crazyagent/src | tr '\n' ' ')
  git checkout 6bd700abd -- $base_files
  cargo test -p "$pkg" --lib --locked --no-run > "$out/build-base.log" 2>&1 || { tail -40 "$out/build-base.log"; echo "::error::the base files do not build"; bad=1; }
  while read -r name; do
    [ "$name" = "$row" ] && continue
    if run "base-${name##*::}" cargo test -p "$pkg" --lib --locked -- --exact "$name"; then
      if grep -qE "^test $name \.\.\. ok$" "$out/base-${name##*::}.log"; then
        echo "::error::NEW red, green at the base: $name"
      else
        echo "::error::NEW red, no such row at the base: $name"
      fi
      bad=1
    else
      echo "OLD red, also red at the base: $name"
    fi
  done < "$out/green-bridge-failed.txt"
fi
git checkout HEAD -- apps crates
git diff --quiet || { echo "::error::the tree is not clean"; bad=1; }

echo "== CLIPPY at the tip"
cargo clippy -p "$pkg" --lib --locked -- -D warnings > "$out/clippy-lib.log" 2>&1 || { tail -40 "$out/clippy-lib.log"; echo "::error::clippy on the desktop lib"; bad=1; }
cargo clippy -p "$pkg" --lib --profile test --locked -- -D warnings > "$out/clippy-test.log" 2>&1 || { tail -40 "$out/clippy-test.log"; echo "::error::clippy on the desktop test build"; bad=1; }
exit $bad
