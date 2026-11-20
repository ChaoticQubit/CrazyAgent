set -uo pipefail
out="$RUNNER_TEMP/v244-linux-team-s5b-e2e-linux.txt"
tip=efa81c043
rows="a_supervised_linux_sub_agent_turn_lands_its_page_and_is_refused_every_git_metadata_write a_linux_master_with_its_live_switch_on_still_answers_the_owner_headless"
failed=0
: > "$out"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"
}
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ]; then
  row "code tip" "$tip" "NOT: $code_tip"
  exit 1
fi
row "code tip" "$tip" "$code_tip"
row "host" linux "$(uname -srm)"
row "tmux on the runner" "recorded" "$(tmux -V 2>&1 || true)"
cargo build -p crazyagent --bin crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/bins-linux.txt" 2>&1 || { row "engine and helper binaries" built NOT; tail -30 "$RUNNER_TEMP/bins-linux.txt" | tee -a "$out"; exit 1; }
row "engine and helper binaries" built built
built=$(cargo test -p crazyagent --test linux_team_turn --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-linux_team_turn-linux.txt" | sed -n 's/.*Executable tests\/linux_team_turn.rs (\(.*\))$/\1/p' | head -1)
if [ -z "$built" ] || [ ! -x "$built" ]; then
  row "linux_team_turn binary" built NOT
  grep -E -A12 '^error' "$RUNNER_TEMP/build-linux_team_turn-linux.txt" | head -120 | cut -c1-300 | tee -a "$out"
  echo "S5b e2e failed=1" | tee -a "$out"
  exit 1
fi
built=$(realpath "$built")
row "linux_team_turn binary" built built
file="$RUNNER_TEMP/linux-team-turn-linux.txt"
listed=$("$built" --list 2>/dev/null | grep -c ': test$' || true)
row "rows listed in linux_team_turn" 2 "$listed"
for name in $rows; do
  file="$RUNNER_TEMP/linux-team-turn-$name-linux.txt"
  start=$(date +%s)
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" timeout 900 "$built" --exact "$name" --nocapture > "$file" 2>&1)
  rc=$?
  row "$name seconds" "under 900" "$(( $(date +%s) - start ))"
  grep -E '^live problem: ' "$file" | head -2 | tee -a "$out" || true
  if grep -qE "^test $name \.\.\. ok$" "$file"; then row "$name" green green; else
    row "$name" green "NOT: rc $rc"
    awk '/panicked at/{on=1} on' "$file" | grep -E 'panicked|problem|status:|pane:|live_problem|role:|author:|state:|text:|stderr|linux_|agent panes|unavailable|refus|error' | head -80 | cut -c1-400 | tee -a "$out"
    failed=1
  fi
done
cargo clippy -p crazyagent --test linux_team_turn --locked -- -D warnings > "$RUNNER_TEMP/clippy-e2e-linux.txt" 2>&1
rc=$?
if [ "$rc" -eq 0 ]; then row "clippy --test linux_team_turn" "rc 0" "rc 0"; else
  row "clippy --test linux_team_turn" "rc 0" "NOT: rc $rc"
  grep -E -A10 '^(warning|error)' "$RUNNER_TEMP/clippy-e2e-linux.txt" | head -60 | cut -c1-300 | tee -a "$out"
  failed=1
fi
echo "S5b e2e failed=$failed" | tee -a "$out"
exit "$failed"
