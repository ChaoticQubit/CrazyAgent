set +e -uo pipefail
label=${1:?usage: v244-batch.sh <label>}
out="$RUNNER_TEMP/v244-batch-$label.txt"
table="$RUNNER_TEMP/v244-batch-$label.md"
git log --oneline -1 | tee "$out"
printf '| %s step | exit |\n| --- | --- |\n' "$label" > "$table"
fail=0
step() {
  local name=$1
  shift
  "$@" 2>&1 | tee -a "$out"
  local rc=${PIPESTATUS[0]}
  echo "== $name rc=$rc" | tee -a "$out"
  printf '| %s | %s |\n' "$name" "$rc" >> "$table"
  [ "$rc" = 0 ] || fail=1
}
cargo=(cargo +"$RUST_TOOLCHAIN")
case "$RUNNER_OS" in
  Windows)
    step "win-sandbox test --lib" "${cargo[@]}" test -p crazyagent-win-sandbox --lib --locked --no-fail-fast
    for target in boundary private_judge private_windows; do
      step "win-sandbox test --test $target" "${cargo[@]}" test -p crazyagent-win-sandbox --test "$target" --locked --no-fail-fast
    done
    step "build --bin crazyagent-win-boundary" "${cargo[@]}" build -p crazyagent-win-sandbox --bin crazyagent-win-boundary --locked
    mkdir -p target/debug/deps && cp target/debug/crazyagent-win-boundary.exe target/debug/deps/crazyagent-win-boundary.exe || fail=1
    step "crazyagent test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
    step "crazyagent lib rows (sandbox policy and windows, plugins, storage, context_mode windows)" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- sandbox::policy::tests sandbox::windows::tests plugins:: storage:: context_mode::windows_tests
    step "crazyagent clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "crazyagent clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    step "win-sandbox clippy --lib" "${cargo[@]}" clippy -p crazyagent-win-sandbox --lib --locked -- -D warnings
    step "win-sandbox clippy --bins" "${cargo[@]}" clippy -p crazyagent-win-sandbox --bins --locked -- -D warnings
    step "win-sandbox clippy --tests" "${cargo[@]}" clippy -p crazyagent-win-sandbox --tests --locked --keep-going -- -D warnings
    ;;
  Linux)
    step "build --bin crazyagent-landlock" "${cargo[@]}" build -p crazyagent --bin crazyagent-landlock --locked
    mkdir -p target/debug/deps && cp target/debug/crazyagent-landlock target/debug/deps/crazyagent-landlock || fail=1
    step "build --bin crazyagent" "${cargo[@]}" build -p crazyagent --bin crazyagent --locked
    step "test --lib --no-run" "${cargo[@]}" test -p crazyagent --lib --locked --no-run
    step "test --lib" "${cargo[@]}" test -p crazyagent --lib --locked --no-fail-fast -- --skip github::tests::signed_in_gh_cli_token_is_reused_through_the_real_gh
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  macOS)
    step "build --lib" "${cargo[@]}" build -p crazyagent --lib --locked
    step "clippy --lib" "${cargo[@]}" clippy -p crazyagent --lib --locked -- -D warnings
    step "clippy --tests" "${cargo[@]}" clippy -p crazyagent --tests --locked --keep-going -- -D warnings
    ;;
  *)
    echo "::error::no batch recipe for RUNNER_OS=$RUNNER_OS"
    exit 1
    ;;
esac
failed=$(awk '/^test .* \.\.\. FAILED$/ {print $2}' "$out" | sort -u)
{
  cat "$table"
  echo
  echo "Failed rows: $(printf '%s' "$failed" | grep -c .)"
  [ -z "$failed" ] || printf -- '- %s\n' $failed
} >> "$GITHUB_STEP_SUMMARY"
exit "$fail"
