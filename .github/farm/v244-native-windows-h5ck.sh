#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux | windows) ;;
  *) echo "::error::unknown os $os"; exit 2 ;;
esac
image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-native-windows-h5ck-$os-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
copy_row=native_term::host::tests::the_copy_chord_with_a_selection_copies_and_sends_no_interrupt
plain_row=native_term::host::tests::the_copy_chord_with_no_selection_sends_the_interrupt

red_built=na
red_rc=na
red_failed=na
if [ "$os" = windows ]; then
  git checkout -q --detach "$RED_SHA"
  log "== red $RED_SHA: build the lib rows"
  cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
  red_built=$?
  tail -n 40 "$RUNNER_TEMP/red-build.txt" | tee -a "$out"
  if [ "$red_built" = 0 ]; then
    log "== red: the copy chord with a selection (want FAILED in host.rs)"
    cargo test "${pkg[@]}" --lib -- --exact "$copy_row" > "$RUNNER_TEMP/red-copy.txt" 2>&1
    red_rc=$?
    grep -E '^test |^test result|panicked' "$RUNNER_TEMP/red-copy.txt" | tee -a "$out"
    red_failed=1
    grep -Eq "^test $copy_row \.\.\. FAILED" "$RUNNER_TEMP/red-copy.txt" || red_failed=0
    grep -Eq 'panicked at .*host\.rs' "$RUNNER_TEMP/red-copy.txt" || red_failed=0
  fi
  git checkout -q --detach "$GREEN_SHA"
fi

log "== green $GREEN_SHA: lib build"
cargo build "${pkg[@]}" --lib > "$RUNNER_TEMP/lib-build.txt" 2>&1
lib_built=$?
tail -n 40 "$RUNNER_TEMP/lib-build.txt" | tee -a "$out"

rows_rc=na
if [ "$os" = windows ]; then
  log "== green: both copy chord rows"
  cargo test "${pkg[@]}" --lib -- native_term::host::tests > "$RUNNER_TEMP/rows.txt" 2>&1
  rows_rc=$?
  grep -E '^test |^test result|panicked' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
  grep -Eq "^test $copy_row \.\.\. ok" "$RUNNER_TEMP/rows.txt" || rows_rc=9
  grep -Eq "^test $plain_row \.\.\. ok" "$RUNNER_TEMP/rows.txt" || rows_rc=9
fi

log "== green: clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings > "$RUNNER_TEMP/clippy-lib.txt" 2>&1
clip=$?
grep -E -A12 '^(warning|error)' "$RUNNER_TEMP/clippy-lib.txt" | head -n 400 | tee -a "$out"
log "== green: clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings > "$RUNNER_TEMP/clippy-tests.txt" 2>&1
clipt=$?
grep -E -A12 '^(warning|error)' "$RUNNER_TEMP/clippy-tests.txt" | head -n 400 | tee -a "$out"

{
  printf '| %s %s | exit |\n| --- | --- |\n' "$os" "$image"
  printf '| red build, red copy row rc, failed in host.rs (Windows only, want 0, non-zero, 1) | %s, %s, %s |\n' \
    "$red_built" "$red_rc" "$red_failed"
  printf '| green lib build | %s |\n| both copy chord rows (Windows only) | %s |\n' "$lib_built" "$rows_rc"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

red_ok=0
if [ "$os" = windows ]; then
  { [ "$red_built" = 0 ] && [ "$red_failed" = 1 ]; } || red_ok=1
  [ "$rows_rc" = 0 ] || red_ok=1
fi
[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
