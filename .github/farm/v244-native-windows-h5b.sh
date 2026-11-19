#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux | windows) ;;
  *) echo "::error::unknown os $os"; exit 2 ;;
esac
image="${ImageOS:-unknown}"
out="$RUNNER_TEMP/v244-native-windows-h5b-$os-$image.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }
view_row=native_term::windows::rows::a_view_sits_over_the_webview_draws_a_fed_screen_hides_and_drops_its_surface_first
keys_row=native_term::windows::rows::each_key_reaches_the_sink_once_as_the_panel_sends_it_and_nothing_while_inactive
clip_row=native_term::windows::clip::tests::the_desktop_clipboard_round_trips_refuses_while_held_and_bounds_every_block
uia_row=native_term::windows::rows::a_screen_reader_reads_lines_the_selection_bounds_and_events
wall_row=native_term::windows::rows::an_agent_container_reaches_the_view_and_reads_nothing_of_it
empty_row=native_term::windows::uia::tests::an_empty_range_reads_an_allocated_empty_string_never_a_null_one
ime_row=native_term::windows::rows::the_ime_marks_inline_commits_once_refuses_bad_answers_and_nothing_while_inactive
fault_row=native_term::windows::rows::a_gpu_refusal_or_loss_faults_a_view_for_good_and_a_minimized_parent_does_not
wait_row=native_term::windows::rows::a_failed_wait_faults_its_own_view_never_another_at_its_scale
kept_row=native_term::windows::draw::tests::an_unused_scale_keeps_its_shared_state
spent_row=native_term::windows::draw::tests::a_surface_that_fails_before_its_device_request_spends_no_attempt
h5a_rows=(
  native_term::windows::board::tests::a_faulted_view_is_never_drawn_again_and_its_removal_still_drops_it
  native_term::windows::board::tests::faulting_a_view_off_the_board_is_a_bug
  native_term::windows::board::tests::faulting_a_view_before_the_hand_over_is_a_bug
  native_term::windows::board::tests::a_zero_width_is_a_bug
  native_term::windows::board::tests::a_zero_height_is_a_bug
  native_term::windows::board::tests::an_unknown_scale_key_is_a_bug
  native_term::windows::draw::tests::a_ready_view_with_no_frame_is_a_bug
  native_term::windows::draw::tests::the_newest_frame_is_drawn_and_kept_for_a_redraw
  native_term::windows::draw::tests::a_scale_replaces_a_lost_device_at_most_twice_a_run
  native_term::windows::draw::tests::a_loss_on_an_unknown_scale_is_a_bug
  native_term::windows::draw::tests::drawing_a_view_the_render_side_never_made_is_a_bug
  native_term::windows::draw::tests::drawing_a_view_with_no_surface_the_board_never_faulted_is_a_bug
  native_term::windows::draw::tests::a_view_retired_earlier_in_its_round_is_not_drawn
  native_term::windows::draw::tests::an_unused_scale_keeps_its_shared_state
  native_term::windows::draw::tests::a_surface_that_fails_before_its_device_request_spends_no_attempt
  native_term::windows::draw::tests::refitting_a_view_with_no_shared_state_is_a_bug
  native_term::windows::draw::tests::refitting_a_faulted_view_is_a_bug
  native_term::windows::draw::tests::presenting_a_view_with_no_shared_state_is_a_bug
  native_term::windows::draw::tests::presenting_a_faulted_view_is_a_bug
  native_term::windows::draw::tests::making_a_view_twice_is_a_bug
  native_term::windows::draw::tests::a_view_past_the_bound_is_a_bug_to_make
  native_term::windows::draw::tests::posting_the_cell_of_a_scale_with_no_shared_state_is_a_bug
  native_term::windows::draw::tests::posting_the_cell_of_a_view_off_the_board_is_a_bug
)
compose_rows=(
  native_term::ime::compose::tests::composition_flags_ask_for_the_result_the_string_and_the_caret
  native_term::ime::compose::tests::ime_string_sizes_are_refused_or_capped_before_any_allocation
  native_term::ime::compose::tests::the_caret_past_the_composition_clamps_to_its_end
  native_term::ime::compose::tests::a_char_position_query_too_small_or_past_the_marked_text_is_refused
  native_term::ime::compose::tests::an_inactive_view_commits_nothing_and_keeps_no_marked_text
  native_term::ime::compose::tests::a_commit_with_controls_or_escape_goes_through_the_text_caps
  native_term::ime::compose::tests::a_result_commits_before_the_new_composition_marks
  native_term::ime::compose::tests::character_cells_follow_the_drawn_preedit_and_stop_at_its_end
)
uia_rows=(
  native_term::windows::uia::tests::a_document_range_reads_moves_and_expands_by_lines_and_characters
  native_term::windows::uia::tests::a_range_of_another_view_or_of_no_view_is_refused_as_an_argument
  native_term::windows::uia::tests::a_range_held_past_its_view_answers_element_not_available
  native_term::windows::uia::tests::endpoints_snap_into_a_shorter_text_and_a_cut_keeps_surrogate_pairs_whole
  "$empty_row"
)
range_rows=(
  native_term::a11y::range::tests::characters_step_over_wide_cells_surrogate_pairs_and_marks
  native_term::a11y::range::tests::lines_and_the_document_move_and_expand_with_their_newlines
  native_term::a11y::range::tests::a_count_past_the_text_stops_at_its_ends_and_an_old_index_snaps_into_a_shorter_text
  native_term::a11y::range::tests::bounds_go_line_by_line_and_leave_the_newline_out
  native_term::a11y::range::tests::the_selection_or_an_empty_range_at_the_cursor_is_what_is_marked
  native_term::a11y::range::tests::a_point_takes_the_cell_under_it_or_the_nearest_edge_cell
)
h2_rows=(
  native_term::windows::keys::tests::named_keys_carry_their_modifier_bits_and_send_on_the_key_down
  native_term::windows::keys::tests::edit_keys_follow_the_panel_and_drop_their_character_message
  native_term::windows::keys::tests::control_and_alt_chords_send_from_the_keys_own_character
  native_term::windows::keys::tests::altgr_text_and_plain_keys_go_to_the_character_path
  native_term::windows::keys::tests::windows_key_chords_send_nothing_and_alt_f4_and_alt_space_stay_the_systems
  native_term::windows::keys::tests::copy_and_paste_chords_follow_the_panel_off_a_mac
  native_term::windows::keys::tests::the_character_path_types_one_character_and_joins_surrogate_pairs
  native_term::windows::place::tests::the_ime_spot_is_the_cursor_cell_in_device_pixels
  native_term::windows::board::tests::the_render_side_posts_each_views_cell_for_the_ime
)
h2_r2_rows=(
  native_term::windows::keys::tests::a_private_use_character_from_the_layout_passes_and_never_names_a_key
)
h4_rows=(
  native_term::windows::clip::tests::the_private_board_takes_one_to_cap_bytes_and_gives_them_back
  native_term::windows::clip::tests::a_block_reads_to_its_first_nul_and_never_past_the_cap
)
style_row=native_term::render::tests::every_style_finds_its_own_face_with_no_system_font
linux_rows=(
  "$style_row"
  native_term::render::tests::each_platform_draws_with_its_own_backends
  native_term::render::tests::every_present_fallback_file_loads_its_faces
  native_term::style::tests::on_linux_monospace_is_dejavu_sans_mono_else_the_built_in_font
  native_term::tests::engine_socket_needs_an_engine_db_and_a_socket
  native_term::render::tests::the_built_in_font_gives_the_rig_xterm_grid
  "${range_rows[@]}"
  "${compose_rows[@]}"
)
pure_rows=(
  "${compose_rows[@]}"
  "${uia_rows[@]}"
  "${range_rows[@]}"
  "${h2_rows[@]}"
  "${h2_r2_rows[@]}"
  "${h4_rows[@]}"
  native_term::windows::place::tests::the_dpi_gives_a_scale_key_held_to_one_to_four_times
  native_term::windows::place::tests::a_page_rect_is_finite_bounded_and_at_least_one_device_pixel
  native_term::windows::place::tests::fractional_scales_round_edges_so_touching_boxes_touch
  native_term::windows::place::tests::the_webview_offset_moves_the_rect_and_is_bounded
  native_term::windows::place::tests::a_scale_key_out_of_range_is_a_bug
  native_term::windows::board::tests::a_fresh_view_is_handed_over_once_with_its_handle_and_geometry
  native_term::windows::board::tests::a_view_draws_only_shown_with_a_frame_and_a_change
  native_term::windows::board::tests::the_newest_state_wins
  native_term::windows::board::tests::a_frame_posted_during_the_swap_chain_wait_lands_in_that_frame
  native_term::windows::board::tests::a_geometry_posted_during_the_swap_chain_wait_is_owed_after_the_take
  native_term::windows::board::tests::a_failed_swap_chain_wait_keeps_the_change_for_a_bounded_retry
  native_term::windows::board::tests::a_geometry_change_alone_redraws_the_last_frame
  native_term::windows::board::tests::remove_waits_for_the_render_side_drop_within_its_bound
  native_term::windows::board::tests::a_view_never_handed_over_is_removed_at_once
  native_term::windows::board::tests::views_are_bounded
  native_term::windows::board::tests::adding_an_id_twice_is_a_bug
  "${h5a_rows[@]}"
)
keymap_rows=(
  native_term::keymap::tests::enter_keys_match_the_panel
  native_term::keymap::tests::edit_keys_follow_their_modifiers
  native_term::keymap::tests::control_and_option_make_xterm_bytes
  native_term::keymap::tests::named_keys_carry_their_modifiers
)

