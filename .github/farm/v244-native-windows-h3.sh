#!/usr/bin/env bash
set +e -uo pipefail

[ "${1:-}" = windows ] || { echo "::error::this round runs on Windows only"; exit 2; }
image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-native-windows-h3-windows-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
wall_row=native_term::windows::rows::an_agent_container_reaches_the_view_and_reads_nothing_of_it

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os windows image $image green $GREEN_SHA $(rustc -V) cores $(nproc 2>/dev/null || echo na)"

log "== the boundary helper beside the lib rows, as an agent launch needs it"
cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/helper.txt" 2>&1
helper_rc=$?
tail -n 5 "$RUNNER_TEMP/helper.txt" | tee -a "$out"
if [ "$helper_rc" = 0 ]; then
  mkdir -p target/debug/deps
  cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
  helper_rc=$?
fi

log "== lib rows build"
cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/build.txt" 2>&1
rows_built=$?
tail -n 5 "$RUNNER_TEMP/build.txt" | tee -a "$out"

want=0
wall_rc=na
lookups=na
patterns=na
exits=na
container_read=na
if [ "$rows_built" = 0 ] && [ "$helper_rc" = 0 ]; then
  log "== the H3 wall row: an agent container reaches the view and reads nothing"
  timeout 600 cargo test "${pkg[@]}" --lib -- --ignored --exact "$wall_row" --test-threads=1 --show-output \
    > "$RUNNER_TEMP/wall.txt" 2>&1
  wall_rc=$?
  grep -E '^h3 row:|^test result' "$RUNNER_TEMP/wall.txt" | tee -a "$out"
  grep -E -A3 'panicked' "$RUNNER_TEMP/wall.txt" | head -n 40 | tee -a "$out"
  readers=$(grep -E '^h3 row: agent container reader (PowerShell|Compiled): ' "$RUNNER_TEMP/wall.txt" || true)
  lookups=$(printf '%s\n' "$readers" | grep -Fc 'reader_stage=lookup' || true)
  patterns=$(printf '%s\n' "$readers" | grep -Fc 'reader_text_pattern=False' || true)
  exits=$(grep -E '^h3 row: (screen reader text|agent container reader (PowerShell|Compiled)): ' "$RUNNER_TEMP/wall.txt" | grep -Fc ' rc Some(0) ' || true)
  container_read=0
  grep -Fq "an agent's AppContainer read the view" "$RUNNER_TEMP/wall.txt" && container_read=1
  [ "$lookups" = 2 ] || { log "::error::$lookups of 2 container readers reached their UIA lookup"; want=1; }
  [ "$patterns" = 2 ] || { log "::error::$patterns of 2 container readers printed no Text pattern"; want=1; }
  [ "$exits" = 3 ] || { log "::error::$exits of 3 readers exited 0"; want=1; }
  [ "$container_read" = 0 ] || { log "::error::an agent AppContainer reader read the view"; want=1; }
  grep -Eq "^test $wall_row \.\.\. ok" "$RUNNER_TEMP/wall.txt" || { log "::error::the wall row did not pass"; want=1; }
fi

log "== clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings > "$RUNNER_TEMP/clippy.txt" 2>&1
clipt=$?
grep -E '^(warning|error)' "$RUNNER_TEMP/clippy.txt" | head -n 40 | tee -a "$out"
tail -n 3 "$RUNNER_TEMP/clippy.txt" | tee -a "$out"

{
  printf '| windows %s | exit |\n| --- | --- |\n' "$image"
  printf '| boundary helper, lib rows build | %s, %s |\n' "$helper_rc" "$rows_built"
  printf '| wall row (want 0) | %s |\n' "$wall_rc"
  printf '| container lookups, no Text pattern printed, readers exited 0 (want 2, 2, 3) | %s, %s, %s |\n' \
    "$lookups" "$patterns" "$exits"
  printf '| container read the view (want 0), checks ok (0 = yes) | %s, %s |\n' "$container_read" "$want"
  printf '| clippy --tests -D warnings | %s |\n' "$clipt"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$helper_rc" = 0 ] && [ "$rows_built" = 0 ] && [ "$wall_rc" = 0 ] && [ "$want" = 0 ] && [ "$clipt" = 0 ]
