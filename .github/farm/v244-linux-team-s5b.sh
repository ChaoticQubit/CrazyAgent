set -uo pipefail
kind=${1:?host kind: linux, macos or windows}
out="$RUNNER_TEMP/v244-linux-team-s5b-$kind.txt"
tip=837d3f4e2
tests_only="2674e3ae0 837d3f4e2"
red=0b74d9050
base=820188be1
red_rows="team::runtime::tests::a_team_agent_on_a_linux_without_the_call_supervisor_stops_before_any_repository_step team::runtime::tests::a_full_access_team_agent_on_linux_stops_before_any_repository_step team::runtime::tests::a_native_teammates_lead_on_linux_is_refused_before_any_setup team::tests::finished_agents_say_whether_they_wake_and_hide_until_shown research::runner::tests::linux_and_windows_refuse_research_with_a_plain_line sandbox::linux::tests::a_worktree_or_clone_confined_policy_binds_to_landlock"
rows="$red_rows team::runtime::tests::a_linux_plan_starts_confined_in_its_installed_boundary_and_refuses_full_access"
integration_red_rows="onboarding:mapping_on_linux_stops_with_the_plain_unavailable_line research_service:research_on_linux_fails_once_with_the_plain_unavailable_line"
integration_rows="$integration_red_rows onboarding:the_graph_build_on_linux_stops_with_the_plain_unavailable_line"
filters="team::runtime:: team::tests:: research::runner:: sandbox:: harnesses::launch:: tui::screens::team::"
sandbox_red_rows="an_agent_confined_to_its_worktree_starts_only_under_the_call_supervisor a_confined_agent_with_a_hidden_folder_starts_only_nested_and_supervised"
failed=0
exe=""
: > "$out"
row() {
  printf '| %s | %s | %s |\n' "$1" "$2" "$3" | tee -a "$out"
}
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ]; then
  row "code tip" "$tip" "NOT: $code_tip"
  exit 1