display() {
  local row="$1" name="$2"
  timeout 600 cargo test "${pkg[@]}" --lib -- --ignored --exact "$row" --test-threads=1 --show-output \
    > "$RUNNER_TEMP/$name.txt" 2>&1
  local rc=$?
  grep -E '^(h1|h2|h2b|h3|h4|h5a) row:|^trial loss|^test result' "$RUNNER_TEMP/$name.txt" | tee -a "$out"
  grep -E -A3 'panicked' "$RUNNER_TEMP/$name.txt" | head -n 40 | tee -a "$out"
  return "$rc"
}

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os image $image red ${RED_SHA:-none} green $GREEN_SHA $(rustc -V) cores $(nproc 2>/dev/null || echo na)"

red_built=na
red_unit=na
red_view=na
red_failed=na
probe_built=na
probe_rc=na
webview_runtime=na
if [ -n "${RED_SHA:-}" ] && [ "$os" = windows ]; then
git checkout -q --detach "$RED_SHA"
log "== red $RED_SHA: build the lib rows"
cargo test "${pkg[@]}" --lib --no-run > "$RUNNER_TEMP/red-build.txt" 2>&1
red_built=$?
tail -n 30 "$RUNNER_TEMP/red-build.txt" >> "$out"
if [ "$red_built" = 0 ]; then
  red_unit=skip
  red_failed=1
  red_failed=1
  log "== red: the fault row (want FAILED in rows.rs: the new device lands in kept shared state)"
  display "$fault_row" red-fault
  red_view=$?
  grep -Eq "^test $fault_row \.\.\. FAILED" "$RUNNER_TEMP/red-fault.txt" || { log "::error::red $fault_row did not fail"; red_failed=0; }
  grep -Eq 'panicked at .*rows\.rs' "$RUNNER_TEMP/red-fault.txt" || { log "::error::red $fault_row did not fail in rows.rs"; red_failed=0; }
  log "== red: the kept and spent rows (want both FAILED in draw.rs)"
  timeout 600 cargo test "${pkg[@]}" --lib -- --exact "$kept_row" "$spent_row" > "$RUNNER_TEMP/red-pure.txt" 2>&1
  red_unit=$?
  grep -E '^test result|^test .* \.\.\. ' "$RUNNER_TEMP/red-pure.txt" | tee -a "$out"
  grep -E -A3 'panicked' "$RUNNER_TEMP/red-pure.txt" | head -n 20 | tee -a "$out"
  for row in "$kept_row" "$spent_row"; do
    grep -Eq "^test $row \.\.\. FAILED" "$RUNNER_TEMP/red-pure.txt" || { log "::error::red $row did not fail"; red_failed=0; }
  done
  [ "$(grep -cE 'panicked at .*draw\.rs' "$RUNNER_TEMP/red-pure.txt")" = 2 ] || { log "::error::red pure rows did not both fail in draw.rs"; red_failed=0; }
