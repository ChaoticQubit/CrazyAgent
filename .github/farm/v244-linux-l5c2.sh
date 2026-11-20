#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux) ;;
  *) echo "::error::the Linux host rows run on Linux only, not $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-l5c2-$os.txt"
: > "$out"
root="$RUNNER_TEMP/l5c2-cells"
mkdir -p "$root"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

host=native_term::host::linux::tests
term=native_term::linux::term::tests
h_focus=$host::a_focus_asked_at_open_applies_at_the_first_shown_place
h_cover=$host::a_covered_place_hides_the_view_and_gives_the_keys_back
h_tick=$host::a_view_held_back_by_its_frame_callback_arms_one_tick
h_rescale=$host::a_rescale_drops_the_surface_and_the_next_fit_remakes_it
h_rect=$host::an_open_place_that_is_not_finite_is_refused
t_hidden=$term::a_term_made_at_open_stays_hidden_until_its_first_shown_place
t_unmapped=$term::an_open_over_an_unmapped_toplevel_fails
t_focus=$term::focus_asked_before_the_first_place_is_applied_right_after_it
t_cover=$term::a_cover_takes_the_child_off_screen_until_the_next_shown_place
t_rescale=$term::a_rescale_reports_the_new_scale_and_the_next_present_carries_it
focus_kinds="x11 sway"
plain_rows=("$h_tick" "$h_rescale" "$h_rect" "$t_hidden" "$t_unmapped" "$t_cover" "$t_rescale")
focus_rows=("$h_focus" "$h_cover" "$t_focus")
kept_rows=(
  native_term::tests::only_the_page_copy_command_writes_a_board_with_no_user_event
  native_term::tests::no_host_platform_file_raises_or_focuses_a_window
  native_term::tests::no_host_platform_file_brings_the_app_forward_or_makes_a_window_key
  native_term::tests::no_linux_native_file_raises_or_focuses_a_window
  native_term::tests::no_native_file_brings_the_app_forward_or_makes_a_window_key
  native_term::linux::clip::tests::a_command_write_needs_the_seat_not_an_event
  native_term::linux::clip::tests::a_private_board_keeps_each_board_in_process
)
red_rows=("$h_focus" "$h_tick" "$h_rescale")
red_messages=(
  "a focus asked at open applies at the first shown place"
  "a view held back by its frame callback arms one tick"
  "a rescale drops the surface"
)
red_kinds=("x11 sway" "x11 weston sway" "x11 weston sway")
host_file=apps/desktop/src-tauri/src/native_term/host/linux.rs
cover_message="a covered place hides the view"

mkdir -p apps/desktop/dist
[ -f apps/desktop/dist/index.html ] || printf '<!doctype html><title>farm</title>\n' > apps/desktop/dist/index.html
log "os $os base $BASE_SHA red $RED_SHA gate $GATE_SHA green $GREEN_SHA $(rustc -V)"
log "weston $(weston --version 2>&1 | head -1); $(sway --version 2>&1 | head -1); $(vulkaninfo --summary 2>/dev/null | grep -m1 deviceName)"
git merge-base --is-ancestor "$BASE_SHA" "$RED_SHA" && git merge-base --is-ancestor "$RED_SHA" "$GREEN_SHA"
anc=$?
log "base $BASE_SHA, red $RED_SHA and green $GREEN_SHA in line (0 = yes): $anc"
git checkout -q --detach "$GREEN_SHA"

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
  log "== green: native_term rows"
  cargo test "${pkg[@]}" --lib --no-fail-fast -- native_term 2>&1 | tee "$RUNNER_TEMP/rows.txt" >> "$out"
  rows_rc=${PIPESTATUS[0]}
  passed=$(grep -cE '^test native_term::.* \.\.\. ok$' "$RUNNER_TEMP/rows.txt")
  failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$RUNNER_TEMP/rows.txt" | sort -u)
  [ -z "$failed" ] || { log "::error::rows failed:"; log "$failed"; want=1; }
  for row in "${kept_rows[@]}"; do
    grep -qE "^test $row (- should panic )?\.\.\. ok$" "$RUNNER_TEMP/rows.txt" || { log "::error::$row did not pass"; want=1; }
  done
  for row in "${plain_rows[@]}"; do
    grep -qx "test $row ... ignored, needs a display: the farm runs it under Xvfb, weston and sway" "$RUNNER_TEMP/rows.txt" \
      || { log "::error::$row is not ignored in a plain run"; want=1; }
  done
  for row in "${focus_rows[@]}"; do
    grep -qx "test $row ... ignored, needs a display: the farm runs it under Xvfb and sway" "$RUNNER_TEMP/rows.txt" \
      || { log "::error::$row is not ignored in a plain run"; want=1; }
  done
  grep -E '^test result:' "$RUNNER_TEMP/rows.txt" | tee -a "$out"
