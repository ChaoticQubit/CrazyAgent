import shell from "../../src/style.css?inline";
import projects from "../../src/screens/project/projects.css?inline";

/**
 * Draws the Agents tab's grouping as agents.tsx, team.tsx and the team panels draw it: the
 * Agents and tasks fold, a team list with a nested teammates list, and the Research list, with
 * the real stylesheets. Input: the theme. Output: none.
 */
export function mount(theme = "light") {
  document.head.innerHTML = "";
  const style = document.createElement("style");
  style.textContent = [shell, projects].join("\n");
  document.head.append(style);
  document.documentElement.dataset.theme = theme;
  document.body.innerHTML = '<div class="pj" style="width: 900px; padding: 16px"><div class="pj-stack pj-agents">'
    + '<details class="pj-stack-s" open><summary><strong>Agents and tasks</strong> <span class="pj-small pj-muted">Stop all.</span></summary>'
    + '<ul class="pj-rows" id="team"><li><span class="pj-kind" id="lead">master</span>'
    + '<ul class="pj-rows" id="mates"><li><span class="pj-kind">w1 api</span></li><li><span class="pj-kind">w2 tests</span></li></ul></li>'
    + '<li><strong id="task">Fix the login</strong></li></ul></details>'
    + '<h3>Research</h3><ul class="pj-rows" id="research"><li class="pj-agent-row"><strong class="pj-kind" id="job">Overview research</strong></li>'
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
 * Reads one list's box. Input: the list's id. Output: its side, top and bottom edges, corner,
 * fill and the first and last rows' top edges.
 */
function list(id) {
  const style = getComputedStyle(document.getElementById(id));
  const rows = document.querySelectorAll(`#${id} > li`);
  return {
    side: style.borderLeftWidth, top: style.borderTopWidth, bottom: `${style.borderBottomWidth} ${style.borderBottomColor}`,
    corner: style.borderTopLeftRadius, fill: style.backgroundColor,
    first: getComputedStyle(rows[0]).borderTopWidth, last: `${getComputedStyle(rows[rows.length - 1]).borderTopWidth} ${getComputedStyle(rows[rows.length - 1]).borderTopColor}`,
  };
}

/**
 * Reads the Agents tab's lists and names. Output: the hairline token, the team, teammates and
 * Research lists, and the weight of the fold label, a team name, a task and a research title.
 */
export function look() {
  const weight = (selector) => getComputedStyle(document.querySelector(selector)).fontWeight;
  return {
    border: token("border"),
    team: list("team"),
    mates: list("mates"),
    research: list("research"),
    weights: { fold: weight("summary strong"), lead: weight("#lead"), task: weight("#task"), job: weight("#job") },
  };
}
