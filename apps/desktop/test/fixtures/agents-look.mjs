import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";

/**
 * Draws the Agents tab's grouping as agents.tsx and team.tsx draw it: the Agents and tasks
 * fold holding the Team table (a teammates list inside a row's name cell) and the Team tasks
 * list, then the Research heading and list, with the real stylesheets. Input: the theme.
 * Output: none.
 */
export function mount(theme = "light") {
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div class="pj" style="width: 900px; padding: 16px"><div class="pj-stack pj-agents">'
    + '<details class="pj-stack-s" open><summary><strong>Agents and tasks</strong> <span class="pj-small pj-muted">Stop all.</span></summary>'
    + '<section class="pj-stack-s"><div class="pj-row between"><h3>Team</h3></div>'
    + '<div class="pj-table-wrap" id="team"><table class="pj-table"><thead><tr><th scope="col">Agent</th><th scope="col">Status</th></tr></thead><tbody>'
    + '<tr><th scope="row" id="lead-row"><span class="pj-kind" id="lead">master</span><span class="pj-small pj-muted pj-block">Master</span>'
    + '<ul class="pj-rows" id="mates"><li class="pj-row"><span class="pj-small pj-secondary">Teammate in pane %1</span></li>'
    + '<li class="pj-row"><span class="pj-small pj-secondary">Teammate in pane %2</span></li></ul></th><td>Working</td></tr>'
    + '<tr><th scope="row"><span class="pj-kind">w1 api</span></th><td>Working</td></tr></tbody></table></div>'
    + '<ul class="pj-rows" id="tasks"><li class="pj-task"><strong id="task">Fix the login</strong></li><li class="pj-task"><strong>Add tests</strong></li></ul>'
    + '</section></details>'
    + '<h3 id="research-title">Research</h3><ul class="pj-rows" id="research"><li class="pj-agent-row"><strong class="pj-kind" id="job">Overview research</strong></li>'
    + '<li class="pj-agent-row"><strong class="pj-kind">Roadmap</strong></li></ul></div></div>';
}

/**
 * The color one token paints in the mounted theme, as the browser computes it.
 * Input: the token name without its dashes. Output: an rgb() string.
 */
function token(name) {
  const probe = document.createElement("span");
  probe.style.color = `var(--${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
}

/**
 * Reads one group's box. Input: the group's id. Output: its side, top and bottom edges, corner
 * and fill.
 */
function box(id) {
  const style = getComputedStyle(document.getElementById(id));
  return {
    side: style.borderLeftWidth, top: style.borderTopWidth, bottom: `${style.borderBottomWidth} ${style.borderBottomColor}`,
    corner: style.borderTopLeftRadius, fill: style.backgroundColor,
  };
}

/**
 * Reads one list's box and rows. Input: the list's id. Output: its box and the first and last
 * rows' top edges.
 */
function list(id) {
  const rows = document.querySelectorAll(`#${id} > li`);
  return {
    ...box(id),
    first: getComputedStyle(rows[0]).borderTopWidth, last: `${getComputedStyle(rows[rows.length - 1]).borderTopWidth} ${getComputedStyle(rows[rows.length - 1]).borderTopColor}`,
  };
}

/**
 * Reads the Agents tab's groups and names. Output: the hairline token, the Team table's box,
 * the teammates, Team tasks and Research lists, and the weight of the fold label, a team name
 * and its cell, a task, the Research heading and a research title.
 */
export function look() {
  const weight = (selector) => getComputedStyle(document.querySelector(selector)).fontWeight;
  return {
    border: token("border"),
    team: box("team"),
    mates: list("mates"),
    tasks: list("tasks"),
    research: list("research"),
    weights: { fold: weight("summary strong"), lead: weight("#lead"), cell: weight("#lead-row"), task: weight("#task"), heading: weight("#research-title"), job: weight("#job") },
  };
}