fi

display_pid=""
load_pids=()
cell=""

start_x11() {
  Xvfb :99 -screen 0 1280x800x24 -nolisten tcp -noreset > "$root/xvfb-$cell.log" 2>&1 &
  display_pid=$!
  export DISPLAY=:99 GDK_BACKEND=x11
  unset WAYLAND_DISPLAY
  for _ in $(seq 1 50); do
    [ -S /tmp/.X11-unix/X99 ] && return 0
    sleep 0.1
  done
  return 1
}

start_weston() {
  export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-$cell" GDK_BACKEND=wayland
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  gdb -q -batch -ex 'handle SIGPIPE nostop noprint pass' -ex run -ex 'thread apply all bt' --args weston --socket=wayland-farm --width=1280 --height=800 --idle-time=0 --debug --backend=headless --renderer=pixman --shell=kiosk > "$root/weston-$cell.log" 2>&1 &
  display_pid=$!
  export WAYLAND_DISPLAY=wayland-farm
  unset DISPLAY
  for _ in $(seq 1 50); do
    [ -S "$XDG_RUNTIME_DIR/wayland-farm" ] && return 0
    sleep 0.1
  done
  return 1
}

start_sway() {
  export XDG_RUNTIME_DIR="$RUNNER_TEMP/xdg-$cell" GDK_BACKEND=wayland
  mkdir -p -m 700 "$XDG_RUNTIME_DIR"
  printf '%s\n' 'output HEADLESS-1 resolution 1280x800 position 0 0' 'default_border none' 'default_floating_border none' 'focus_follows_mouse no' 'xwayland disable' > "$root/sway.conf"
  WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman WLR_HEADLESS_OUTPUTS=1 sway -c "$root/sway.conf" > "$root/sway-$cell.log" 2>&1 &
  display_pid=$!
  unset DISPLAY
  local socket
  for _ in $(seq 1 50); do
    socket=$(ls "$XDG_RUNTIME_DIR" 2> /dev/null | grep -E '^wayland-[0-9]+$' | head -1)
    if [ -n "$socket" ]; then
      export WAYLAND_DISPLAY=$socket
      return 0
    fi
    sleep 0.1
  done
  return 1
}

display_died=()
display_alive() {
  local after=$1 status
  [ -n "$display_pid" ] || return 1
  kill -0 "$display_pid" 2> /dev/null && return 0
  wait "$display_pid" 2> /dev/null
  status=$?
  log "::error::$cell: the display (pid $display_pid) is gone after $after, exit status $status"
  grep -A40 -E 'received signal|Program terminated' "$root/$(ls "$root" | grep -E "^(weston|sway|xvfb)-$cell\.log$" | head -1)" 2> /dev/null | tee -a "$out"
  display_died+=("$cell after $after, exit $status")
  free -m | tee -a "$out"
  sudo dmesg -T 2> /dev/null | tail -n 30 | tee -a "$out"
  display_pid=""
  return 1
}

stop_display() {
  [ -n "$display_pid" ] && kill "$display_pid" 2> /dev/null
  wait "$display_pid" 2> /dev/null
  display_pid=""
}

load_on() {
  for _ in $(seq 1 "$(nproc)"); do
    yes > /dev/null &
    load_pids+=($!)
  done
}

load_off() {
  [ "${#load_pids[@]}" = 0 ] && return
  kill "${load_pids[@]}" 2> /dev/null
  wait "${load_pids[@]}" 2> /dev/null
  load_pids=()
}

