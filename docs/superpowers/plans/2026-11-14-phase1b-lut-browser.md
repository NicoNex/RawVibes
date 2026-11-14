# Phase 1b — LUT browser inside the editor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Browse the LUT library from the Grade sidebar and see each LUT
live on the photo while stepping through them with the keyboard or mouse;
the Compare LUTs page goes away.

**Architecture:** The existing gallery pane (folder row, sub-folder
dropdown, search, `FlowBox` of thumbnails) moves from the Compare page into
the sidebar's LUTs section. The selected (or hovered) LUT becomes a
*candidate* layer appended after the look stack only for the preview, so
the main canvas, split, scopes and filmstrip see it exactly as if it were
added. Enter / double-click commits the candidate to the look stack;
Escape drops it. Thumbnails always render on the graded photo.

**Tech Stack:** Rust 2024, relm4 0.11 / GTK4 / libadwaita, wgpu.

**Spec:** `docs/superpowers/specs/2026-11-14-rawvalley-design.md` (UI → "LUT browsing lives in the editor").

## Global Constraints

- English strings, Italian `msgstr` in `po/it.po` in the same commit (`make pot`, then fill).
- LUT Lab stays a separate page; only Compare LUTs is removed.
- `make check` passes at every commit; commit trailer as in phase 1.
- Keyboard: arrows step through visible LUTs (FlowBox native 2-D
  navigation), Enter adds, Escape drops the preview; works while the
  pointer is anywhere once the browser has focus.

## UX bar (author: "la UX deve essere sempre al top")

- Instant feedback: a candidate shows on the photo within one frame of
  the selection moving (LUT load + bake off the main thread, latest wins);
  the thumbnail being previewed is highlighted.
- Hover preview only after the pointer rests ~120 ms on a card, so
  sweeping across the grid does not flicker the photo.
- The browser keeps keyboard focus after Enter/Escape so the next arrow
  press keeps browsing; after commit the new look row flashes once
  (`.accent` pulse) so the user sees where it went.
- Empty states say what to do: no folder → "Choose a LUT folder" button;
  no match for the search → "No LUTs match".
- Every control has a tooltip with its shortcut and an accessible label.

## Review Focus

1. Search/sub-folder filter hiding the selected LUT: the preview must drop
   (no invisible LUT applied).
2. A LUT file that fails to load: toast, no stale candidate left applied.
3. Changing the look stack (add/remove/reorder/intensity) while a
   candidate is previewed: candidate stays last and the stack is not
   duplicated on commit.
4. Escape while editing the crop must still cancel the crop, not the
   LUT preview (crop wins while editing).
5. Grade changes while the browser is open: thumbnails refresh once the
   grade settles (debounced), never on every slider tick.

---

### Task 1: Gallery pane in the LUTs section; thumbnails on the graded photo

**Files:**
- Modify: `gtk/src/main.rs` (move `gallery` vbox into `build_sidebar`'s
  LUTs section; drop the "Original / With grade" toggle and
  `compare_graded`; thumbnail refresh scheduling)
- Modify: `po/it.po`

**Interfaces:**
- Produces: `Msg::Browse(bool)` (browser expanded/collapsed),
  `App.browsing: bool`.

- [ ] **Step 1:** LUTs section body: the look stack as today, then a
  `gtk::Expander`-style row "Browse LUTs" (toggle button with
  `view-grid-symbolic`, `B` shortcut) revealing the gallery pane:
  folder row (folder name, reload, choose folder, open in LUT Lab),
  sub-folder dropdown, search, and the `FlowBox` in a `ScrolledWindow`
  with `min_content_height(240)`, `max_content_height(420)`,
  `propagate_natural_height(true)`. FlowBox: `min/max_children_per_line(2)`,
  thumbnails `height_request(64)`.
- [ ] **Step 2:** Thumbnails always on the graded base (`compare_graded`
  removed, `sync_compare_base` uses the graded branch). They render when
  the browser opens and, while open, after the grade settles: reuse the
  `ROLL_SETTLE` tick (`Msg::RollTick`) to call `sync_compare_base()` and
  `render_gallery()` when `browsing`.
- [ ] **Step 3:** `make check`; run: open the browser, thumbnails appear
  on the graded photo; move a slider, thumbnails refresh once after it
  settles. Commit `LUT browser in the sidebar`.

### Task 2: Live candidate preview, commit and cancel

**Files:**
- Modify: `gtk/src/main.rs`
- Modify: `po/it.po`

**Interfaces:**
- Consumes: `LabLayer`, `push_lut`, `look_layers`, `Job::SetLook`.
- Produces: `App.candidate: Option<(usize, LabLayer)>`,
  `Msg::Preview(Option<usize>)` (hover/selection), `Msg::PreviewLoaded(usize, Result<Arc<Lut3d>, String>)`,
  `Msg::CommitCandidate`, `Msg::DropCandidate`.

- [ ] **Step 1:** `look_layers()` appends the candidate (intensity 1,
  input `Profile::guess_from_name`) after the looks, also when
  `looks_off` is false only; exported LUTs and presets never include it
  (`Export` uses the looks without the candidate).
- [ ] **Step 2:** Selection change (`selected-children-changed`) →
  `Preview(Some(i))`; pointer entering a card → `Preview(Some(i))`,
  leaving the FlowBox → `Preview(selected)`. Loading off the main thread,
  latest request wins (index check on `PreviewLoaded`); errors toast and
  clear the candidate. Filtered-out selection → `Preview(None)`.
- [ ] **Step 3:** `child-activated` (double-click, Enter) →
  `CommitCandidate`: pushes the candidate into `looks` through
  `push_layer` and clears it. Escape → `DropCandidate` unless the crop is
  being edited (crop cancel keeps priority). The "Add LUT" menu's
  "selected" entry uses the candidate.
- [ ] **Step 4:** Italian strings; `make check`; run: arrow through LUTs
  and see them on the photo; split shows before/after; Enter adds; Escape
  restores. Commit `Preview LUTs live while browsing`.

### Task 3: Remove the Compare LUTs page

**Files:**
- Modify: `gtk/src/main.rs` (page, `compare_*` widgets, `Page::Compare`,
  `render_compare`, `SelectLut` compare path, `Alt+2` → LUT Lab,
  shortcuts dialog, `OpenInLab`)
- Modify: `core/src/gpu.rs` (`Job::ApplyLut`, `Done::Compared`,
  `Slot::Compare`, worker `selected`)
- Modify: `po/it.po`

- [ ] **Step 1:** Delete the page and everything only it used; `grep -n
  "Compare\|compare_\|ApplyLut\|Compared"` finds only the before/after
  `Msg::Compare` and `CompareBase`.
- [ ] **Step 2:** `make check`; run: header shows Grade and LUT Lab only;
  `Alt+1`/`Alt+2` switch them. Commit `Remove the Compare LUTs page`.

### Phase close

- [ ] `/code-review high` then `/simplify` on the phase diff, fix, `make check`, commit.
