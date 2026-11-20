set -uo pipefail
tip=31845c439
base=de467713d
os=${FARM_OS:?FARM_OS is windows}
out="$RUNNER_TEMP/w5a"
mkdir -p "$out"
bad=0
short() { git rev-parse --short=9 "$1"; }
say() { printf '%s\n' "$*"; }
code=$(git log -1 --format=%H HEAD -- . ':!.github')
if [ "$(short "$code")" != "$tip" ] || ! git merge-base --is-ancestor "$base" "$tip"; then
  say "chain NOT: code $(short "$code"), want $tip on $base"
  exit 1
fi
git checkout -q -f --detach "$tip" || exit 1
say "tree $(short HEAD) on $base; git $(git --version); runner ${ImageOS:-unknown} ${ImageVersion:-unknown}"
NEW="landing::windows_tests::each_refused_landing_function_refuses_by_name_on_windows landing::windows_tests::a_windows_confined_or_reading_step_is_refused_until_its_container_exists landing::windows_tests::the_master_landing_path_on_windows_starts_no_git_and_records_nothing landing::windows_tests::the_store_only_landing_functions_answer_on_windows"
clippy() {
  local kind log rc
  for kind in --lib --tests; do
    log="$out/clippy-crazyagent$kind.log"
    cargo clippy -p crazyagent "$kind" --locked --keep-going -- -D warnings > "$log" 2>&1
    rc=$?
    say "clippy crazyagent $kind rc=$rc, $(grep -cE '^(warning|error)' "$log" || true) warning/error lines"
    [ "$rc" -eq 0 ] || { grep -E -A7 '^(warning|error)' "$log" | grep -vE '^\s*\|?\s*$' | head -400 | cut -c1-220; bad=1; }
  done
}
listed() {
  local log="$out/list.txt"
  cargo test -p crazyagent --lib --locked -- --list > "$log" 2> "$out/list.err" || { tail -40 "$out/list.err" | cut -c1-300; return 1; }
  say "crazyagent lib rows: $(grep -c ': test$' "$log")"
}
lib_rows() {
  local prefixes=$1 log="$out/rows.log" p n names="$out/sel.txt" rc
  : > "$names"
  for p in $prefixes; do
    n=$(grep -cE "^${p}[^ ]*: test$" "$out/list.txt" || true)
    say "rows $p: $n"
    [ "$n" -gt 0 ] || { say "ZERO ROWS $p"; bad=1; }
    grep -E "^${p}[^ ]*: test$" "$out/list.txt" | sed 's/: test$//' >> "$names" || true
  done
  sort -u -o "$names" "$names"
  cargo test -p crazyagent --lib --locked -- $prefixes > "$log" 2>&1
  rc=$?
  say "lib rows rc=$rc | $(grep '^test result' "$log" | tail -1 | cut -c1-120)"
  grep -E '^test .* \.\.\. FAILED$' "$log" | sed -E 's/^test (.*) \.\.\. FAILED$/\1/' > "$out/failed.txt" || true
  grep -E '^test .* \.\.\. ok$' "$log" | sed -E 's/^test (.*) \.\.\. ok$/\1/' > "$out/passed.txt" || true
  say "lib: $(wc -l < "$out/passed.txt" | tr -d ' ') ok, $(wc -l < "$out/failed.txt" | tr -d ' ') FAILED of $(wc -l < "$names" | tr -d ' ') listed"
  sed 's/^/FAILED /' "$out/failed.txt" | head -40
  grep -A8 'panicked at' "$log" | head -80 | cut -c1-400
  [ "$rc" -eq 0 ] || bad=1
}
need() {
  local file=$1 row
  shift
  for row in "$@"; do
    grep -qE "^test ${row}( - should panic)? \.\.\. ok$" "$file" || { say "ROW NOT OK: $row"; bad=1; }
  done
}
red_at_base() {
  say "red: the W5a rows at the base $base"
  git checkout -q -f --detach "$base" || return 0
  if cargo test -p crazyagent --lib --locked -- --list > "$out/list-base.txt" 2>/dev/null; then
    say "base rows named landing::windows_tests: $(grep -c '^landing::windows_tests::' "$out/list-base.txt" || true) (want 0)"
  else
    say "base list did not build"
  fi
  git checkout -q -f --detach "$tip"
}
case "$os" in
  windows)
    clippy
    if listed; then
      lib_rows "landing::"
      need "$out/rows.log" $NEW
    else
      bad=1
    fi
    red_at_base
    ;;
  *) echo "bad FARM_OS $os"; exit 2 ;;
esac
[ "$bad" -eq 0 ] && say "W5a $os ok at $tip"
exit "$bad"
