set -uo pipefail
tip=635b451fd
red=4aa746a64
row=a_supervised_linux_worktree_add_keeps_every_other_admin_folder_byte_for_byte
out="$RUNNER_TEMP/v244-linux-team-s4-r5-linux.txt"
failed=0
: > "$out"
say() { printf '%s\n' "$*" | tee -a "$out"; }
code_tip=$(git log -1 --format=%h --abbrev=9 HEAD -- . ':!.github')
if [ "$code_tip" != "$tip" ] || ! git merge-base --is-ancestor "$red" "$tip"; then
  say "| code tip | $tip on $red | NOT: $code_tip |"
  exit 1
fi
farm_head=$(git rev-parse HEAD)
say "| code tip | $tip | $code_tip |"
say "| host | linux | $(uname -srm), uid $(id -u), $(git --version) |"
built() {
  local label=$1 exe
  cargo build -p crazyagent --bin crazyagent-landlock --locked > "$RUNNER_TEMP/helper-$label.txt" 2>&1 || { say "| helper at $label | built | NOT |" >&2; tail -20 "$RUNNER_TEMP/helper-$label.txt" | tee -a "$out" >&2; return 1; }
  exe=$(cargo test -p crazyagent --test linux_worktree_add --locked --no-run 2>&1 | tee "$RUNNER_TEMP/build-$label.txt" | sed -n 's/.*Executable tests\/linux_worktree_add.rs (\(.*\))$/\1/p' | head -1)
  if [ -z "$exe" ] || [ ! -x "$exe" ]; then
    say "| integration binary at $label | built | NOT |" >&2
    grep -E -A8 '^error' "$RUNNER_TEMP/build-$label.txt" | head -40 | cut -c1-300 | tee -a "$out" >&2
    return 1
  fi
  realpath "$exe"
}
one() {
  local want=$1 label=$2 exe=$3 file got
  file="$RUNNER_TEMP/row-$label.txt"
  (cd crates/crazyagent && CARGO_MANIFEST_DIR="$PWD" "$exe" --exact "$row" > "$file" 2>&1)
  got=red
  grep -qE "^test $row \.\.\. ok$" "$file" && got=green
  if [ "$got" = "$want" ]; then say "| $row at $label | $want | $got |"; else say "| $row at $label | $want | NOT: $got |"; failed=1; fi
  grep -E 'linux_supervisor|linux_helper' "$file" | sort | uniq -c | head -5 | tee -a "$out"
  [ "$got" = red ] && grep -A4 'panicked at' "$file" | head -6 | cut -c1-400 | tee -a "$out"
  return 0
}
git checkout -q -f --detach "$red"
if git cherry-pick -n "$tip" > "$RUNNER_TEMP/pick.txt" 2>&1; then
  say "| $tip rows applied onto $red | clean | clean |"
  if exe=$(built red); then one red "$red+rows" "$exe"; else failed=1; fi
else
  say "| $tip rows applied onto $red | clean | NOT |"
  cat "$RUNNER_TEMP/pick.txt" | tee -a "$out"
  failed=1
fi
git reset -q --hard
git checkout -q -f --detach "$farm_head"
if exe=$(built tip); then one green "$tip" "$exe"; else failed=1; fi
log="$RUNNER_TEMP/clippy-tests.txt"
cargo clippy -p crazyagent --tests --locked -- -D warnings > "$log" 2>&1
rc=$?
if [ "$rc" -eq 0 ]; then say "| clippy --tests at $tip | rc 0 | rc 0 |"; else
  say "| clippy --tests at $tip | rc 0 | NOT: rc $rc |"
  grep -E -A10 '^(warning|error)' "$log" | head -60 | cut -c1-300 | tee -a "$out"
  failed=1
fi
say "S4 r5 linux failed=$failed"
exit "$failed"
