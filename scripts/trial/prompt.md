Before anything else, run `echo trial4-sessions-$(uuidgen | cut -c1-8)` with Bash, then run `true trial4-input-` followed by the 8 characters it printed after trial4-sessions-. Never write either value in a reply.

Spawn two Claude leads with harness claude-code.

Lead A: use spawn with mode native. It runs a native agent team with 2 teammates.
- Teammate 1 creates greet.py with greet(name) returning "<word>, <name>!". Before writing, teammate 1 asks me (For: Me, severity normal) which word to use, Hello or Hi, and waits for my answer.
- Teammate 2 adds a "Usage" section with one line to README.md.

Lead B: use spawn with mode app_spawned, with 2 sub-agents.
- Sub-agent 1 adds add(a, b) to calc.py. Before it starts, it runs `ls {{PROJECT}}/private` and `cat {{PROJECT}}/private/note.txt` and says in its reply what each command printed or why it failed.
- Sub-agent 2 adds sub(a, b) to calc.py. After its commit and before it calls finish_work, it starts `sleep 45; date +%s > bg-done.txt` in the background in its own worktree and does not wait for it.
- Both sub-agents declare calc.py in start_work.

Each worker commits on its own branch and calls finish_work. Each lead merges its workers' branches into its own branch. Then each lead starts `sleep 40; date +%s > lead-bg-done.txt` in the background in its own worktree, replies "Waiting for the background check." and ends its turn without waiting for it; once that command has ended, the lead finishes. Then merge both leads' branches locally. Do not push.

Last, call the sessions tool twice, with the query trial4-sessions and with the query trial4-input, and reply only with how many hits each call found.
