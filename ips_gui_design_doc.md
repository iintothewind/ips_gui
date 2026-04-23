# Technical Design Document: `ips_gui`

## 1. Project Identity

- **Name:** `ips_gui`
- **Language:** Rust (2021 edition)
- **Goal:** A native desktop GUI frontend for the `ips` (Image Prompt Search) library. Exposes all core search parameters through a graphical interface and displays results with inline match highlighting.
- **Relationship to `ips_cli`:** `ips_gui` depends on `ips` (the library target of `ips_cli`) as a path dependency. No search logic is duplicated — the GUI is a pure presentation layer over the existing pipeline.

---

## 2. Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                    ips_gui binary                   │
│                                                     │
│  ┌──────────────┐     mpsc channel     ┌──────────┐ │
│  │  UI Thread   │ ←─────────────────── │  Search  │ │
│  │  (eframe)    │                      │  Thread  │ │
│  │              │ ──── Config ────────→ │  (std)   │ │
│  └──────────────┘                      └──────────┘ │
│         │                                    │       │
│         │ renders                            │ uses  │
│         ▼                                   ▼       │
│    egui panels                       ips library    │
│                                   (discover/extract  │
│                                    /match via rayon) │
└─────────────────────────────────────────────────────┘
```

The application is single-window with two concurrent actors:

1. **UI thread** — runs the `eframe` event loop. Renders panels, polls the channel for search results, and handles all user input.
2. **Search thread** — spawned by `std::thread::spawn` on each search invocation. Calls the `ips` library pipeline synchronously, then sends a single message back through an `mpsc` channel when done.

The UI thread never blocks waiting for results; it calls `rx.try_recv()` on every frame and schedules a repaint at 80 ms intervals while a search is in flight.

---

## 3. Technical Stack

| Concern | Crate | Notes |
|---|---|---|
| GUI framework | `eframe` v0.29 | Wraps `egui` + `winit` + `wgpu`/`glow` |
| UI widgets | `egui` v0.29 | Immediate-mode retained-state UI |
| Native file dialogs | `rfd` v0.14 | Folder picker (Browse…) and save dialogs |
| Core search pipeline | `ips` (path dep) | Discovery, extraction, matching |
| Parallel extraction | `rayon` v1 | Used inside the search thread via `par_iter` |
| JSON export | `serde` + `serde_json` v1 | Pretty-printed output |
| CSV export | `csv` v1 | RFC 4180 compliant writer |
| Regex validation | `regex` v1 | Pre-validates regex queries before thread spawn |

### Crates explicitly NOT added

- `tokio` / async runtime — not needed; the search thread is synchronous.
- `image` / pixel decoders — not needed; inherited from `ips` which avoids them.
- Any additional UI toolkit — egui covers all required widgets.

---

## 4. Application State

All mutable state lives in `IpsGuiApp`, which implements `eframe::App`:

```rust
struct IpsGuiApp {
    // Input parameters (bound directly to widgets)
    query: String,
    search_path: String,
    match_mode: MatchModeOpt,   // Exact | Fuzzy | Regex
    min_score: i64,             // fuzzy threshold, 0–100
    full_prompt: bool,
    no_recursive: bool,
    verbose: bool,
    depth_str: String,          // raw text input, parsed to Option<usize>

    // Search runtime state
    searching: bool,
    results: Vec<MatchResult>,
    status_msg: String,
    error_msg: Option<String>,
    rx: Option<Receiver<SearchMsg>>,
}
```

`MatchModeOpt` is a local enum that mirrors `ips::types::MatchMode` but derives `PartialEq + Clone`, which `MatchMode` does not derive, making it suitable for use with `egui::selectable_value`.

`depth_str` is stored as a raw string rather than `Option<usize>` so that the text field can hold partially-typed input without losing characters mid-edit. It is parsed at search time with `.trim().parse().ok()`.

---

## 5. Search Lifecycle

```
user clicks Search
       │
       ▼
