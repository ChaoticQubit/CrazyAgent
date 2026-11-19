set -uo pipefail
ROW=a_hidden_folders_cover_stays_on_whatever_the_agent_unmounts
uname -r
git log --oneline -3
bwrap --version
unshare --version
cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null || true
cat /proc/sys/user/max_user_namespaces
cat /sys/kernel/security/lsm 2>/dev/null
echo
cargo test -p crazyagent --test linux_sandbox --locked -- --exact "$ROW" > "$RUNNER_TEMP/probe.txt" 2>&1
grep -E '^(running [0-9]+ tests?|test result)' "$RUNNER_TEMP/probe.txt"
awk -v head="---- $ROW stdout ----" '$0 == head {on = 1} on && /^(---- |failures:$)/ && $0 != head {exit} on {print}' "$RUNNER_TEMP/probe.txt" | python3 -I -c '
import sys
text = sys.stdin.read()
print(text.replace("\\\\n", "\n").replace("\\n", "\n").replace("\\\"", "\""))
'
grep -q "test $ROW ... FAILED" "$RUNNER_TEMP/probe.txt"
