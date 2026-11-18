#!/usr/bin/env bash
set +e -uo pipefail

os="$1"
case "$os" in
  linux) ;;
  *) echo "::error::the L1 key rows run on Linux only, not $os"; exit 2 ;;
esac
out="$RUNNER_TEMP/v244-linux-keys-$os.txt"
: > "$out"
pkg=(-p crazyagent-desktop --locked)
log() { printf '%s\n' "$*" | tee -a "$out"; }

red_rows=(
  native_term::linux::keys::tests::letters_carry_control_and_alt_as_xterm_bytes
  native_term::linux::keys::tests::lock_keys_and_the_input_method_forward_bit_are_not_modifiers
  native_term::linux::keys::tests::super_and_hyper_chords_send_nothing
  native_term::linux::keys::tests::edit_keys_follow_the_panel
  native_term::linux::keys::tests::delete_and_the_keypad_are_named_before_any_text
  native_term::linux::keys::tests::function_keys_are_f1_to_f12_and_insert_with_shift_or_control_sends_nothing
)
kept_rows=(
  native_term::keymap::tests::enter_keys_match_the_panel
  native_term::keymap::tests::edit_keys_follow_their_modifiers
  native_term::keymap::tests::control_and_option_make_xterm_bytes
  native_term::keymap::tests::named_keys_carry_their_modifiers
  native_term::render::tests::each_platform_draws_with_its_own_backends
  native_term::style::tests::on_linux_monospace_is_dejavu_sans_mono_else_the_built_in_font
)

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
  printf '| red build (fails: the rows name linux::keys::key_input before it exists) | %s |\n' "$red_built"
  printf '| red rows present at red tip (want 0) | %s of %s |\n' "$red_present" "${#red_rows[@]}"
  printf '| green lib build | %s |\n| green lib rows build | %s |\n' "$lib_built" "$rows_built"
  printf '| native_term rows | %s, %s passed |\n' "$rows_rc" "$passed"
  printf '| named rows ok, none failed (0 = yes) | %s |\n' "$want"
  printf '| clippy --lib -D warnings | %s |\n| clippy --tests -D warnings | %s |\n' "$clip" "$clipt"
} | tee -a "$out" >> "$GITHUB_STEP_SUMMARY"

[ "$red_ok" = 0 ] && [ "$lib_built" = 0 ] && [ "$rows_built" = 0 ] && [ "$want" = 0 ] \
  && [ "$clip" = 0 ] && [ "$clipt" = 0 ] && { [ "$os" = windows ] || [ "$rows_rc" = 0 ]; }