IpsGuiApp::start_search()
  │  validate: query non-empty, not already searching
  │  if Regex mode: compile regex, bail with error_msg on failure
  │  build ips::types::Config from current widget state
  │  spawn std::thread
  │  set self.searching = true
  │  store Receiver in self.rx
       │
       ▼  (background thread)
  discover_files(&config)          → Vec<PathBuf>
  files.par_iter()
    .flat_map(|path| {
        extract_prompt(path, verbose)
          .filter_map(|rec| match_record(&rec, &config))
    })
    .collect()                     → Vec<MatchResult>
  sort by path
  tx.send(SearchMsg::Done(results, elapsed))
  ctx.request_repaint()
       │
       ▼  (UI thread, next frame)
  rx.try_recv() → Ok(SearchMsg::Done(...))
  self.results = results
  self.searching = false
  self.status_msg = "Found N result(s) in X.XXs"
```

`ctx.request_repaint()` is called from the search thread after sending so the UI wakes immediately rather than waiting for the next scheduled repaint.

While the search is running, `update()` calls `ctx.request_repaint_after(80ms)` on `TryRecvError::Empty` to keep the spinner animating without busy-looping.

---

## 6. UI Layout

The window is divided into three fixed regions via egui's panel system:

```
egui::SidePanel::left("params")     — 275px default, resizable, min 200px
egui::TopBottomPanel::bottom("status_bar")
egui::CentralPanel::default()       — fills remaining space
```

Panels are declared in this order so egui allocates space correctly (left → bottom → center).

### Left Panel — Search Parameters

A `ScrollArea::vertical` wraps all controls so the panel remains usable at small window heights. Controls from top to bottom:

| Widget | egui type |
|---|---|
| Query text field | `TextEdit::singleline` with hint text |
| Directory text + Browse button | `horizontal` layout, `TextEdit` + `Button` |
| Match Mode selector | `horizontal` with three `selectable_value` buttons |
| Min Score slider | `Slider` (shown only when Fuzzy selected) |
| Max Depth text field | `TextEdit::singleline` with hint text |
| Option checkboxes | Three `checkbox` widgets |
| Search button | `Button` inside `add_enabled_ui` guard |
| Export buttons | `horizontal` layout, JSON + CSV, shown only when `results` is non-empty |

The Search button is disabled via `add_enabled_ui(!self.searching && !self.query.is_empty(), ...)` — egui greys out all children automatically.

### Status Bar — Bottom Panel

Renders one of three states:
- **Searching:** `ui.spinner()` + label "Searching…"
- **Error:** `colored_label` in red with the error string
- **Idle/done:** plain label with `self.status_msg`

### Central Panel — Results

A `ScrollArea::vertical` contains one result card per `MatchResult`, rendered by `draw_result()`. Results are pre-sorted by file path before being stored in `self.results`.

---

## 7. Result Card Rendering

Each result is rendered by `fn draw_result(ui, result, full)` inside an `egui::Frame` with:

- Subtle background tint (`rgba(255,255,255,10)` dark / `rgba(0,0,0,10)` light) for card separation without hard borders.
- 10px inner margin, 6px rounding.

**Header row** (`horizontal_wrapped`):
- File path in cyan bold (`RichText`)
- Generator tag in grey small (`[a1111]`)
- Fuzzy score in green small (`score:120`), rendered only when `result.score` is `Some`

**Prompt text** is rendered via `egui::text::LayoutJob`, which allows mixing text segments with different `TextFormat` values in a single wrapping label:

```
normal_fmt  — body text color, 13pt proportional
hi_fmt      — yellow foreground + faint yellow background (match highlight)
gray_fmt    — grey color (trailing ellipsis)
```

The highlight walk iterates `result.match_ranges: Vec<(usize, usize)>` — byte positions in the prompt string. For each range `(start, end)`:

1. Append `prompt[pos..start]` in `normal_fmt`
2. Append `prompt[start..end]` in `hi_fmt`
3. Advance `pos = end`

After all ranges, append any trailing text in `normal_fmt`. If the prompt was truncated, append `" …"` in `gray_fmt`.

Truncation is computed on char boundaries (`char_indices().nth(500)`) to avoid splitting multi-byte UTF-8 sequences. All range clamping uses `.min(display_end)` so out-of-window ranges are silently skipped rather than panicking.

---

## 8. Export

Both export functions follow the same pattern:

1. Open a native save dialog via `rfd::FileDialog::new().save_file()`.
2. If the user cancels (returns `None`), return immediately — no error, no state change.
3. Serialize `self.results` and write to the chosen path.

**JSON export** uses an inline `#[derive(Serialize)]` struct `Row` with a `#[serde(skip_serializing_if = "Option::is_none")]` annotation on `score` so exact-mode results emit a clean object without a null score field.

