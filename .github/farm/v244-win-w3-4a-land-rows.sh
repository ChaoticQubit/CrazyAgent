set -uo pipefail
tip=ef92a3df6
red=da2ad4014
os=${FARM_OS:?FARM_OS is windows or linux}
out="$RUNNER_TEMP/w3-4a"
mkdir -p "$out"
bad=0
say() { printf '%s\n' "$*"; }
rustc -V | grep -qF " ${RUST_TOOLCHAIN:?} " || { say "rustc is not $RUST_TOOLCHAIN: $(rustc -V)"; exit 1; }
git checkout -q -f --detach "$tip" || exit 1
[ "$(git rev-parse --short=9 HEAD)" = "$tip" ] || { say "tree is not $tip"; exit 1; }
r3="engine::bootstrap::anchor_windows_tests::an_anchor_given_as_a_short_name_or_another_case_opens_at_its_plain_long_name engine::owner::transport::root_windows_tests::a_short_name_root_and_a_verbatim_unc_root_are_refused"
r3_files="crates/crazyagent/src/engine/bootstrap/anchor_windows_tests.rs crates/crazyagent/src/engine/owner/transport/root_windows_tests.rs"
case "$os" in
  windows)
    cargo test -p crazyagent-win-sandbox --locked --test private_windows -- --test-threads 1 > "$out/w1.log" 2>&1
    rc=$?
    say "W1 private_windows rc=$rc | $(grep '^test result' "$out/w1.log" | tail -1 | cut -c1-120)"
    [ "$rc" -eq 0 ] || bad=1
    grep -q 'a_deletable_secret_file_is_read_while_held_and_deleted_through_its_own_handle \.\.\. ok' "$out/w1.log" || { say "ROW NOT OK: a_deletable_secret_file_is_read_while_held_and_deleted_through_its_own_handle"; bad=1; }
    mkdir -p /c/w34atmp
    git checkout -q "$red" -- $r3_files || exit 1
    TMP='C:\w34atmp' TEMP='C:\w34atmp' cargo test -p crazyagent --lib --locked -- --test-threads 1 --exact $r3 > "$out/red.log" 2>&1
    rc=$?
    git checkout -q "$tip" -- $r3_files
    git diff --quiet "$tip" -- crates || { say "tree not back at $tip"; exit 1; }
    hits=$(grep -c "spelled with a short name" "$out/red.log" || true)
    say "red r3 rows at $red's two row files rc=$rc | $(grep '^test result' "$out/red.log" | tail -1 | cut -c1-120) | short-name lines $hits"
    { [ "$rc" -ne 0 ] && grep -qE 'test result: FAILED\. 0 passed; 2 failed' "$out/red.log" && [ "$hits" -ge 2 ]; } || { grep -E 'panicked|^error' "$out/red.log" | head -20 | cut -c1-300; bad=1; }
    TMP='C:\w34atmp' TEMP='C:\w34atmp' cargo test -p crazyagent --lib --locked -- --test-threads 1 --show-output --exact $r3 > "$out/r3.log" 2>&1
    rc=$?
    skips=$(grep -c '^SKIP' "$out/r3.log" || true)
    say "r3 rows at the tip rc=$rc | $(grep '^test result' "$out/r3.log" | tail -1 | cut -c1-120) | SKIP lines $skips"
    { [ "$rc" -eq 0 ] && grep -qE 'test result: ok\. 2 passed; 0 failed' "$out/r3.log" && [ "$skips" -eq 0 ]; } || { grep -A8 'panicked' "$out/r3.log" | head -30 | cut -c1-300; bad=1; }
    cargo test -p crazyagent --lib --locked -- --test-threads 1 engine::owner::transport::key_windows_tests:: engine::owner::transport::root_windows_tests:: engine::bootstrap::anchor_windows_tests:: storage::projects_home::engine_root_windows_tests:: projects_home::compare_windows_tests:: > "$out/tip.log" 2>&1
    rc=$?
    say "W3-4a, W3-2 and W3-1 rows at the tip rc=$rc | $(grep '^test result' "$out/tip.log" | tail -1 | cut -c1-120)"
    { [ "$rc" -eq 0 ] && grep -qE 'test result: ok\. 38 passed; 0 failed' "$out/tip.log"; } || { grep -E '\.\.\. FAILED$' "$out/tip.log" | head; grep -A8 'panicked' "$out/tip.log" | head -30 | cut -c1-300; bad=1; }
    ;;
  linux)
    cargo test -p crazyagent --lib --locked -- projects_home > "$out/unix.log" 2>&1
    rc=$?
    say "Unix projects_home rows rc=$rc | $(grep '^test result' "$out/unix.log" | tail -1 | cut -c1-120)"
    { [ "$rc" -eq 0 ] && grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed' "$out/unix.log"; } || { grep -E '\.\.\. FAILED$' "$out/unix.log" | head; bad=1; }
    ;;
  *) say "bad FARM_OS $os"; exit 2 ;;
esac
[ "$bad" -eq 0 ] && say "W3-4a $os ok at $tip"
exit "$bad"
