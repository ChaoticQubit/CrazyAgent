#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux | windows) ;;
  *) echo "::error::unknown os $os"; exit 2 ;;
esac
image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-native-windows-h2-compile-$os-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os image $image at $GREEN_SHA $(rustc -V) cores $(nproc 2>/dev/null || echo na)"

log "== lib rows build (compile only)"
cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/build.txt" 2>&1
built=$?
grep -E '^(error|warning)(\[|:)' -A12 "$RUNNER_TEMP/build.txt" | head -n 400 | tee -a "$out"
tail -n 5 "$RUNNER_TEMP/build.txt" | tee -a "$out"
{
  printf '| %s %s | exit |\n| --- | --- |\n' "$os" "$image"
  printf '| lib rows build at %s | %s |\n' "$GREEN_SHA" "$built"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"
[ "$built" = 0 ]