fi
git checkout -q --detach "$GREEN_SHA"
else
  log "== no red leg on $os"
fi

helper_rc=na
if [ "$os" = windows ]; then
  log "== green: the boundary helper beside the lib rows, as an agent launch needs it"
  cargo build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked > "$RUNNER_TEMP/helper.txt" 2>&1
  helper_rc=$?
  tail -n 5 "$RUNNER_TEMP/helper.txt" | tee -a "$out"
  if [ "$helper_rc" = 0 ]; then
    mkdir -p target/debug/deps
    cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe
    helper_rc=$?
  fi
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
view_rc=na
keys_rc=na
clip_rc=na
ime_rc=na
fault_rc=na
wait_rc=na
uia_rc=na
wall_rc=na
container_lookup=na
container_read=na
if [ "$rows_built" = 0 ]; then
  log "== green: native_term rows (Windows: the windows module only, as L0 runs none there)"
  filter=(native_term)
  [ "$os" = windows ] && filter=(native_term::windows native_term::a11y native_term::ime)
  cargo test "${pkg[@]}" --lib --no-fail-fast -- "${filter[@]}" 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  rows_rc=${PIPESTATUS[0]}
  passed=$(grep -cE '^test native_term::.* \.\.\. ok' "$RUNNER_TEMP/rows.txt")
  failed=$(awk '/^test .* \.\.\. FAILED/ {print $2}' "$RUNNER_TEMP/rows.txt" | sort -u)
  [ -z "$failed" ] || { log "::error::rows failed:"; log "$failed"; want=1; }
  named=("${keymap_rows[@]}" "${linux_rows[@]}")
  [ "$os" = windows ] && named=("${pure_rows[@]}")
  for row in "${named[@]}"; do
    grep -Eq "^test $row( - should panic)? \.\.\. ok" "$RUNNER_TEMP/rows.txt" || { log "::error::$row did not pass"; want=1; }
  done
  if [ "$os" = windows ]; then
    log "== green: $style_row"
    cargo test "${pkg[@]}" --lib -- --exact "$style_row" > "$RUNNER_TEMP/style.txt" 2>&1
    grep -E '^test native_term|^test result' "$RUNNER_TEMP/style.txt" | tee -a "$out"
    grep -qx "test $style_row ... ok" "$RUNNER_TEMP/style.txt" || { log "::error::the style row did not pass"; want=1; }
    log "== green: the H1 view display row"
    display "$view_row" view
    view_rc=$?
    grep -Eq "^test $view_row \.\.\. ok" "$RUNNER_TEMP/view.txt" || { log "::error::the view row did not pass"; want=1; }
    log "== green: the H2 keys display row"
    display "$keys_row" keys
    keys_rc=$?
    grep -Eq "^test $keys_row \.\.\. ok" "$RUNNER_TEMP/keys.txt" || { log "::error::the keys row did not pass"; want=1; }
    log "== green: the H2b composition display row"
    display "$ime_row" ime
    ime_rc=$?
    grep -Eq "^test $ime_row \.\.\. ok" "$RUNNER_TEMP/ime.txt" || { log "::error::the ime row did not pass"; want=1; }
    log "== green: the H5a fault display row, H5b's kept shared state"
    display "$fault_row" fault
    fault_rc=$?
    grep -Eq "^test $fault_row \.\.\. ok" "$RUNNER_TEMP/fault.txt" || { log "::error::the fault row did not pass"; want=1; }
    log "== green: the H5a r2 wait display row"
    display "$wait_row" wait
    wait_rc=$?
    grep -Eq "^test $wait_row \.\.\. ok" "$RUNNER_TEMP/wait.txt" || { log "::error::the wait row did not pass"; want=1; }
    log "== green: the H4 clip desktop row"
    display "$clip_row" clip
    clip_rc=$?
    grep -Eq "^test $clip_row \.\.\. ok" "$RUNNER_TEMP/clip.txt" || { log "::error::the clip row did not pass"; want=1; }
    log "== green: the H3 wall row: an agent container reaches the view and reads nothing"
    display "$wall_row" wall
    wall_rc=$?
    if grep -Fq 'h3 row: agent container reader ' "$RUNNER_TEMP/wall.txt"; then
      container_lookup=$(grep -E 'h3 row: agent container reader (PowerShell|Compiled): ' "$RUNNER_TEMP/wall.txt" | grep -Fc 'reader_stage=lookup' || true)
      [ "$container_lookup" = 2 ] || log "::error::INCONCLUSIVE: $container_lookup of 2 agent container readers reached their UIA lookup"
      container_read=0
      grep -Fq "an agent's AppContainer read the view" "$RUNNER_TEMP/wall.txt" && container_read=1
      [ "$container_read" = 0 ] || log "::error::an agent AppContainer reader read the view"
    fi
    grep -Eq "^test $wall_row \.\.\. ok" "$RUNNER_TEMP/wall.txt" || { log "::error::the wall row did not pass"; want=1; }
    log "== green: the H3 full reader row, the compiled IUIAutomation reader: lines, selection, bounds, Select and events"
    display "$uia_row" uia
    uia_rc=$?
    grep -Eq "^test $uia_row \.\.\. ok" "$RUNNER_TEMP/uia.txt" || { log "::error::the uia row did not pass"; want=1; }
  fi
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
fi

