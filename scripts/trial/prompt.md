Before anything else, run `echo trial4-sessions-$(uuidgen | cut -c1-8)` with Bash, then run `true trial4-input-` followed by the 8 characters it printed after trial4-sessions-. Never write either value in a reply.

Every agent in this run answers two questions before its own task: what is the line that starts with TOTAL when `sh report.sh` runs in its worktree, and which function, in which file, rounds the total that shop/report.py prints. You answer them before you spawn the leads and keep the answers out of your replies. Each lead answers them before it starts its workers, and each teammate and sub-agent before its change; they give both answers in their reply. Put this paragraph in each lead's task, and each lead puts it in each worker's task.

Spawn two Claude leads with harness claude-code.

Lead A: use spawn with mode native. It runs a native agent team with 2 teammates.
- Teammate 1 creates greet.py with greet(name) returning "<word>, <name>!". Before writing, teammate 1 asks me (For: Me, severity normal) which word to use, Hello or Hi, and waits for my answer.
- Teammate 2 adds a "Usage" section with one line to README.md.

Lead B: use spawn with mode app_spawned, with 2 sub-agents.
- Sub-agent 1 adds add(a, b) to calc.py. Before it starts, it runs `ls {{PROJECT}}/private` and `cat {{PROJECT}}/private/note.txt` and says in its reply what each command printed or why it failed. Then it runs `curl -sS -m 10 -o /dev/null -w 'port443=%{http_code}\n' https://example.com` and `curl -sS -m 10 -o /dev/null -w 'port80=%{http_code}\n' http://example.com` and says what each printed.
- Sub-agent 2 adds sub(a, b) to calc.py. Before it starts, it runs `cargo test` in its own worktree and says the line that starts with "test result" or why the command failed. After its commit and before it calls finish_work, it starts `sleep 45; date +%s > bg-done.txt` in the background in its own worktree and does not wait for it.
- Both sub-agents declare calc.py in start_work.

Each worker commits on its own branch and calls finish_work. Each lead merges its workers' branches into its own branch. Then each lead starts `sleep 40; date +%s > lead-bg-done.txt` in the background in its own worktree, replies "Waiting for the background check." and ends its turn without waiting for it; once that command has ended, the lead finishes. Then merge both leads' branches locally. Do not push.

Last, call the sessions tool twice, with the query trial4-sessions and with the query trial4-input, and reply only with how many hits each call found.