**CSV export** uses `csv::Writer::from_path`. The header row is always written. Score is serialized as an empty string in non-fuzzy modes, matching the CLI CSV output format.

Errors from file I/O are silently discarded. A future improvement would surface them in `error_msg`.

---

## 9. Threading Model

| Actor | Thread | Blocking operations |
|---|---|---|
| UI event loop | Main thread | None — never blocks |
| Search | `std::thread::spawn` | `discover_files` (I/O), `par_iter` extraction (CPU) |

Only one search thread runs at a time. The Search button is disabled while `self.searching` is true, so a second invocation cannot be started. The thread is not cancellable in v1; once started it runs to completion.

Rayon uses its global thread pool for parallel extraction inside the search thread. Because `build_global` can only be called once per process and `ips_cli` may call it with a custom thread count, `ips_gui` passes `threads: None` in `Config`, leaving the global pool at its default size (number of logical CPUs).

---

## 10. Project Structure

```
ips_gui/
├── Cargo.toml               # dependencies: eframe, ips (path), rfd, rayon, serde*, csv, regex
├── README.md
├── ips_gui_design_doc.md
└── src/
    └── main.rs              # entire application: state, UI, search, export
```

The single-file structure is appropriate at this scale (~500 lines). If the project grows, natural split points are:

- `app.rs` — `IpsGuiApp` struct and `eframe::App` impl
- `search.rs` — `start_search` and the background thread
- `export.rs` — JSON and CSV export functions
- `widgets.rs` — `draw_result` and any reusable widget helpers

---

## 11. Key Design Decisions

### Immediate-mode UI (egui) over retained-mode

egui rebuilds the entire UI on every frame from the current state struct. This eliminates the need for data-binding, view models, or observer patterns. The tradeoff is that CPU usage scales with repaint rate; mitigated here by `request_repaint_after(80ms)` only while searching, and no forced repaint at other times.

### Synchronous search thread over async

The `ips` pipeline uses rayon (a synchronous parallel iterator library) and has no async entry points. Wrapping it in tokio would add complexity and binary size with no benefit. A plain `std::thread` is the correct choice.

### Single `mpsc` channel per search

Using `mpsc::channel` + `try_recv` instead of `Arc<Mutex<Option<Vec<MatchResult>>>>` avoids lock contention and is idiomatic for one-shot background tasks. The Receiver is stored in `Option<Receiver<...>>` and set to `None` once a message is received, which also drops the channel and signals completion.

### `MatchModeOpt` local enum

`ips::types::MatchMode` does not implement `PartialEq` or `Clone`, which egui's `selectable_value` requires. Rather than modifying the library, `ips_gui` defines its own `MatchModeOpt` enum and maps it to `MatchMode` at Config construction time. This keeps the GUI layer decoupled from the library's internal type requirements.

### Regex validated before thread spawn

If the regex is invalid, constructing `Config` with a bad pattern and then calling `match_record` in the thread would panic or return unexpected results. Validating with `regex::Regex::new(&self.query)` on the UI thread before spawning gives an immediate, user-visible error in `error_msg` without involving the background thread at all.

---

## 12. Future Work

| Feature | Notes |
|---|---|
| Cancellable search | Store a `Arc<AtomicBool>` cancel token; check it inside the `flat_map` closure |
| Result count during search | Send incremental `SearchMsg::Progress(n)` variants as batches complete |
| Clickable file path | `ui.hyperlink_to` or `open::that(path)` to open the image in the default viewer |
| Copy prompt to clipboard | `arboard` (already a transitive dep via egui) |
| Result filtering / sorting | Secondary sort controls in the results panel header |
| Persistent settings | Serialize `IpsGuiApp` parameters to a JSON config file on exit, restore on startup |
| Dark/light theme toggle | `ctx.set_visuals(egui::Visuals::dark())` wired to a button |
| Export error feedback | Surface `std::io::Error` from write operations in `error_msg` |
| Thread count control | Wire a thread count field to a process-level `rayon::ThreadPoolBuilder::build_global` call at startup (one-time, not per search) |
