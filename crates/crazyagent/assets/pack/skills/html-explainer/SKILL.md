---
name: html-explainer
description: >-
  Turn a document, article, README, spec, PDF or URL into a visual-first single-file
  interactive HTML page (Thariq Shihipar style): icon cards, diagrams, flows, stat
  tiles, timelines, simulations. Use on "make an HTML version", "create an explainer",
  "make this visual".
---

# HTML Explainer — visual-first interactive pages

Turn source into self-contained `.html` understood in fraction of original reading time. Page = infographic crossed with small app, not document. Prose is serial — reader must process every word in order. Visual structure (cards, diagrams, flows, color, position) is parallel — eye takes in grid at glance, drills only where needed. Convert serial prose to parallel visuals; push remaining prose behind interactions.

**Squint test:** zoomed out, page must read as shapes/colors/icons/short labels. Gray text columns = failed.

## The contract: full coverage, minimal visible text

Reader uses page *instead of* original. Nothing vanishes — but coverage = **represented**, not transcribed.

1. **Every topic, section, claim represented** — as card, diagram node, flow step, table row, timeline event, stat tile. Also every number, name, caveat, example. 9 source sections → 9 visual blocks.
2. **Visible text short + natural, never paragraphs.** Card = title + 2–3 short complete sentences — natural language, not clipped fragments ("Untrained memory roughly halves within days" harder than "Memory fades fast at first. Without review, you lose about half within a few days."). Section = 1–2 lead sentences before visual block. >~80 visible words outside visual elements = writing document again — convert or hide. "Visual elements" = everything in pattern catalog: cards, flips, tiles, flow steps, timeline lines, callouts (incl. analogy), expandables, sim readouts/missions, quiz items — text inside them exempt; each pattern's own text rules govern. Complete-sentence rule applies to card/flip/callout bodies + leads; micro-labels (tile labels, flow spans, chips, TOC, timeline lines) may stay fragments.
3. **Detail goes behind interactions, never deleted.** Flip, hover tooltip, expandable — long explanation one click away. Fast path = look; deep path = click.
4. **Code, math, quotes verbatim** (usually inside expandables). Never paraphrase. Hard-English quote: keep verbatim + one-line plain-words gloss under it.
5. **Citations compressible** — inline attributions ("Murre & Dros, 2015") + link to original suffice.

Two tests: "could reader answer any question original answers?" (coverage via interactions) + "whole gist in under 2 minutes without reading a paragraph?" (visual delivery).

## Language: write for a global reader

Reader = smart learner, English not first language. Technical terms fine — fancy *general* vocabulary, idioms, long winding sentences send them to dictionary. Rewrite everything displayed — never copy source prose style:

- **Short sentences, one idea each.** 8–15 words. Two-three small sentences beat one packed 30-word sentence.
- **Common words.** *use/get/show/start/fast* — not *utilize/obtain/demonstrate/commence/rapidly*.
- **Keep technical terms; explain once.** *retention*, *token bucket*, *consistent hashing* stay — define on first use in plain words (hover glossary or flip card), ideally with analogy.
- **No idioms, culture-bound references, wordplay** ("cut to the chase", "silver bullet").
- **Active voice, concrete subjects.** "The server checks the bucket" — not "the bucket is checked".
- **Concrete numbers/examples over abstractions.** "You lose about half in 2 days" beats "substantial decay occurs rapidly".

Test: B1–B2 English learner reads every visible sentence without dictionary (technical terms excepted — page defines them).

## Analogies: fastest path to understanding

Every hard concept gets daily-life analogy — water, money, food, traffic, school, phone battery. Reader borrows existing intuition. Rules:

- Place **right next to concept** as own visual element — analogy callout (`💡 Think of it like…`) or flip-card front whose back explains real mechanism.
- Map the **mechanism**, not mood. "Token bucket = parking lot: cars (requests) take spaces (tokens); spaces free at fixed rate" maps; "guard at door" doesn't explain refill.
- Analogy breaks somewhere important → say where in one short sentence.
- ≥1 analogy per major concept; simulations pair well ("slider = 'how well you studied' knob").

## Workflow

### 1. Ingest

