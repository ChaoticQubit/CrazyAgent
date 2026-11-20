set -uo pipefail
tip=fe10c45ce
rows=516488ebd
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
NEW="landing::windows_tests::each_refused_landing_function_refuses_by_name_on_windows landing::windows_tests::a_windows_engine_git_step_or_call_is_refused_and_starts_nothing landing::windows_tests::the_master_landing_path_on_windows_records_nothing landing::windows_tests::the_store_reads_of_one_change_set_answer_on_windows landing::windows_tests::a_bounded_read_on_windows_refuses_a_link_and_reads_a_plain_file"
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
  say "red: the W5a rows on the base, $rows"
  git checkout -q -f --detach "$rows" || return 0
  if cargo test -p crazyagent --lib --locked -- --list > "$out/list-rows.txt" 2> "$out/list-rows.err"; then
    say "rows commit built: $(grep -c '^landing::windows_tests::' "$out/list-rows.txt" || true) rows listed (want no build)"
    bad=1
  else
    say "rows commit lib tests did not build (red, as wanted): $(grep -m3 -E '^error' "$out/list-rows.err" | cut -c1-160 | tr '\n' ' ')"
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
