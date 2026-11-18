#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  macos | linux | windows) ;;
  *) echo "::error::unknown os $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-native-seam-$os.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

case "$os" in
  macos)
    red_rows=(
      native_term::render::tests::each_platform_draws_with_its_own_backends
      native_term::render::tests::every_present_fallback_file_loads_its_faces
    )
    kept_rows=(
      native_term::render::tests::menlo_loads_from_its_system_file
      native_term::style::tests::a_style_checks_its_size_and_font
      native_term::style::tests::the_first_known_installed_family_is_used
      native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
    )
    ;;
  linux)
    red_rows=(
      native_term::render::tests::each_platform_draws_with_its_own_backends
      native_term::render::tests::every_present_fallback_file_loads_its_faces
      native_term::style::tests::on_linux_monospace_is_dejavu_sans_mono_else_the_built_in_font
    )
    kept_rows=(
      native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
      native_term::render::tests::the_built_in_font_gives_the_rig_xterm_grid
    )
    ;;
  windows)
    red_rows=()
    kept_rows=()
    ;;
esac

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os red $RED_SHA green $GREEN_SHA $(rustc -V)"

case "$os" in
  linux)
    log "== fonts present"
    ls /usr/share/fonts/truetype/dejavu /usr/share/fonts/truetype/noto /usr/share/fonts/opentype/noto 2>&1 \
      | grep -E 'DejaVuSans(Mono)?\.ttf|NotoColorEmoji|NotoSansCJK-Regular' | tee -a "$out"
    ;;
  windows)
    log "== fonts present"
    ls /c/Windows/Fonts | grep -iE '^(cascadia|consola|seguiemj|seguisym)' | tee -a "$out"
    ;;
esac

red_built=na
red_present=0
if [ "$os" != windows ]; then
  git checkout -q --detach "$RED_SHA"
  log "== red $RED_SHA: build the lib rows"
  cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
  red_built=$?
  tail -n 40 "$RUNNER_TEMP/red-build.txt" >> "$out"
  grep -E '^error(\[E[0-9]+\])?:' "$RUNNER_TEMP/red-build.txt" | sort | uniq -c | head -20 | tee -a "$out"
  : > "$RUNNER_TEMP/red-list.txt"
  if [ "$red_built" = 0 ]; then
    cargo test "${pkg[@]}" --lib -- --list > "$RUNNER_TEMP/red-list.txt" 2>&1
  fi
  for row in "${red_rows[@]}"; do
    if grep -qx "$row: test" "$RUNNER_TEMP/red-list.txt"; then
      red_present=$((red_present + 1))
      log "red row already present: $row"
    fi
  done
  log "red: build exit $red_built, red rows present $red_present of ${#red_rows[@]}"
  git checkout -q --detach "$GREEN_SHA"
fi

log "== green $GREEN_SHA: lib build"
cargo build "${pkg[@]}" --lib 2>&1 | tee -a "$out"
lib_built=${PIPESTATUS[0]}
log "== green: lib rows build"
cargo test "${pkg[@]}" --lib --no-run 2>&1 | tee -a "$out"
rows_built=${PIPESTATUS[0]}

rows_rc=na
want=0
passed=na
if [ "$os" != windows ] && [ "$rows_built" = 0 ]; then
  log "== green: native_term rows"
  cargo test "${pkg[@]}" --lib --no-fail-fast -- native_term 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  rows_rc=${PIPESTATUS[0]}
  passed=$(grep -cE '^test native_term::.* \.\.\. ok$' "$RUNNER_TEMP/rows.txt")
  failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$RUNNER_TEMP/rows.txt" | sort -u)
  [ -z "$failed" ] || { log "::error::rows failed:"; log "$failed"; want=1; }
  for row in "${red_rows[@]}" "${kept_rows[@]}"; do
    grep -qx "test $row ... ok" "$RUNNER_TEMP/rows.txt" || { log "::error::$row did not pass"; want=1; }
  done
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
fi

log "== green: clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings 2>&1 | tee -a "$out"
clip=${PIPESTATUS[0]}
log "== green: clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings 2>&1 | tee -a "$out"
clipt=${PIPESTATUS[0]}

red_ok=1
if [ "$os" != windows ]; then
  [ "$red_present" = 0 ] && red_ok=0
else
  red_ok=0
fi
{
  printf '| %s | exit |\n| --- | --- |\n' "$os"
  printf '| red build (macOS: fails, rows reference L0 items; Linux: builds without native_term) | %s |\n' "$red_built"
  printf '| red rows present at red tip (want 0) | %s of %s |\n' "$red_present" "${#red_rows[@]}"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$clip" = 0 ] && [ "$clipt" = 0 ] && { [ "$os" = windows ] || [ "$rows_rc" = 0 ]; }