- **URL** → fetch. Course/multi-page → fetch every page/lesson/chapter before writing.
- **Local file** → read (pdf/docx skills for those formats).
- **Mangled math:** HTML-to-text extraction destroys `<math>`/MathML (Wikipedia especially). Equation-bearing content → recover from raw source; raw fails → rederive from surrounding prose (usually names variables/constants) — legitimate last resort.
- Source images: photos/screenshots → hotlink with `alt`; diagrams → redraw as inline SVG (sharper, themeable, offline).
- **Media inventory alongside text inventory.** Scan source for YouTube/Vimeo embeds and links (also inside `<iframe>`s + anchor hrefs), direct video files (.mp4/.webm), images. Record URL, title, surrounding descriptive text — media is content, must survive into page (see "Media" in references/patterns.md).

### 2. Inventory

Before any HTML: **content inventory** — flat list of every section/topic with key claims, figures, examples, code. Coverage checklist for step 7.

### 3. Map every inventory item to a visual form

Makes page visual instead of textual. Assign each item a representation — prose not on menu, only lead-sentence slot:

| Source material | Visual form (see references/patterns.md) |
|---|---|
| Concept / mechanism | **Diagram (SVG)** or **interactive simulation** |
| List of points / factors / tips | **Icon card grid** — one card per point |
| Process, steps, sequence | **Flow strip** — connected step cards + arrows |
| Comparison (X vs Y) | **Comparison matrix** ✅/❌/− |
| Numbers, statistics, results | **Stat tiles** — big number, tiny label, delta chip |
| Definitions / terminology | **Flip cards** or hover glossary |
| History, chronology, story arc | **Timeline** colored dots |
| Worked example | **Expandable** — closed by default |
| Caveat / warning / gotcha | **Callout chip** — one line, icon, color |
| Memorable claim or quote | **Pull quote** — large type, verbatim |
| Video (YouTube/Vimeo/file) | **Video toggle** — summary (if derivable) + real embed in collapsible |
| Hard / abstract concept | **Analogy callout** next to it (`💡 Think of it like…`) |
| Argument with sub-points | **Hero claim + supporting card row** |
| Relationships between entities | **Boxes-and-arrows SVG map**, labeled edges |

Nuanced essay argument → hero statement + card per sub-point, author's reasoning behind flips.

Guard second failure mode: wall of cards instead of gray paragraphs. Grid ≤ ~6–8 cards; past that split into labeled grids under sub-headings, or merge minor points into one card's flip back.

### 4. Architecture

1. **Kicker** — category · source · time-to-understand (small, muted)
2. **H1 + one-line subtitle** — payoff in one sentence
3. **Gist row** — 3–5 stat tiles/chips = core takeaways at glance (replaces prose TL;DR)
4. **Sticky TOC** — one click anywhere
5. **Visual blocks** — one per source section: optional 1–2 lead sentences, then visual. Simulations/diagrams go where they teach best — usually right after concept they animate, even if source introduces math later
6. **Glossary / self-check** — flip cards or quiz near end
7. **Footer** — source link, generation date

### 5. Choose interactive elements

Every element must reduce effort to understand, not add chrome.

- **Simulations: highest payoff.** Build when concept has parameters/state (algorithm behavior, system under load, curve responding to inputs, feedback loop). Small: 1–3 controls, SVG stage re-rendering instantly, one-line readout naming what changed, reset. One good sim beats three decorative.
- **Flip cards / hover-reveals** = workhorse: hide 80% of words, one click away.
- **Quiz / flashcards** — default short quiz whenever user is learning (not just formal courses). Each "why" line = second chance to teach. Answers + scores persist in `localStorage` (skeleton handler); stable `data-quiz` id + reset button.
- **Progress persists via library server.** Page JS can't write to disk (browser sandbox, http or file). Skeleton sync probes `GET /__progress?site=<page's directory URL>` on load — served by user's Explainers library server → every quiz answer, checkbox, theme change auto-saved to library's SQLite store, restored any later visit any browser. Opened as file → silent fallback to `localStorage` + **⤓ save / ⤒ load buttons** (manual `progress.json` export/import).
- **Exercises / sim missions.** 1–2 challenges under sim ("Try: set strength to 8. How much is left after 7 days?"). Doing beats watching.
- **Progress bar** for long pages. **Dark mode default** (`<html data-theme="dark">`); toggle → light, persists via `localStorage` (`try{}`-guarded) so multi-page outputs keep theme across navigation.

### 6. Build

Start from `assets/skeleton.html` (tokens, layout, TOC scrollspy, progress bar, theme toggle, card grids, flip cards, flow strip, tabs, quiz handler — all wired). Read `references/patterns.md` for snippets. Delete what content doesn't need; near threshold, keeping working code fine.

Hard requirements:

- **One self-contained file.** Inline CSS/JS. No CDN, no build step, no network dependency except hotlinked photos. Works from `file://` offline.
- **Vanilla JS, inline SVG** for all diagrams/charts (hand-roll — rectangles and paths).
- **Icons:** emoji ~1.5rem fine; inline-SVG glyphs for polish. Every card gets one — icons let grid scan without reading.
- **Design tokens top of CSS** — one accent, muted palette, type scale.
- Semantic HTML (`<section>`, `<nav>`, `<details>`, real buttons).

