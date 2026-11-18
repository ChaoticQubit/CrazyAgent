#!/usr/bin/env bash
set +e -uo pipefail

image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-native-windows-h4-diag-windows-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
clip_row=native_term::windows::clip::tests::the_desktop_clipboard_round_trips_refuses_while_held_and_bounds_every_block

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os windows image $image at $GREEN_SHA $(rustc -V) cores $(nproc 2>/dev/null || echo na)"

log "== probe: raw CF_UNICODETEXT blocks written and read back through pwsh"
cp .github/farm/v244-native-windows-h4-probe.ps1 "$RUNNER_TEMP/probe.ps1"
timeout 300 pwsh -NoProfile -NonInteractive -File "$RUNNER_TEMP/probe.ps1" 2>&1 | tee -a "$out"
probe_rc=${PIPESTATUS[0]}

log "== lib rows build"
cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/build.txt" 2>&1
built=$?
tail -n 5 "$RUNNER_TEMP/build.txt" | tee -a "$out"

row_rc=na
if [ "$built" = 0 ]; then
  log "== the clip desktop row, full output"
  timeout 600 cargo test "${pkg[@]}" --lib -- --ignored --exact "$clip_row" --test-threads=1 --show-output \
    2>&1 | tee -a "$out"
  row_rc=${PIPESTATUS[0]}
fi

{
  printf '| windows %s | exit |\n| --- | --- |\n' "$image"
  printf '| probe | %s |\n| lib rows build | %s |\n| clip row (diagnostic) | %s |\n' "$probe_rc" "$built" "$row_rc"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$probe_rc" = 0 ] && [ "$built" = 0 ]