shot() {
  local kind=$1 file=$2
  case "$kind" in
    x11) xwd -root -silent > "$file.xwd" && convert "xwd:$file.xwd" "$file" ;;
    sway) timeout 10 grim "$file" ;;
    weston)
      local shots="$root/shots"
      mkdir -p "$shots"
      rm -f "$shots"/*.png
      (cd "$shots" && timeout 10 weston-screenshooter > /dev/null 2>&1) || return 1
      local taken
      taken=$(ls "$shots"/*.png 2> /dev/null | head -1)
      [ -n "$taken" ] && mv "$taken" "$file"
      ;;
  esac
}

probe() {
  local name=$1 file=$2 x=$3 y=$4 want=$5 got
  got=$(convert "$file" -crop "1x1+${x}+${y}" +repage -format '%[fx:int(255*r+0.5)],%[fx:int(255*g+0.5)],%[fx:int(255*b+0.5)]' info: 2> /dev/null || echo unreadable)
  if [ "$got" = "$want" ]; then
    log "  $cell $name ($x,$y) $got ok"
    return 0
  fi
  log "::error::$cell $name ($x,$y) got $got, want $want"
  return 1
}

pixels() {
  local kind=$1 step=$2 dir=$3
  local file="$dir/$step.png"
  shot "$kind" "$file" || { log "::error::$cell $step: no screenshot"; return 1; }
  local ox oy w h s
  read -r ox oy w h s < <(jq -r '[(.root[0] // .device[0]), (.root[1] // .device[1]), .device[2], .device[3], .scale] | @tsv' "$dir/$step.json")
  local cx=$((ox + w / 2)) cy=$((oy + h / 2)) px=$((ox - 15 * s)) py=$((oy + 10 * s)) rc=0
  case "$step" in
    placed | shown)
      probe "${step}_child" "$file" "$cx" "$cy" 0,255,0 || rc=1
      probe "${step}_page" "$file" "$px" "$py" 255,255,255 || rc=1
      ;;
    hidden | covered)
      probe "${step}_child_area" "$file" "$cx" "$cy" 255,255,255 || rc=1
      ;;
    uncovered)
      probe uncovered_child "$file" "$cx" "$cy" 0,255,0 || rc=1
      probe uncovered_page "$file" "$px" "$py" 255,255,255 || rc=1
      ;;
    rescaled)
      local lx ly
      if [ "$kind" = x11 ]; then
        lx=$ox ly=$oy
      else
        lx=$((ox / s)) ly=$((oy / s))
      fi
      probe rescaled_child "$file" $((lx + 20)) $((ly + 20)) 0,255,0 || rc=1
      probe rescaled_page "$file" $((lx - 15)) $((ly + 10)) 255,255,255 || rc=1
      ;;
    *) log "::error::no probes for $step"; rc=1 ;;
  esac
  return "$rc"
}

term_rc=na
term_shots=0
term_passed=0
wait_step() {
  local file=$1 pid=$2
  for _ in $(seq 1 1200); do
    [ -f "$file" ] && return 0
    kill -0 "$pid" 2> /dev/null || return 1
    sleep 0.1
  done
  return 1
}


term_rc=na
term_shots=0
term_passed=0
run_term() {
  local kind=$1 dir=$2 row=$3
  shift 3
  local steps=("$@")
  mkdir -p "$dir"
  NO_AT_BRIDGE=1 NATIVE_ROW_DIR="$dir" timeout 240 cargo test "${pkg[@]}" --lib -- --ignored --exact "$row" --test-threads=1 > "$dir/row.txt" 2>&1 &
  local pid=$! step
  term_shots=0
  term_passed=0
  for step in "${steps[@]}"; do
    if ! wait_step "$dir/$step.json" "$pid"; then
      log "::error::$cell ${row##*::}: the row never reached $step"
      break
    fi
    term_shots=$((term_shots + 1))
    pixels "$kind" "$step" "$dir" && term_passed=$((term_passed + 1))
    : > "$dir/go-$step"
  done
  wait "$pid"
  term_rc=$?
  display_alive "${row##*::}"
  tail -n 12 "$dir/row.txt" >> "$out"
  log "$cell ${row##*::}: row exit $term_rc, steps $term_shots of ${#steps[@]}, pixel steps passed $term_passed of ${#steps[@]}"
  [ "$term_rc" = 0 ] && [ "$term_passed" = "${#steps[@]}" ] && grep -qE '^test result: ok\. 1 passed;' "$dir/row.txt"
}

cells=()
cell_rows=()
row_ok=0
run_cell() {
  cell=$1
  local kind=$2 load=$3
  local dir="$root/$cell" bad=0 line="| $cell |"
  mkdir -p "$dir"
  log "== cell $cell ($kind, $load)"
  if ! "start_$kind"; then
    log "::error::$cell: the $kind display did not start"
    cells+=("$cell")
    cell_rows+=("| $cell | display did not start |")
    row_ok=1
    stop_display
    return
  fi
  [ "$load" = loaded ] && load_on
  run_term "$kind" "$dir/hidden" "$t_hidden" hidden placed || bad=1
  line="$line $term_rc ($term_passed of 2) |"
  run_term "$kind" "$dir/cover" "$t_cover" shown covered uncovered || bad=1
  line="$line $term_rc ($term_passed of 3) |"
  run_term "$kind" "$dir/rescale" "$t_rescale" rescaled || bad=1
  line="$line $term_rc ($term_passed of 1) |"
  local row
  for row in "$t_unmapped" "$h_tick" "$h_rescale" "$h_rect" "$t_focus" "$h_focus" "$h_cover"; do
    case " ${focus_rows[*]} " in
      *" $row "*) [ "$kind" = weston ] && { line="$line na |"; continue; } ;;
    esac
    run_term "$kind" "$dir/${row##*::}" "$row" || bad=1
    line="$line $term_rc |"
  done
  load_off
  stop_display
  cell_rows+=("$line")
  if [ "$bad" != 0 ]; then
    row_ok=1
    cells+=("$cell")
  fi
}

red_hit() {
  local file=$1 message=$2
  [ "$(grep -cE '^test result: FAILED\. 0 passed; 1 failed;' "$file")" = 1 ] && grep -q 'panicked at' "$file" && grep -qF "$message" "$file"
}

red_lines=()
if [ "${LAND:-0}" = 1 ]; then
  red_ok=0
  red_built=0
  mutant_ok=0
  log "== land: the reds of $RED_SHA and the cover mutant are not run here; they ran in $SLICE_RUN"
else
red_ok=1
red_built=na
git checkout -q --detach "$RED_SHA"
git cherry-pick -n "$GATE_SHA" >> "$out" 2>&1
gated=$?
log "== red $RED_SHA with the seam's macOS gate on the v243 device rows ($GATE_SHA, test cfg only, applied: 0 = yes $gated): lib rows build"
cargo test "${pkg[@]}" --lib --no-run >> "$out" 2>&1
red_built=$?
if [ "$red_built" = 0 ]; then
  red_ok=0
  for kind in x11 weston sway; do
    cell="red-$kind"
    "start_$kind" || { log "::error::$cell: the display did not start"; red_ok=1; stop_display; continue; }
    for i in "${!red_rows[@]}"; do
      case " ${red_kinds[$i]} " in *" $kind "*) ;; *) continue ;; esac
      row=${red_rows[$i]}
      file="$root/$cell-${row##*::}.txt"
      NO_AT_BRIDGE=1 timeout 240 cargo test "${pkg[@]}" --lib -- --ignored --exact "$row" --test-threads=1 > "$file" 2>&1
      rc=$?
      red_hit "$file" "${red_messages[$i]}"
      hit=$?
      tail -n 12 "$file" >> "$out"
      log "red $RED_SHA $cell ${row##*::}: exit $rc (want 101), failed by its assertion (0 = yes) $hit"
      red_lines+=("| $cell | ${row##*::} | $rc | $hit |")
      { [ "$rc" = 101 ] && [ "$hit" = 0 ]; } || red_ok=1
    done
    stop_display
  done
fi
git reset -q --hard
git checkout -q -f --detach "$GREEN_SHA"
[ -z "$(git status --porcelain --untracked-files=no)" ] || { log "::error::the tree is not clean after the red cells"; rows_built=1; }

mutant_ok=1
log "== mutant of green $GREEN_SHA: place_view shows a view placed hidden; $h_cover must fail on '$cover_message' on X11 and sway"
python3 -I - "$host_file" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = """    let at = shown
        .then(|| host_rect(&view.page, &view.toplevel, rect))
        .flatten();"""
assert s.count(old) == 1, "the place_view shown fold"
s = s.replace(old, """    let _ = shown;
    let at = host_rect(&view.page, &view.toplevel, rect);""")
open(p, "w").write(s)
PY
mutated=$?
git diff --stat -- "$host_file" | tee -a "$out"
if [ "$mutated" = 0 ] && cargo test "${pkg[@]}" --lib --no-run >> "$out" 2>&1; then
  mutant_ok=0
  for kind in x11 sway; do
    cell="mutant-$kind"
    "start_$kind" || { log "::error::$cell: the display did not start"; mutant_ok=1; stop_display; continue; }
    file="$root/$cell-cover.txt"
    NO_AT_BRIDGE=1 timeout 240 cargo test "${pkg[@]}" --lib -- --ignored --exact "$h_cover" --test-threads=1 > "$file" 2>&1
    rc=$?
    red_hit "$file" "$cover_message"
    hit=$?
    tail -n 12 "$file" >> "$out"
    log "mutant $cell ${h_cover##*::}: exit $rc (want 101), failed by its assertion (0 = yes) $hit"
    red_lines+=("| $cell | ${h_cover##*::} | $rc | $hit |")
    { [ "$rc" = 101 ] && [ "$hit" = 0 ]; } || mutant_ok=1
    stop_display
  done
fi
git checkout -q -f -- "$host_file"
[ -z "$(git status --porcelain --untracked-files=no)" ] || { log "::error::the tree is not clean after the mutant"; rows_built=1; }

fi

log "== green: lib rows build again after the red cells"
cargo test "${pkg[@]}" --lib --no-run >> "$out" 2>&1 || rows_built=1

if [ "$rows_built" = 0 ]; then
  run_cell x11 x11 quiet
  run_cell weston weston quiet
  run_cell sway sway quiet
  run_cell x11-loaded x11 loaded
  run_cell weston-loaded weston loaded
  run_cell sway-loaded sway loaded
  run_cell weston-loaded-2 weston loaded
  run_cell weston-loaded-3 weston loaded
else
  row_ok=1
fi

log "== green: clippy --lib"
cargo clippy "${pkg[@]}" --lib -- -D warnings 2>&1 | tee -a "$out"
clip=${PIPESTATUS[0]}
log "== green: clippy --tests"
cargo clippy "${pkg[@]}" --tests -- -D warnings 2>&1 | tee -a "$out"
clipt=${PIPESTATUS[0]}

{
  printf '\n| %s | exit |\n| --- | --- |\n' "$os"
  printf '| base %s, red %s and green %s in line (0 = yes) | %s |\n' "$BASE_SHA" "$RED_SHA" "$GREEN_SHA" "$anc"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n| named rows ok, none failed, display rows ignored (0 = yes) | %s |\n' "$rows_rc" "$passed" "$want"
  printf '| red %s: lib rows build | %s |\n| red: every red row fails by its own assertion in its cells (0 = yes) | %s |\n' "$RED_SHA" "$red_built" "$red_ok"
  printf '| green with place_view showing a hidden place: the cover row fails by its own assertion on X11 and sway (0 = yes) | %s |\n' "$mutant_ok"
  printf '| green: every display row passes in every cell (0 = yes) | %s |\n' "$row_ok"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
  printf '\n| cell | open hidden (pixels) | cover (pixels) | rescale (pixels) | unmapped | host tick | host rescale | host rect | term focus | host focus | host cover |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n'
  printf '%s\n' "${cell_rows[@]}"
  printf '\n| red cell | row | exit (want 101) | own assertion (0 = yes) |\n| --- | --- | --- | --- |\n'
  printf '%s\n' "${red_lines[@]}"
  [ "${#cells[@]}" = 0 ] || printf '\nfailed cells: %s\n' "${cells[*]}"
  [ "${#display_died[@]}" = 0 ] || printf '\ndisplays gone: %s\n' "${display_died[*]}"
} | tee -a "$out" >> "${GITHUB_STEP_SUMMARY:-/dev/null}"

fail=0
for v in "$anc" "$lib_built" "$rows_built" "$want" "$red_built" "$red_ok" "$mutant_ok" "$row_ok" "$clip" "$clipt"; do
  [ "$v" = 0 ] || fail=1
done
[ "$rows_rc" = 0 ] || fail=1
exit "$fail"