Thariq-style: generous whitespace, numbered section kickers (01, 02…), small colored chips, single accent, strong hierarchy. Card titles = short headlines; bodies = 2–3 small natural plain-English sentences — clipped fragments look sleek, read badly for non-native speakers.

### 7. Verify

- **Coverage pass:** walk inventory; every item maps to visible element or interaction.
- **Text-budget pass:** >~80 visible words outside visuals per section? Visible paragraph anywhere? Convert or hide.
- **Language pass:** read every visible sentence as B1–B2 learner. Fancy word, idiom, 25-word sentence → simplify. Hard concept without nearby analogy → add one.
- **Squint test:** shapes and colors, not gray columns?
- **Mechanical pass:** every TOC href resolves; flips/tabs/quiz/sim JS self-consistent (re-read own script); no unclosed tags.
- **Scale check:** source >~15,000 words → `index.html` with module cards + one file per module, cross-linked, shared token block.

### 8. Deliver

**Deliver into the library.** User keeps Explainers library — folder containing `explainers_server/` (at `~/Explainers`, sites under `Courses/`). Save site as new subfolder (e.g. `Courses/WebFlies/<Site Name>/`). Library server serves at `http://localhost:8765/courses/...`, lists on dashboard, saves all progress to single SQLite store zero-setup — page sync + server speak same `/__progress?site=<dir>` contract. Ask for folder access if missing; library genuinely unavailable → save `.html` to normal output folder (works opened directly via localStorage fallback).

**Always write `site.json`** next to HTML — dashboard filters feed on it:

```json
{
  "title": "Claude 101 — Basics",
  "provider": "Anthropic Academy",
  "price": "free",
  "duration": "2h 30m",
  "lessons": 12,
  "tags": ["claude", "ai", "beginner"]
}
```

Fill what source reveals, omit rest (every field optional): `provider` = platform/author; `price` = `"free"`/`"paid"` if stated; `duration` = stated length or reading-time estimate; `lessons` = module count (or page count); `tags` = 3–6 short lowercase topic tags; `added` = today (ISO).

Skeleton pagebar carries **⌂ library** link (accent, top-left, shown over http) + **completion circle** per lesson page. Auto-completion: quiz pages complete when every question answered; no-quiz pages after ~2 min visible reading once reader hit bottom. Manual toggle stays. Persists as `done:<page path>`; index pages show green ✓ on completed lesson links; dashboard derives course completion (✓ ring, n/m bar, status filter) from same keys. Circle hides on `index.html`.

Source course awards certificate → final certificate page with **upload box** — never render fake certificate. Reader uploads real one (pdf/png/jpg/webp) via `POST /__cert?site=<dir>&name=<filename>` (raw body; check `GET /__cert?site=<dir>`). Display uploaded cert **inline** — `<img>` for images, `<iframe>` viewer for PDFs (cache-bust URL so replacements show) — plus "open in new tab" link. Page marks done only when every other lesson done AND certificate uploaded.

Present result one line: what it is, `http://localhost:8765/...` URL.

## Example invocations

**Input:** "Make this visual so I don't have to read it all: https://example.com/12-rules-for-api-design"
**Output:** One page — gist row (3 chips), 12 icon cards (rule headline, 2-line gist, flip = author's full reasoning + code verbatim), comparison matrix, SVG decision flow, glossary flip cards. Visible body text under 400 words; original ~4,000.

**Input:** "Turn this 8-module course into something I can study fast" + folder of markdown
**Output:** `index.html` with 8 module cards + progress; each module page = concept diagram or simulation, flow strip, flip-card vocabulary, 5-question quiz. Every lesson topic + code sample present — code verbatim inside expandables.
