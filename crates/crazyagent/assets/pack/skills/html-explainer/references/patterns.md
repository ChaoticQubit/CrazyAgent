# Pattern catalog

Recipes for visual elements. All assume tokens + helpers in `assets/skeleton.html`. Snippets show working shape, not sacred markup — adapt freely.

Patterns 1–8 = **visual-first primitives** (replace paragraphs). 9–12 = **diagrams & simulation**. 13–19 = **support**.

## Contents

1. [Icon card grid](#1-icon-card-grid)
2. [Flip cards](#2-flip-cards)
3. [Flow strip (process steps)](#3-flow-strip-process-steps)
4. [Stat tiles / gist row](#4-stat-tiles--gist-row)
5. [Comparison matrix](#5-comparison-matrix)
6. [Timeline](#6-timeline)
7. [Pull quote & hero claim](#7-pull-quote--hero-claim)
8. [Callout chips](#8-callout-chips)
9. [SVG diagrams](#9-svg-diagrams)
10. [Interactive simulation](#10-interactive-simulation)
11. [Hand-rolled SVG chart](#11-hand-rolled-svg-chart)
12. [Math formulas](#12-math-formulas)
13. [Expandable deep-dive](#13-expandable-deep-dive)
14. [Tabs](#14-tabs)
15. [Hover glossary](#15-hover-glossary)
16. [Code block + copy button](#16-code-block--copy-button)
17. [Quiz](#17-quiz)
18. [Sticky TOC, progress bar, reading aids](#18-sticky-toc-progress-bar-reading-aids)
19. [Course index + slide-deck mode](#19-course-index--slide-deck-mode)
20. [Media: videos & images](#20-media-videos--images)

---

## 1. Icon card grid

**Default replacement for prose.** Any list of points/factors/tips/features → one card each. Icon + headline title + 2–3 short plain sentences. Grid scans in seconds, no reading order.

```html
<div class="grid">
  <div class="card"><span class="ico">🧠</span><h3>Memory fades fast</h3>
    <p>You lose the most in the first hours after learning. After a few days,
    about half is gone. Then the loss slows down.</p></div>
  <div class="card"><span class="ico">🔁</span><h3>Review resets the clock</h3>
    <p>Each review brings memory back to full. It also makes the next
    fade slower — like re-charging a battery that drains slower each time.</p></div>
</div>
```
```css
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:14px;margin:1.2rem 0}
.card{border:1px solid var(--line);border-radius:12px;padding:16px;background:var(--bg)}
.card .ico{font-size:1.5rem;display:block;margin-bottom:6px}
.card h3{margin:0 0 4px;font-size:.98rem}
.card p{margin:0;font-size:.86rem;color:var(--muted);line-height:1.45}
```
Card text: title = short clear headline. Body = **2–3 small complete plain-English sentences**, not clipped fragments ("Steepest loss first hours" reads badly for non-native speakers; small full sentences read fast AND clearly). More to say → flip (pattern 2) or expandable.

## 2. Flip cards

Workhorse for hiding text without deleting. Front: icon + term/claim. Back: full explanation — source's actual reasoning, condensed but complete, plain English. Click flips. Use for definitions, "why" behind points, FAQ answers.

```html
<div class="grid">
  <button class="flip" data-flip>
    <span class="f-front"><span class="ico">📉</span><b>Savings method</b><i>how Ebbinghaus measured it</i></span>
    <span class="f-back">Relearning takes less time than first learning; the % time saved is the memory measure. 100 → 35 min = 65% savings.</span>
  </button>
</div>
```
```css
.flip{position:relative;border:1px solid var(--line);border-radius:12px;padding:16px;background:var(--bg);
  cursor:pointer;text-align:left;font:inherit;color:inherit;min-height:120px}
.flip .f-back{display:none;font-size:.87rem;line-height:1.5;color:var(--ink)}
.flip.on .f-front{display:none}
.flip.on .f-back{display:block}
.flip .f-front i{display:block;color:var(--muted);font-style:normal;font-size:.83rem;margin-top:4px}
.flip .f-front b{display:block;font-size:.98rem}
.flip::after{content:"↻";position:absolute;top:10px;right:12px;color:var(--muted);font-size:.8rem}
```
JS (skeleton): `[data-flip]` click → toggle `.on`. Real `<button>` = keyboard accessible free.

## 3. Flow strip (process steps)

Any sequence — steps, pipeline, lifecycle, request path — becomes connected step cards + arrows, not numbered prose list. Reads left→right, wraps on mobile.

```html
<div class="flow">
  <div class="fstep"><b>1 · Identify</b><span>key or IP → bucket key</span></div>
  <div class="fstep"><b>2 · Look up</b><span>Redis hash per route+key</span></div>
  <div class="fstep"><b>3 · Consume</b><span>refill by Δt, take one token</span></div>
  <div class="fstep fail"><b>4 · Reject</b><span>empty → 429 + Retry-After</span></div>
</div>
```
```css
.flow{display:flex;flex-wrap:wrap;gap:8px;align-items:stretch;margin:1.2rem 0}
.fstep{flex:1 1 140px;border:1px solid var(--line);border-radius:10px;padding:10px 12px;
  position:relative;background:var(--surface);font-size:.85rem}
.fstep b{display:block;font-size:.9rem;margin-bottom:2px}
.fstep span{color:var(--muted)}
.fstep:not(:last-child)::after{content:"→";position:absolute;right:-12px;top:42%;color:var(--accent);z-index:2}
.fstep.fail{border-color:var(--bad)}
```
Each step can be `<details>` whose summary is the card — visual flow + hidden depth in one element.

## 4. Stat tiles / gist row

Numbers never live in sentences. Big number, tiny label, optional delta chip. Also page-opener: **gist row** of 3–5 tiles/chips replaces prose TL;DR.

```html
<div class="stats">
  <div class="stat"><b>~70%</b><span>forgotten within 24h, untrained</span></div>
  <div class="stat"><b>R = e<sup>−t/S</sup></b><span>the curve's shape</span></div>
  <div class="stat"><b>4×</b><span>spaced reviews to near-permanence</span></div>
</div>
```
```css
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px;margin:1.2rem 0}
.stat{border:1px solid var(--line);border-radius:12px;padding:14px;text-align:center;background:var(--surface)}
.stat b{font-size:1.5rem;letter-spacing:-.02em;display:block}
.stat span{font-size:.78rem;color:var(--muted);line-height:1.35;display:block;margin-top:4px}
.stat em.up{color:var(--good)}.stat em.down{color:var(--bad)}
```

## 5. Comparison matrix

Source compares 2+ things → make comparison spatial. Glyphs over words: ✅ ❌ − ⚠️. Cell text ≤4 words; nuance → footnote row or flip card below.

```css
table{border-collapse:collapse;width:100%;font-size:.88rem;margin:1rem 0}
th,td{border:1px solid var(--line);padding:8px 12px;text-align:left;vertical-align:top}
thead th{background:var(--surface)}
td.y{color:var(--good)}td.n{color:var(--bad)}
```

## 6. Timeline

Chronologies, histories, incidents. Time chip + dot + one-line label; dots colored by phase/severity. One line per event — detail flips/expands.

```html
<ol class="timeline">
  <li><span class="t">1885</span><span class="dot" style="background:var(--accent)"></span>
      Ebbinghaus publishes the curve</li>
</ol>
```
```css
.timeline{list-style:none;border-left:2px solid var(--line);padding-left:22px;margin:1.2rem 0}
.timeline li{position:relative;margin:.7rem 0;font-size:.92rem}
.timeline .t{font-family:var(--mono);color:var(--muted);margin-right:8px;font-size:.8rem}
.timeline .dot{position:absolute;left:-28px;top:6px;width:10px;height:10px;border-radius:50%}
```

## 7. Pull quote & hero claim

Memorable verbatim line → large type, not buried sentence. Argument's thesis → **hero claim**: one big statement + card row of supporting points (flip for reasoning).

```css
.pull{font-size:1.3rem;line-height:1.45;border-left:4px solid var(--accent);
  padding:6px 0 6px 20px;margin:1.6rem 0;letter-spacing:-.01em}
.pull cite{display:block;font-size:.8rem;color:var(--muted);margin-top:6px;font-style:normal}
.hero{font-size:1.45rem;letter-spacing:-.02em;line-height:1.35;margin:1.4rem 0 .8rem;font-weight:700}
```

## 8. Callout chips

Caveats, warnings, gotchas: one line + icon + color. Never paragraph.

```html
<div class="callout warn">⚠️ <b>Careful in dev</b> — limits work per process locally, so your tests
  won't show real cluster behavior.</div>
<div class="callout tip">★ If the default limit is enough, you can skip the YAML file completely.</div>
<div class="callout analogy">💡 <b>Think of it like</b> a parking lot: requests are cars, tokens are
  spaces. Spaces free up at a fixed speed. Lot full → car turned away (429).</div>
```
```css
.callout{padding:10px 14px;border-radius:8px;margin:.8rem 0;border-left:4px solid;font-size:.92rem}
.callout.warn{background:var(--warnbg);border-color:var(--warnline)}
.callout.tip{background:var(--accent-soft);border-color:var(--accent)}
.callout.analogy{background:var(--surface);border-color:var(--good)}
```

**Analogy callout deserves special attention.** One next to every hard concept — daily-life domains (water, money, food, traffic, school, phone battery) let reader borrow existing intuition. Map the *mechanism*, not mood; analogy breaks somewhere important → one short sentence saying where.

## 9. SVG diagrams

Mechanisms, architectures, relationships → boxes + arrows, redrawn inline (never screenshot text). Conventions:

- `viewBox` sized to content; width 100%; page font.
- Boxes `rx="8"`, fill `var(--surface)`, stroke `var(--line)`; hot path `stroke:var(--accent);stroke-width:2`.
- One `<marker>` arrowhead, reused. **Label every arrow** — unlabeled arrows are how diagrams lie.
- Optional: wrap nodes in `<a href="#section">` — diagram becomes navigation.

```html
<svg viewBox="0 0 640 160" role="img" aria-label="request flow">
  <defs><marker id="ar" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">
    <path d="M0 0L10 5L0 10z" fill="var(--muted)"/></marker></defs>
  <rect x="10" y="55" width="130" height="50" rx="8" fill="var(--surface)" stroke="var(--line)"/>
  <text x="75" y="84" text-anchor="middle">client</text>
  <line x1="140" y1="80" x2="240" y2="80" stroke="var(--muted)" marker-end="url(#ar)"/>
  <text x="190" y="70" text-anchor="middle" font-size="11" fill="var(--muted)">HTTP</text>
</svg>
```

## 10. Interactive simulation

Highest-payoff element. Build when concept has parameters/state prose can't convey: algorithm behavior, system under load, curve responding to inputs, feedback loop. Shape:

- **Controls row**: 1–3 sliders/buttons, visible current values.
- **Stage**: SVG (preferred) or canvas, re-rendered every input.
- **Readout line**: one sentence of state — "4 nodes · 32 keys · 8 moved on last change". Delta = lesson.
- **Reset button.** Always.

```html
<div class="sim">
  <div class="sim-controls">
    <label>strength <input type="range" id="s" min="1" max="10" value="3"><b id="sv">3</b></label>
    <button id="toggle">spaced reviews</button><button id="reset">reset</button>
  </div>
  <svg id="stage" viewBox="0 0 420 260"></svg>
  <p class="readout" id="readout"></p>
</div>
<script>
  const state={s:3,spaced:false};
  function render(){/* clear stage, draw from state, update readout */}
  document.getElementById('s').oninput=e=>{state.s=+e.target.value;render()};
  render();
</script>
```
Principles: single `state` object + single `render()`; instant feedback; readout names *what changed*. Under ~120 lines — intuition pump, not product.

Two additions that turn watching into learning:
- **Analogy line above controls** mapping knobs to life: "the strength slider is the 'how well you studied' knob."
- **1–2 missions below**: `🎯 Try: set strength to 8. How much memory is left after 7 days?` Mission gives reason to touch controls; answer teaches lesson. Hide each answer behind click (small `<details>` or reveal button) so reader plays before peeking.

## 11. Hand-rolled SVG chart

No libraries — bars = `<rect>`s, lines = `<polyline>`. Scales computed inline; axes labeled directly; accent for series that matters, muted rest; value `<text>` on each bar. ~30 lines. Skip pie charts; use bars.

## 12. Math formulas

No CDN → no MathJax/KaTeX. HTML + CSS covers most needs:

- Inline: italic vars in `<i>`, `<sup>`/`<sub>`, real minus (−) and middle dot (·): `<i>R</i> = <i>e</i><sup>−<i>t</i>/<i>S</i></sup>`
- Displayed: centered block. Fractions: stacked flexbox.

```css
.formula{text-align:center;font-size:1.15rem;margin:1.2rem 0;font-family:Georgia,serif}
.frac{display:inline-flex;flex-direction:column;text-align:center;vertical-align:middle;font-size:.85em}
.frac>span:first-child{border-bottom:1px solid var(--ink);padding:0 4px}
```
Gnarly notation (integral limits, matrices) → small inline SVG. Define every symbol nearby or in glossary; unexplained formula = decoration.

## 13. Expandable deep-dive

`<details>` for worked examples, derivations, long verbatim code/quotes — closed by default. Summary carries conclusion; body carries words removed from visible page. Source-reference chip in summary when relevant.

```html
<details class="step"><summary><span class="step-no">▣</span> Worked example: the savings calculation
  <code class="chip">Ebbinghaus 1885</code></summary>
  <p>…full detail, verbatim quotes, code…</p></details>
```
(Styling + expand-all button in skeleton.) 5+ expandables → keep expand-all button.

## 14. Tabs

Parallel variants of one thing: languages, config/usage/output, before/after. (Markup + JS in skeleton.)

## 15. Hover glossary

Jargon-heavy sources: dotted-underline term, tooltip on hover/focus via `data-def`, + glossary section (flip cards or `<dl>`) at end.

```html
<span class="term" tabindex="0" data-def="The hash output range treated as a circle.">ring</span>
```

## 16. Code block + copy button

Code always verbatim, usually inside expandable or tab. Copy button top-right (`[data-copy]`, JS in skeleton). Full syntax highlighters not worth weight; few `<span>` classes if needed.

## 17. Quiz

Courses, study material, anything to retain. 3–6 multiple choice, instant right/wrong color, one-line plain-English *why*, running score. (`[data-quiz]` handler in skeleton.)

```html
<div class="quiz" data-quiz="module-3">
  <div class="q" data-a="1"><p>1. When a node leaves, which keys move?</p>
    <button>All</button><button>Only its arc</button><button>None</button>
    <p class="why hidden">Only the departed node's arc reassigns.</p></div>
  <p class="score"></p>
  <button class="qreset" data-quiz-reset>reset quiz</button>
</div>
```

**Answers + score persist.** Skeleton handler saves each answer to `localStorage` (key `quiz:<pathname>:<quiz-id>`, `try{}`-guarded), restores full state — colors, why-lines, score — on load. Navigation/reload never wipes attempts. Stable id (`data-quiz="module-3"`) required when page has several quizzes. Include reset button.

**File backup.** `localStorage` disk-backed, survives restarts; dies with "clear browsing data", private windows, other browser/machine. Pagebar has **⤓ save** (exports all `quiz:*` + `done:*` keys + theme as `progress.json`) and **⤒ load** (file picker → restores → reloads). Keep both on any page with quiz/course tracking; module-done checkboxes use `done:`-prefixed keys so export catches them.

**Automatic disk save.** Page JS can never write to disk itself — browser sandbox forbids over `http://` same as `file://`; in-browser SQLite (WASM) persists only to browser storage, same wipe rules. Real backend = only way. Skeleton sync probes `GET /__progress?site=<page's directory URL>` on load: present → merges server snapshot into `localStorage` (reload if changed) + debounce-POSTs snapshot after every quiz answer, checkbox, theme change; absent → silent fallback to localStorage + buttons. Call `syncSoon()` after any custom write.

Backend = user's **Explainers library server** (`~/Explainers`): one server for whole folder of sites, single SQLite store, dashboard at `/`. Save site into its `Courses/` folder → persistence zero-setup. Site id = page's own directory URL — no per-site config ever.

## 18. Sticky TOC, progress bar, reading aids

All in skeleton: sidebar TOC with `IntersectionObserver` scrollspy (collapses to top on mobile), 3px scroll-progress bar, theme toggle (dark default; choice saved to `localStorage` key `theme`, guarded, so every page of multi-file output honors it), "~N min" estimate in kicker. TOC labels ≤3 words.

## 19. Course index + slide-deck mode

**Course (>~15k words source):** `index.html` with card per module (number, icon, title, 1-line blurb, time, link) + overall progress via `localStorage` checkboxes using `done:`-prefixed keys, e.g. `done:module-3` (guard `try{}`; degrade to plain links on `file://` denial). `done:` prefix matters — save/load buttons (pattern 17) export those keys. Module pages link index + prev/next. Copy token block into every file — no shared stylesheet, each file self-contained.

**Deck (only when asked):** `<section class="slide">` per slide, arrow keys, counter:
```js
const slides=[...document.querySelectorAll('.slide')];let i=0;
function show(n){i=Math.max(0,Math.min(slides.length-1,n));
  slides.forEach((s,j)=>s.classList.toggle('on',j===i))}
addEventListener('keydown',e=>{if(e.key==='ArrowRight')show(i+1);if(e.key==='ArrowLeft')show(i-1)});
```

## 20. Media: videos & images

Every video + meaningful image in source survives into page. Mirrors text contract: **summarize when you can, always include real artifact** — summary saves reader 10 minutes; embed there when they want original.

**Videos (YouTube/Vimeo/direct files).** One collapsible toggle per video, placed where source placed it:

```html
<details class="step media">
  <summary>▶ Video: How tokens work <code class="chip">3:42 · YouTube</code></summary>
  <p class="vidsum"><b>In short:</b> Tokens are word pieces. The model reads and writes
  tokens, not letters. Price and limits are counted in tokens.</p>
  <div class="vidwrap"><iframe src="https://www.youtube-nocookie.com/embed/VIDEO_ID"
    title="How tokens work" loading="lazy" allowfullscreen></iframe></div>
  <p><a href="https://youtu.be/VIDEO_ID" target="_blank">watch on YouTube ↗</a></p>
</details>
```
```css
.vidwrap{position:relative;aspect-ratio:16/9;margin:.6rem 0}
.vidwrap iframe,.vidwrap video{position:absolute;inset:0;width:100%;height:100%;
  border:0;border-radius:10px}
```

- **Summary first, when derivable** — transcript/captions reachable, or surrounding text describes content → 2–3 plain-English sentences (Language rules apply) + key points if dense. Not derivable → embed with title only; never invent summary.
- YouTube: `youtube-nocookie.com/embed/<id>`; extract id from any URL form (`watch?v=`, `youtu.be/`, `embed/`, `shorts/`). Vimeo: `player.vimeo.com/video/<id>`. Direct files: `<video controls preload="metadata" src="...">`.
- Embeds need internet — the one sanctioned exception to offline rule. Always include external "watch on …" link as fallback.
- Non-embeddable platforms (login-walled, unknown players): link card with title, duration if known, summary. URL must not be lost.
- Toggle counts as the video's representation; open by default (`<details open>`) when video is the lesson's main content rather than supplement.

**Images.** Meaningful screenshots/photos: `<figure>` with downloaded local copy (see Ingest) + one-line caption; `loading="lazy"`. Secondary/decorative-but-large images → inside collapsible (`▣ screenshot: the settings page`). Diagrams still redrawn as SVG — image of diagram = last resort.
