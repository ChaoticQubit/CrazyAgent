#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  macos | linux | windows) ;;
  *) echo "::error::unknown os $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-native-seam-r2-$os.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

style_row=native_term::render::tests::every_style_finds_its_own_face_with_no_system_font
case "$os" in
  macos)
    kept_rows=(
      native_term::render::tests::each_platform_draws_with_its_own_backends
      native_term::render::tests::every_present_fallback_file_loads_its_faces
      native_term::render::tests::menlo_loads_from_its_system_file
      native_term::style::tests::a_style_checks_its_size_and_font
      native_term::style::tests::the_first_known_installed_family_is_used
      native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
    )
    ;;
  linux)
    kept_rows=(
      native_term::render::tests::each_platform_draws_with_its_own_backends
      native_term::render::tests::every_present_fallback_file_loads_its_faces
      native_term::style::tests::on_linux_monospace_is_dejavu_sans_mono_else_the_built_in_font
      native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
      native_term::render::tests::the_built_in_font_gives_the_rig_xterm_grid
    )
    ;;
  windows)
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
      | grep -E 'DejaVuSans(Mono)?(-[A-Za-z]+)?\.ttf|NotoColorEmoji|NotoSansCJK-Regular' | tee -a "$out"
    ;;
  windows)
    log "== fonts present"
    ls /c/Windows/Fonts | grep -iE '^(cascadia|consola|seguiemj|seguisym)' | tee -a "$out"
    ;;
esac

red_built=na
red_failed=na
if [ "$os" != windows ]; then
  git checkout -q --detach "$RED_SHA"
  log "== red $RED_SHA: build the lib rows"
  cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
  red_built=$?
  tail -n 20 "$RUNNER_TEMP/red-build.txt" >> "$out"
  red_failed=1
  if [ "$red_built" = 0 ]; then
    log "== red $RED_SHA: $style_row must fail by its assertion"
    cargo test "${pkg[@]}" --lib -- --exact "$style_row" > "$RUNNER_TEMP/red-run.txt" 2>&1
    red_rc=$?
    tail -n 20 "$RUNNER_TEMP/red-run.txt" >> "$out"
    if [ "$red_rc" = 101 ] && grep -qxF "test $style_row ... FAILED" "$RUNNER_TEMP/red-run.txt" \
      && grep -q "span has a face" "$RUNNER_TEMP/red-run.txt"; then
      red_failed=0
    fi
    log "red: run exit $red_rc, failed by its assertion (0 = yes) $red_failed"
  fi
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
if [ "$rows_built" = 0 ]; then
  if [ "$os" = windows ]; then
    log "== green: $style_row"
    cargo test "${pkg[@]}" --lib -- --exact "$style_row" 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  else
    log "== green: native_term rows"
    cargo test "${pkg[@]}" --lib --no-fail-fast -- native_term 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  fi
  rows_rc=${PIPESTATUS[0]}
  passed=$(grep -cE '^test native_term::.* \.\.\. ok$' "$RUNNER_TEMP/rows.txt")
  failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$RUNNER_TEMP/rows.txt" | sort -u)
  [ -z "$failed" ] || { log "::error::rows failed:"; log "$failed"; want=1; }
  for row in "$style_row" "${kept_rows[@]}"; do
    grep -qx "test $row ... ok" "$RUNNER_TEMP/rows.txt" || { log "::error::$row did not pass"; want=1; }
  done
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
else
  want=1
fi

log "== green: clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings 2>&1 | tee -a "$out"
clip=${PIPESTATUS[0]}
log "== green: clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings 2>&1 | tee -a "$out"
clipt=${PIPESTATUS[0]}

red_ok=1
if [ "$os" = windows ]; then
  red_ok=0
elif [ "$red_built" = 0 ] && [ "$red_failed" = 0 ]; then
  red_ok=0
fi
{
  printf '| %s | exit |\n| --- | --- |\n' "$os"
  printf '| red %s: lib rows build (want 0; Windows runs no red) | %s |\n' "$RED_SHA" "$red_built"
  printf '| red %s: the style row fails by its assertion (0 = yes) | %s |\n' "$RED_SHA" "$red_failed"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| green rows (native_term; Windows: the style row) | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
