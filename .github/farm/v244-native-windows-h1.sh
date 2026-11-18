#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux | windows) ;;
  *) echo "::error::unknown os $os"; exit 2 ;;
esac
image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-native-windows-h1-$os-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
display=native_term::windows::rows::a_view_sits_over_the_webview_draws_a_fed_screen_hides_and_drops_its_surface_first
pure_rows=(
  native_term::windows::place::tests::the_dpi_gives_a_scale_key_held_to_one_to_four_times
  native_term::windows::place::tests::a_page_rect_is_finite_bounded_and_at_least_one_device_pixel
  native_term::windows::place::tests::fractional_scales_round_edges_so_touching_boxes_touch
  native_term::windows::place::tests::the_webview_offset_moves_the_rect_and_is_bounded
  native_term::windows::place::tests::a_scale_key_out_of_range_is_a_bug
  native_term::windows::board::tests::a_fresh_view_is_handed_over_once_with_its_handle_and_geometry
  native_term::windows::board::tests::a_view_draws_only_shown_with_a_frame_and_a_change
  native_term::windows::board::tests::the_newest_state_wins
  native_term::windows::board::tests::a_frame_posted_during_the_swap_chain_wait_lands_in_that_frame
  native_term::windows::board::tests::a_geometry_change_alone_redraws_the_last_frame
  native_term::windows::board::tests::remove_waits_for_the_render_side_drop_within_its_bound
  native_term::windows::board::tests::a_view_never_handed_over_is_removed_at_once
  native_term::windows::board::tests::views_are_bounded
  native_term::windows::board::tests::adding_an_id_twice_is_a_bug
)

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os image $image red $RED_SHA green $GREEN_SHA $(rustc -V) cores $(nproc 2>/dev/null || echo na)"

red_built=na
red_rc=na
red_failed=na
if [ "$os" = windows ]; then
  git checkout -q --detach "$RED_SHA"
  log "== red $RED_SHA: build the lib rows"
  cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
  red_built=$?
  tail -n 30 "$RUNNER_TEMP/red-build.txt" >> "$out"
  if [ "$red_built" = 0 ]; then
    log "== red: the display row (want FAILED)"
    timeout 600 cargo test "${pkg[@]}" --lib -- --ignored --exact "$display" --test-threads=1 \
      > "$RUNNER_TEMP/red-row.txt" 2>&1
    red_rc=$?
    grep -E "^test $display|panicked|^test result" "$RUNNER_TEMP/red-row.txt" | tee -a "$out"
    grep -Eq "^test $display \.\.\. FAILED" "$RUNNER_TEMP/red-row.txt" && red_failed=1 || red_failed=0
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
passed=na
want=0
display_rc=na
if [ "$rows_built" = 0 ]; then
  log "== green: native_term rows (Windows: the windows module only, as L0 runs none there)"
  filter=native_term
  [ "$os" = windows ] && filter=native_term::windows
  cargo test "${pkg[@]}" --lib --no-fail-fast -- "$filter" 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  rows_rc=${PIPESTATUS[0]}
  passed=$(grep -cE '^test native_term::.* \.\.\. ok' "$RUNNER_TEMP/rows.txt")
  failed=$(awk '/^test .* \.\.\. FAILED/ {print $2}' "$RUNNER_TEMP/rows.txt" | sort -u)
  [ -z "$failed" ] || { log "::error::rows failed:"; log "$failed"; want=1; }
  if [ "$os" = windows ]; then
    for row in "${pure_rows[@]}"; do
      grep -Eq "^test $row( - should panic)? \.\.\. ok" "$RUNNER_TEMP/rows.txt" || { log "::error::$row did not pass"; want=1; }
    done
    log "== green: the display row"
    timeout 600 cargo test "${pkg[@]}" --lib -- --ignored --exact "$display" --test-threads=1 --show-output \
      2>&1 | tee "$RUNNER_TEMP/display.txt" >> "$out"
    display_rc=${PIPESTATUS[0]}
    grep -Eq "^test $display \.\.\. ok" "$RUNNER_TEMP/display.txt" || { log "::error::the display row did not pass"; want=1; }
    grep -E '^h1 row:' "$RUNNER_TEMP/display.txt" | tee -a "$out"
  fi
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
fi

log "== green: clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings 2>&1 | tee -a "$out"
clip=${PIPESTATUS[0]}
log "== green: clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings 2>&1 | tee -a "$out"
clipt=${PIPESTATUS[0]}

red_ok=0
if [ "$os" = windows ]; then
  { [ "$red_built" = 0 ] && [ "$red_failed" = 1 ]; } || red_ok=1
fi
{
  printf '| %s %s | exit |\n| --- | --- |\n' "$os" "$image"
  printf '| red build (Windows only) | %s |\n' "$red_built"
  printf '| red display row rc, FAILED seen (want non-zero, 1) | %s, %s |\n' "$red_rc" "$red_failed"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| display row (Windows only) | %s |\n' "$display_rc"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$rows_rc" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
