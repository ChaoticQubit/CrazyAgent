CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL);

CREATE TABLE team_agents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                role TEXT NOT NULL,
                harness TEXT NOT NULL,
                tools INTEGER NOT NULL,
                connection TEXT,
                worktree TEXT,
                status TEXT NOT NULL,
                task INTEGER,
                process_group INTEGER,
                turns INTEGER NOT NULL DEFAULT 0,
                error TEXT,
                created_ms INTEGER NOT NULL,
                heartbeat_ms INTEGER NOT NULL,
                stopped_ms INTEGER
            , session TEXT, parent INTEGER, pane TEXT, mode TEXT, hookless INTEGER NOT NULL DEFAULT 0);

CREATE TABLE team_names (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                persona TEXT NOT NULL,
                handle TEXT
            );

CREATE TABLE team_worktrees (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                repository TEXT NOT NULL,
                worktree TEXT NOT NULL,
                git_dir TEXT NOT NULL
            );

CREATE TABLE team_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                source TEXT NOT NULL,
                turn INTEGER,
                harness TEXT NOT NULL,
                tools INTEGER NOT NULL,
                prompt TEXT NOT NULL,
                status TEXT NOT NULL,
                agent INTEGER,
                attempts INTEGER NOT NULL DEFAULT 0,
                result TEXT,
                error TEXT,
                cancel_requested INTEGER NOT NULL DEFAULT 0,
                delivered INTEGER NOT NULL DEFAULT 0,
                output_bytes INTEGER NOT NULL DEFAULT 0,
                created_ms INTEGER NOT NULL,
                started_ms INTEGER,
                finished_ms INTEGER
            , session TEXT, parent INTEGER, mode TEXT);

CREATE TABLE team_panes (
                agent INTEGER PRIMARY KEY,
                record TEXT NOT NULL
            );

CREATE TABLE team_teammates (
                agent INTEGER NOT NULL,
                project TEXT NOT NULL,
                pane TEXT NOT NULL,
                seen_ms INTEGER NOT NULL,
                ended_ms INTEGER,
                stopped_ms INTEGER,
                PRIMARY KEY(agent, pane)
            );

CREATE TABLE landing_agent_branches (
    worktree TEXT PRIMARY KEY,
    repository TEXT NOT NULL,
    branch TEXT NOT NULL
);

CREATE TABLE coord_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    from_kind TEXT NOT NULL,
    from_agent INTEGER,
    to_kind TEXT NOT NULL,
    to_agent INTEGER,
    kind TEXT NOT NULL,
    q_for TEXT,
    q_severity TEXT,
    work INTEGER,
    reply_to INTEGER,
    body TEXT NOT NULL,
    idem_key TEXT,
    receipt TEXT NOT NULL,
    created INTEGER NOT NULL,
    receipt_at INTEGER NOT NULL,
    UNIQUE(from_agent, idem_key)
);

CREATE TABLE team_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                agent INTEGER NOT NULL,
                task INTEGER,
                kind TEXT NOT NULL,
                text TEXT NOT NULL,
                state TEXT NOT NULL,
                wake INTEGER,
                created_ms INTEGER NOT NULL,
                delivered_ms INTEGER
            );

CREATE TABLE coord_claims (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    agent INTEGER NOT NULL,
    work INTEGER NOT NULL,
    pattern TEXT NOT NULL,
    created INTEGER NOT NULL,
    released INTEGER
);

CREATE TABLE team_master_branch (
                project TEXT PRIMARY KEY,
                branch TEXT NOT NULL
            );

CREATE TABLE landing_change_sets (
    id TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    agent_kind TEXT NOT NULL CHECK(agent_kind IN ('master','sub')),
    agent_id TEXT NOT NULL,
    label TEXT NOT NULL,
    repository TEXT NOT NULL,
    worktree TEXT NOT NULL,
    git_dir TEXT NOT NULL,
    base TEXT NOT NULL,
    head TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','conflict','updating','accepted','discarded')),
    last_outcome TEXT NOT NULL CHECK(last_outcome IN ('succeeded','failed','cancelled')),
    turn TEXT NOT NULL,
    prompt_line TEXT NOT NULL,
    files INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    conflicts TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    restorable_until_ms INTEGER,
    pruned INTEGER NOT NULL DEFAULT 0 CHECK(pruned IN (0,1)),
    landed TEXT,
    intent TEXT
, message TEXT NOT NULL DEFAULT '', waiting INTEGER NOT NULL DEFAULT 0 CHECK(waiting IN (0,1)), outside INTEGER NOT NULL DEFAULT 0 CHECK(outside IN (0,1)));

CREATE TABLE team_paused_projects (
                project TEXT PRIMARY KEY,
                since_ms INTEGER NOT NULL
            );

CREATE TABLE team_owner_stops (
                agent INTEGER PRIMARY KEY,
                project TEXT NOT NULL,
                stopped_ms INTEGER NOT NULL
            );

CREATE TABLE agent_instruction_settings (
            project TEXT PRIMARY KEY,
            mode TEXT NOT NULL,
            transcripts INTEGER NOT NULL
        );

CREATE TABLE plugin_switches (
    project TEXT NOT NULL CHECK(length(project) BETWEEN 1 AND 128),
    item TEXT NOT NULL CHECK(length(item) BETWEEN 1 AND 64),
    PRIMARY KEY(project, item)
);

CREATE TABLE "plugin_items" (
    id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 64),
    kind TEXT NOT NULL CHECK(kind IN ('skill','server','hook')),
    scope TEXT NOT NULL CHECK(length(scope) BETWEEN 1 AND 160),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) <= 65536),
    UNIQUE(kind, scope, name)
);

CREATE TABLE coord_tool_use (
    agent INTEGER NOT NULL,
    tool TEXT NOT NULL,
    count INTEGER NOT NULL,
    last INTEGER NOT NULL,
    PRIMARY KEY(agent, tool)
);

CREATE TABLE instruction_imports (
            project TEXT PRIMARY KEY,
            moved TEXT NOT NULL
        );

CREATE TABLE team_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                author TEXT NOT NULL,
                text TEXT NOT NULL,
                state TEXT NOT NULL,
                created_ms INTEGER NOT NULL
            );

CREATE TABLE coord_settings (
    project TEXT PRIMARY KEY,
    json TEXT NOT NULL
);

CREATE TABLE team_nesting (
                project TEXT PRIMARY KEY,
                depth INTEGER NOT NULL CHECK(depth BETWEEN 1 AND 3),
                lands TEXT NOT NULL CHECK(lands IN ('lead','owner'))
            );

CREATE TABLE team_land_on (
                project TEXT PRIMARY KEY,
                branch TEXT NOT NULL
            );

CREATE TABLE team_background (
                project TEXT PRIMARY KEY,
                mode TEXT NOT NULL CHECK(mode IN ('wait','grace','stop')),
                grace_secs INTEGER CHECK((mode = 'grace') = (grace_secs IS NOT NULL))
            );