log "== green: clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings 2>&1 | tee -a "$out"
clip=${PIPESTATUS[0]}
log "== green: clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings 2>&1 | tee -a "$out"
clipt=${PIPESTATUS[0]}

if [ "$os" = windows ]; then
  log "== probe: the WebView2 window tree under the real app's window"
  webview_runtime=$(reg query 'HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' //v pv 2>/dev/null | awk '/pv/ {print $3}')
  log "probe: webview2 runtime ${webview_runtime:-none}"
  cargo build "${pkg[@]}" --bin crazyagent-desktop > "$RUNNER_TEMP/probe-build.txt" 2>&1
  probe_built=$?
  tail -n 5 "$RUNNER_TEMP/probe-build.txt" | tee -a "$out"
  if [ "$probe_built" = 0 ]; then
    timeout 400 python -I "$RUNNER_TEMP/webview_tree.py" "$(cygpath -w target/debug/crazyagent-desktop.exe)" "$(cygpath -w "$RUNNER_TEMP/probe-app.txt")" > "$RUNNER_TEMP/probe.txt" 2>&1
    probe_rc=$?
    grep -E '^probe:|Error|Traceback' "$RUNNER_TEMP/probe.txt" | head -n 300 | tee -a "$out"
    log "probe: app log tail"
    tail -n 40 "$RUNNER_TEMP/probe-app.txt" | tee -a "$out"
  fi
fi

red_ok=0
helper_ok=0
if [ -n "${RED_SHA:-}" ] && [ "$os" = windows ]; then
  { [ "$red_built" = 0 ] && [ "$red_failed" = 1 ]; } || red_ok=1
fi
if [ "$os" = windows ]; then
  [ "$helper_rc" = 0 ] || helper_ok=1
fi
{
  printf '| %s %s | exit |\n| --- | --- |\n' "$os" "$image"
  printf '| red build | %s |\n' "$red_built"
  printf '| red fault row rc, kept and spent rows rc (Windows only), all failed by assertion (want non-zero x2, 1) | %s, %s, %s |\n' \
    "$red_view" "$red_unit" "$red_failed"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| boundary helper beside the lib rows (Windows only) | %s |\n' "$helper_rc"
  printf '| native_term rows (Windows: the windows, a11y and ime modules) | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| view row, keys row, ime row, fault row, wait row, clip row (Windows only) | %s, %s, %s, %s, %s, %s |\n' \
    "$view_rc" "$keys_rc" "$ime_rc" "$fault_rc" "$wait_rc" "$clip_rc"
  printf '| wall row, container readers at their lookup of 2, container read the view (want 0, 2, 0) | %s, %s, %s |\n' \
    "$wall_rc" "$container_lookup" "$container_read"
  printf '| full reader row (want 0) | %s |\n' "$uia_rc"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '| probe: app build, probe rc (0 webview found, 3 none, 4 no window), WebView2 runtime (Windows only) | %s, %s, %s |\n' \
    "$probe_built" "$probe_rc" "$webview_runtime"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$helper_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] \
  && [ "$want" = 0 ] && [ "$rows_rc" = 0 ] && [ "$clip" = 0 ] && [ "$clipt" = 0 ]