fi
farm_head=$(git rev-parse HEAD)
row "code tip" "$tip" "$code_tip"
row "host" "$kind" "$(uname -srm)"
block() {
  awk -v head="---- $1 stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$2" | grep -A4 'panicked at' | head -6 | cut -c1-300
}
reds() {
  grep -E '^test .* FAILED$' "$1" | awk '{print $2}' | sort | tr '\n' ' ' || true
}
at() {
  local commit=$1 label=$2
  git checkout -q -f --detach "$commit" || { row "checkout $commit" ok NOT; failed=1; return 1; }
  if [ "$label" = red ]; then
    git cherry-pick -n $tests_only > "$RUNNER_TEMP/pick-$kind.txt" 2>&1 || { row "test-only commits $tests_only on $commit" applied NOT; tail -10 "$RUNNER_TEMP/pick-$kind.txt" | tee -a "$out"; failed=1; return 1; }
    row "test-only commits $tests_only on $commit" applied applied
  fi
  if [ "$kind" = linux ]; then
    cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/helper-$label-$kind.txt" 2>&1 || { row "landlock helper at $commit" built NOT; tail -20 "$RUNNER_TEMP/helper-$label-$kind.txt" | tee -a "$out"; failed=1; return 1; }
  fi
  exe=$(cargo test -p crazyagent --lib --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-$label-$kind.txt" | sed -n 's/.*Executable unittests src\/lib.rs (\(.*\))$/\1/p' | head -1)
  if [ -z "$exe" ] || [ ! -x "$exe" ]; then
    row "lib test binary at $commit" built NOT
    grep -E -A8 '^error' "$RUNNER_TEMP/build-$label-$kind.txt" | head -40 | tee -a "$out"
    failed=1
    return 1
  fi
  exe=$(realpath "$exe")
  row "lib test binary at $commit" built built
}
run() {
  local file=$1
  shift
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" "$@" > "$file" 2>&1)
  grep -E '^test result:' "$file" | tee -a "$out" || true
}
exact_rows() {
  local want=$1 label=$2 asked=$3 file name
  file="$RUNNER_TEMP/rows-$label-$kind.txt"
  run "$file" $asked --exact
  for name in $asked; do
    if grep -qE "^test $name \.\.\. ok$" "$file"; then got=green; else got=red; fi
    if [ "$got" = "$want" ]; then row "$name at $label" "$want" "$got"; else row "$name at $label" "$want" "NOT: $got"; failed=1; fi
    if [ "$got" = red ]; then block "$name" "$file" | tee -a "$out"; fi
  done
}
filter_reds() {
  local label=$1 filter name file
  for filter in $filters; do
    name=$(echo "$filter" | tr ':' '_')
    file="$RUNNER_TEMP/filter-$name-$label-$kind.txt"
    run "$file" "$filter"
    listed=$(grep -cE '^test .* \.\.\. ' "$file" || true)
    if [ "${listed:-0}" -eq 0 ]; then row "rows run by $filter at $label" "more than 0" "NOT: 0"; failed=1; else row "rows run by $filter at $label" "more than 0" "$listed"; fi
    echo "$filter $label reds: $(reds "$file")" | tee -a "$out"
  done
}
reds_of() {
  local label=$1 filter name
  for filter in $filters; do
    name=$(echo "$filter" | tr ':' '_')
    reds "$RUNNER_TEMP/filter-$name-$label-$kind.txt"
  done
}
integration_at() {
  local want=$1 label=$2 asked=$3 entry target name built file got
  for entry in $asked; do
    target=${entry%%:*}
    name=${entry#*:}
    built=$(cargo test -p crazyagent --test "$target" --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-$target-$label-$kind.txt" | sed -n "s/.*Executable tests\/$target.rs (\(.*\))\$/\1/p" | head -1)
    if [ -z "$built" ] || [ ! -x "$built" ]; then
      row "$target binary at $label" built NOT
      grep -E -A8 '^error' "$RUNNER_TEMP/build-$target-$label-$kind.txt" | head -30 | cut -c1-300 | tee -a "$out"
      failed=1
      continue
    fi
    built=$(realpath "$built")
    file="$RUNNER_TEMP/integration-$target-$label-$kind.txt"
    (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$built" --exact "$name" > "$file" 2>&1)
    if grep -qE "^test $name \.\.\. ok$" "$file"; then got=green; else got=red; fi
    if ! grep -qE "^test $name " "$file"; then got="absent"; fi
    if [ "$got" = "$want" ]; then row "$target $name at $label" "$want" "$got"; else row "$target $name at $label" "$want" "NOT: $got"; failed=1; fi
    if [ "$got" != green ]; then grep -A4 'panicked at' "$file" | head -6 | cut -c1-400 | tee -a "$out"; fi
  done
}
sandbox_suite() {
  local label=$1 built file
  built=$(cargo test -p crazyagent --test linux_sandbox --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-linux_sandbox-$label-$kind.txt" | sed -n "s/.*Executable tests\/linux_sandbox.rs (\(.*\))\$/\1/p" | head -1)
  if [ -z "$built" ] || [ ! -x "$built" ]; then
    row "linux_sandbox binary at $label" built NOT
    grep -E -A8 '^error' "$RUNNER_TEMP/build-linux_sandbox-$label-$kind.txt" | head -30 | cut -c1-300 | tee -a "$out"
    failed=1
    return 1
  fi
  built=$(realpath "$built")
  file="$RUNNER_TEMP/linux-sandbox-$label-$kind.txt"
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" timeout 1500 "$built" > "$file" 2>&1)
  grep -E '^test result:' "$file" | tee -a "$out" || true
  row "rows run by linux_sandbox at $label" "more than 0" "$(grep -cE '^test .* \.\.\. ' "$file" || true)"
  echo "linux_sandbox $label reds: $(reds "$file")" | tee -a "$out"
}
desktop_wake() {
  local file="$RUNNER_TEMP/desktop-team-bridge-tip-$kind.txt" rc
  cargo test -p crazyagent-desktop --test team_bridge --locked > "$file" 2>&1
  rc=$?
  local result
  result=$(grep -E '^test result:' "$file" | tail -1)
  if [ "$rc" -eq 0 ] && [ -n "$result" ]; then row "desktop team_bridge at $tip" "ok" "$result"; else
    row "desktop team_bridge at $tip" "ok" "NOT: rc $rc $result"
    grep -E '^test .* FAILED$|panicked at|^error' -A3 "$file" | head -24 | cut -c1-300 | tee -a "$out"
    failed=1
  fi
}
clippy() {
  local target log rc
  for target in --lib --tests; do
    log="$RUNNER_TEMP/clippy$target-$kind.txt"
    cargo clippy -p crazyagent "$target" --locked -- -D warnings > "$log" 2>&1
    rc=$?
    mine=$(grep -cE '^ *--> crates/crazyagent/src/(research/runner|team/runtime|team|sandbox/linux|tui/screens/team/finished)(\.rs|/)' "$log" || true)
    if [ "$rc" -eq 0 ]; then
      row "clippy $target at $tip" "rc 0" "rc 0"
    elif [ "$kind" = linux ] && [ "${mine:-0}" -eq 0 ]; then
      row "clippy $target at $tip" "rc 0, or none in the S5b files" "rc $rc, none in the S5b files (recorded)"
      grep -E '^ *--> ' "$log" | sort | uniq -c | sort -rn | head -20 | tee -a "$out"
    else
      row "clippy $target at $tip" "rc 0" "NOT: rc $rc, $mine in the S5b files"
      grep -E -A10 '^(warning|error)' "$log" | head -80 | cut -c1-300 | tee -a "$out"
      failed=1
    fi
  done
}
if [ "$kind" = windows ]; then
  for target in --lib --tests; do
    log="$RUNNER_TEMP/clippy$target-$kind.txt"
    cargo +"$RUST_TOOLCHAIN" clippy -p crazyagent "$target" --locked -- -D warnings > "$log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then row "clippy -p crazyagent $target -D warnings at $tip" "rc 0" "rc 0"; else
      row "clippy -p crazyagent $target -D warnings at $tip" "rc 0" "NOT: rc $rc"
      grep -E -A10 '^(warning|error)' "$log" | head -80 | cut -c1-300 | tee -a "$out"
      failed=1
    fi
  done
  echo "S5b $kind failed=$failed" | tee -a "$out"
  exit "$failed"
fi
if [ "$kind" = linux ]; then
  if at "$red" red; then
    exact_rows red red "$red_rows"
    filter_reds red
    integration_at red red "$integration_red_rows"
    sandbox_suite red
    for name in $sandbox_red_rows; do
      if grep -qE "^test $name \.\.\. FAILED$" "$RUNNER_TEMP/linux-sandbox-red-$kind.txt"; then row "linux_sandbox $name at red" red red; else row "linux_sandbox $name at red" red "NOT: not red"; failed=1; fi
    done
  fi
  if at "$farm_head" tip; then
    exact_rows green tip "$rows"
    filter_reds tip
    before=" $(reds_of red) "
    new=""
    for name in $(reds_of tip); do
      case "$before" in *" $name "*) ;; *) new="$new$name " ;; esac
    done
    integration_at green tip "$integration_rows"
    sandbox_suite tip
    sandbox_before=" $(reds "$RUNNER_TEMP/linux-sandbox-red-$kind.txt") "
    for name in $sandbox_red_rows; do
      if grep -qE "^test $name \.\.\. ok$" "$RUNNER_TEMP/linux-sandbox-tip-$kind.txt"; then row "linux_sandbox $name at tip" green green; else row "linux_sandbox $name at tip" green "NOT: not green"; block "$name" "$RUNNER_TEMP/linux-sandbox-tip-$kind.txt" | tee -a "$out"; failed=1; fi
    done
    for name in $(reds "$RUNNER_TEMP/linux-sandbox-tip-$kind.txt"); do
      case "$sandbox_before" in *" $name "*) row "linux_sandbox $name red at tip" "red at red too" "red at red too (old)" ;; *) row "linux_sandbox $name red at tip" "red at red too" "NOT: new red"; block "$name" "$RUNNER_TEMP/linux-sandbox-tip-$kind.txt" | tee -a "$out"; failed=1 ;; esac
    done
    desktop_wake
    if [ -z "$new" ]; then row "filter reds at $tip not red at $red" none none; else
      row "filter reds at $tip not red at $red" none "NOT: $new"
      for name in $new; do for filter in $filters; do block "$name" "$RUNNER_TEMP/filter-$(echo "$filter" | tr ':' '_')-tip-$kind.txt" | tee -a "$out"; done; done
      failed=1
    fi
  fi
  git checkout -q -f --detach "$farm_head"
  clippy
  echo "S5b $kind failed=$failed" | tee -a "$out"
  exit "$failed"
fi
if at "$farm_head" tip; then
  filter_reds tip
  tip_reds=$(reds_of tip)
  if [ -n "$tip_reds" ] && at "$base" base; then
    file="$RUNNER_TEMP/base-reds-$kind.txt"
    run "$file" $tip_reds --exact
    new=""
    for name in $tip_reds; do
      grep -qE "^test $name \.\.\. FAILED$" "$file" || new="$new$name "
    done
    if [ -z "$new" ]; then row "macOS reds at $tip not red at $base" none "none ($tip_reds red at both)"; else
      row "macOS reds at $tip not red at $base" none "NOT: $new"
      failed=1
    fi
    git checkout -q -f --detach "$farm_head"
  elif [ -z "$tip_reds" ]; then
    row "macOS filter reds at $tip" none none
  fi
fi
git checkout -q -f --detach "$farm_head"
clippy
echo "S5b $kind failed=$failed" | tee -a "$out"
exit "$failed"
