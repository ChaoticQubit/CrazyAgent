set -uo pipefail
kind=${1:?host kind: linux, macos or windows}
out="$RUNNER_TEMP/v244-linux-team-s4-$kind.txt"
tip=4760d4f6f
red=febf76afa
base=bfc2f617b
rows="research::runner::tests::an_agent_worktree_is_added_on_linux_inside_the_installed_boundary research::runner::tests::a_lead_agents_own_git_is_refused_on_linux_before_any_git_starts"
filters="research::runner:: team::runtime::"
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
  local want=$1 label=$2 file name
  file="$RUNNER_TEMP/rows-$label-$kind.txt"
  run "$file" $rows --exact
  for name in $rows; do
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
clippy() {
  local target log rc
  for target in --lib --tests; do
    log="$RUNNER_TEMP/clippy$target-$kind.txt"
    cargo clippy -p crazyagent "$target" --locked -- -D warnings > "$log" 2>&1
    rc=$?
    mine=$(grep -cE '^ *--> crates/crazyagent/src/(research/runner|team/runtime)\.rs' "$log" || true)
    if [ "$rc" -eq 0 ]; then
      row "clippy $target at $tip" "rc 0" "rc 0"
    elif [ "$kind" = linux ] && [ "${mine:-0}" -eq 0 ]; then
      row "clippy $target at $tip" "rc 0, or none in runner.rs or runtime.rs" "rc $rc, none in runner.rs or runtime.rs (recorded)"
      grep -E '^ *--> ' "$log" | sort | uniq -c | sort -rn | head -20 | tee -a "$out"
    else
      row "clippy $target at $tip" "rc 0" "NOT: rc $rc, $mine in runner.rs or runtime.rs"
      grep -E -A10 '^(warning|error)' "$log" | head -80 | cut -c1-300 | tee -a "$out"
      failed=1
    fi
  done
}
if [ "$kind" = windows ]; then
  cargo check -p crazyagent --all-targets --locked --message-format short > "$RUNNER_TEMP/check-$kind.txt" 2>&1
  rc=$?
  issues=$(grep -cE '^(error|warning)' "$RUNNER_TEMP/check-$kind.txt" || true)
  if [ "$rc" -eq 0 ] && [ "${issues:-0}" -eq 0 ]; then
    row "cargo check -p crazyagent --all-targets at $tip" "rc 0, 0 warnings" "rc 0, 0 warnings"
  else
    row "cargo check -p crazyagent --all-targets at $tip" "rc 0, 0 warnings" "NOT: rc $rc, $issues errors or warnings"
    grep -E '(^|: )(error|warning)' "$RUNNER_TEMP/check-$kind.txt" | head -40 | tee -a "$out"
    failed=1
  fi
  echo "S4 $kind failed=$failed" | tee -a "$out"
  exit "$failed"
fi
if [ "$kind" = linux ]; then
  if at "$red" red; then
    exact_rows red red
    filter_reds red
  fi
  if at "$farm_head" tip; then
    exact_rows green tip
    filter_reds tip
    before=" $(reds_of red) "
    new=""
    for name in $(reds_of tip); do
      case "$before" in *" $name "*) ;; *) new="$new$name " ;; esac
    done
    if [ -z "$new" ]; then row "filter reds at $tip not red at $red" none none; else
      row "filter reds at $tip not red at $red" none "NOT: $new"
      for name in $new; do block "$name" "$RUNNER_TEMP/filter-research__runner__-tip-$kind.txt" | tee -a "$out"; block "$name" "$RUNNER_TEMP/filter-team__runtime__-tip-$kind.txt" | tee -a "$out"; done
      failed=1
    fi
  fi
  git checkout -q -f --detach "$farm_head"
  clippy
  echo "S4 $kind failed=$failed" | tee -a "$out"
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
echo "S4 $kind failed=$failed" | tee -a "$out"
exit "$failed"
