import { useState } from "react";
import { call, failureCopy, nextGeneration } from "../../api-projects";
import type { ResearchJob } from "../../api-projects";
import { displayText } from "../conversation";
import { harnessName } from "../new-project/team-editor";
import { researchRow } from "./research";
import type { ResearchRow } from "./research";
import { Badge, NotReady, valueOf } from "./shared";
import type { ProjectData } from "./shared";

/** Words an agent row uses for a research job, from the design's fixed agent set.
 * Input: the research row. Output: the agent status word and tone. */
export function agentStatus(row: ResearchRow): { word: string; tone: string } {
  switch (row.status) {
    case "Queued": return { word: "Starting", tone: "info" };
    case "Needs a pool": return { word: "Needs a pool", tone: "warn" };
    default: return { word: row.status, tone: row.tone };
  }
}

/** Agents tab: every agent the project has started, with live progress from the engine.
 * Input: the project data. Output: the list; rows refresh while research polls. */
export function AgentsTab({ data }: { data: ProjectData }) {
  const [filter, setFilter] = useState<"running" | "all">("running");
  const [stopping, setStopping] = useState<ResearchJob | null>(null);
  const [note, setNote] = useState("");
  const jobs = valueOf(data.research)?.jobs ?? [];
  const setup = valueOf(data.setup);
  const harnesses = valueOf(data.harnesses);
  const running = jobs.filter((job) => job.status === "running" || job.status === "queued");
  const list = filter === "running" ? running : jobs;
  const accounts = data.assigned?.pool.members.length ?? 0;

  async function stop(job: ResearchJob) {
    setStopping(null);
    const result = await call<unknown>("desktop_cancel_research", { id: data.id, job: job.id, generation: nextGeneration() });
    if (result.kind === "ok") {
      setNote("");
      data.reload();
    } else setNote(result.kind === "not_ready" ? "Not ready yet: stopping an agent isn't in this build." : failureCopy(result.code));
  }

  return <div className="pj-stack">
    <div className="pj-row between">
      <div className="pj-seg" role="radiogroup" aria-label="Show">
        <button type="button" role="radio" aria-checked={filter === "running"} onClick={() => setFilter("running")}>Running ({running.length})</button>
        <button type="button" role="radio" aria-checked={filter === "all"} onClick={() => setFilter("all")}>All ({jobs.length})</button>
      </div>
      <span className="pj-secondary">{data.assigned
        ? `${jobs.filter((job) => job.status === "running").length} of ${setup?.team.max_agents ?? "?"} agents running. ${data.assigned.pool.id}: ${accounts} ${accounts === 1 ? "account" : "accounts"}.`
        : "No pool. Agents wait until you choose one."}</span>
    </div>
    {data.research?.kind === "not_ready" && <NotReady what="live agent status isn't in this build." />}
    {data.research?.kind === "failed" && <p className="pj-error" role="alert">Couldn't load agents. {failureCopy(data.research.code)}</p>}
    {note && <p className="pj-note warn" role="status">{note}</p>}
    {list.length > 0 ? <ul className="pj-rows" aria-label="Agents">{list.map((job) => {
      const row = researchRow(job, data.now);
      const status = agentStatus(row);
      return <li className="pj-agent-row" key={job.id}>
        <strong>{displayText(row.title)}</strong>
        <span className="pj-secondary pj-small">{harnessName(harnesses, job.harness)}</span>
        <span><Badge word={status.word} tone={status.tone} /> <span className="pj-small pj-secondary">{displayText(row.progress)}</span></span>
        <span className="pj-small pj-muted">{row.elapsed}</span>
        <span>{job.status === "running" && !job.cancel_requested && <button type="button" className="pj-btn danger-text" onClick={() => setStopping(job)}>Stop</button>}</span>
        <span className="pj-wt pj-small pj-muted">Writes <span className="pj-mono">{displayText(job.report_path)}</span>. Tools {job.tools ? "on" : "off"}.{job.attempts > 1 ? ` Attempt ${job.attempts}.` : ""}</span>
      </li>;
    })}</ul> : data.research?.kind === "ok" && <div className="pj-empty">
      <h3>{filter === "running" && jobs.length ? "No agents running right now" : "No agents yet"}</h3>
      <p className="pj-secondary">Research agents start from the Work step, and the master starts agents when you ask it to build something.</p>
    </div>}
    {stopping && <div className="pj-scrim"><div className="pj-dialog" role="alertdialog" aria-modal="true" aria-labelledby="pj-stop-title">
      <h2 id="pj-stop-title">Stop {displayText(stopping.kind_label)}? Work so far stays in its working copy.</h2>
      <div className="pj-row end">
        <button type="button" className="pj-btn" autoFocus onClick={() => setStopping(null)}>Keep running</button>
        <button type="button" className="pj-btn danger" onClick={() => void stop(stopping)}>Stop agent</button>
      </div>
    </div></div>}
  </div>;
}
