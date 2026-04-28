# Technical Design Document: `ips_gui`

## 1. Project Identity

- **Name:** `ips_gui`
- **Language:** Rust (2021 edition)
- **Goal:** A native desktop GUI for searching AI-generated image prompts embedded in PNG, JPEG, and WebP metadata. Displays results as a thumbnail grid with a detail view for individual images.
- **Self-contained:** The `ips` search library is embedded directly in `src/ips/` as an internal module. No external path dependency on `ips_cli` — both projects are independently maintained.

---

## 2. Architecture Overview

```text
┌──────────────────────────────────────────────────────────────┐
│                        ips_gui binary                        │
│                                                              │
│  ┌──────────────┐   mpsc channel    ┌──────────────────────┐ │
│  │  UI Thread   │ ←──────────────── │   Search Thread      │ │
│  │  (eframe)    │                   │   (std::thread)      │ │
│  │              │ ─── Config ──────→ │   discover_files     │ │
│  └──────────────┘                   │   extract_prompt     │ │
│         │                           │   match_record       │ │
│         │ thumb / full-res          │   (rayon par_iter)   │ │
│         │ requests                  └──────────────────────┘ │
│         ▼                                                    │
│  ┌──────────────┐                                            │
│  │ Thumb Pool   │  rayon::ThreadPool (4 threads)             │
│  │              │  load_thumbnail → ColorImage (≤300 px)     │
│  │              │  load_full_res  → ColorImage (native res)  │
│  └──────────────┘                                            │
└──────────────────────────────────────────────────────────────┘
```

Three concurrent actors:

1. **UI thread** — runs the `eframe` event loop. Renders panels, polls channels, handles all input.
2. **Search thread** — spawned by `std::thread::spawn` per search. Runs the full discovery → extraction → matching pipeline, sends one `SearchMsg::Done` back when complete.
3. **Thumbnail pool** — a dedicated `rayon::ThreadPool` (4 threads) that loads and decodes both thumbnails and full-resolution images on demand. Results are sent back to the UI thread via two separate `mpsc` channels.

The UI thread never blocks. It uses `try_recv()` on every frame for all channels.

---

## 3. Technical Stack

| Concern | Crate | Notes |
|---|---|---|
| GUI framework | `eframe` v0.29 | Wraps `egui` + `winit` + `wgpu`/`glow` |
| UI widgets | `egui` v0.29 | Immediate-mode UI |
| Native file dialogs | `rfd` v0.14 | Folder picker and save dialogs |
| Image thumbnail decoding | `image` v0.25 | PNG, JPEG, WebP; used only for preview thumbnails |
| File discovery | `walkdir` v2 | Recursive directory traversal |
| Fuzzy matching | `fuzzy-matcher` v0.3 | Skim algorithm (`SkimMatcherV2`) |
| Parallel extraction | `rayon` v1 | Used in both search thread and thumbnail pool |
| JSON export | `serde` + `serde_json` v1 | Pretty-printed output |
| CSV export | `csv` v1 | RFC 4180 compliant writer |
| Regex validation | `regex` v1 | Pre-validates queries before thread spawn |

---

## 4. Application State

All mutable state lives in `IpsGuiApp`, which implements `eframe::App`:

```rust
struct IpsGuiApp {
    // Search parameters (bound to left-panel widgets)
    query: String,
    search_path: String,
    match_mode: MatchModeOpt,       // Exact | Fuzzy | Regex
    min_score: i64,                 // fuzzy threshold, 0–100
    no_recursive: bool,
    verbose: bool,
    depth_str: String,              // raw text, parsed to Option<usize> at search time
    search_within_results: bool,

    // Search runtime
    searching: bool,
    results: Vec<MatchResult>,
    status_msg: String,
    error_msg: Option<String>,
    rx: Option<Receiver<SearchMsg>>,

    // View
    view_mode: ViewMode,            // Grid | Detail(usize)
    detail_image_enlarged: bool,    // true while detail view shows the full-res image

    // Thumbnail loading (grid + detail preview)
    thumb_pool: rayon::ThreadPool,
    thumb_result_tx: Sender<(PathBuf, Option<ColorImage>)>,
    thumb_rx: Receiver<(PathBuf, Option<ColorImage>)>,
    thumb_queued: HashSet<PathBuf>,
    thumbnails: HashMap<PathBuf, ThumbState>,

    // Full-resolution loading (detail enlarged view)
    full_res_tx: Sender<(PathBuf, Option<ColorImage>)>,
    full_res_rx: Receiver<(PathBuf, Option<ColorImage>)>,
    full_res_queued: HashSet<PathBuf>,
    full_res: HashMap<PathBuf, ThumbState>,
}
```

`MatchModeOpt` is a local enum mirroring `ips::types::MatchMode` with `PartialEq + Clone` added, which `egui::selectable_value` requires.

`depth_str` is a raw string so the text field can hold partially-typed input without losing characters. Parsed at search time with `.trim().parse().ok()`.

`ThumbState` is `Loaded(TextureHandle) | Failed`. Thumbnail textures are evicted when no longer visible in the grid (via `retain`). Full-res textures are dropped when navigating away from a detail item.

---

## 5. Search Lifecycle

Two code paths share the same Config construction and channel machinery:

### 5.1 Full search (filesystem scan)

```text
user clicks Search (search_within_results = false, or results empty)
       │
       ▼
IpsGuiApp::start_search()
  validate: query non-empty, not already searching
  if Regex: compile, bail with error_msg on failure
  build Config; clear thumbnails
  spawn std::thread
       │
       ▼  (background thread)
  discover_files(&config)        → Vec<PathBuf>
  files.par_iter()
    .flat_map(|path|
        extract_prompt(path, verbose)
          .filter_map(|rec| match_record(&rec, &config))
    )
    .collect()                   → Vec<MatchResult>
  sort by path
  tx.send(SearchMsg::Done(results, elapsed))
  ctx.request_repaint()
       │
       ▼  (UI thread, next frame)
  rx.try_recv() → Ok(...)
  self.results = results; self.searching = false
```

### 5.2 Search within results (re-filter)

```text
user clicks Search (search_within_results = true, results non-empty)
       │
       ▼
IpsGuiApp::start_search()
  same validation and Config build
  clone records: Vec<PromptRecord> from self.results
  spawn std::thread  (thumbnails NOT cleared)
       │
       ▼  (background thread)
  records.par_iter()
    .filter_map(|rec| match_record(rec, &config))
    .collect()                   → Vec<MatchResult>
  sort by path
  tx.send(SearchMsg::Done(results, elapsed))
  ctx.request_repaint()
```

Skips `discover_files` and `extract_prompt` entirely. Works on already-extracted `PromptRecord` values. Can be chained multiple times to progressively narrow a result set.

---

## 6. UI Layout

```text
egui::SidePanel::left("params")       — 30 % of window width, fixed
egui::TopBottomPanel::bottom("status_bar")
egui::CentralPanel::default()         — fills remaining space
```

### Left panel — Search Parameters

Controls top to bottom:

| Widget | egui type |
|---|---|
| Query text field | `TextEdit::singleline` with hint text |
| Directory + Browse | `horizontal`: `TextEdit` + `Button` (opens `rfd::FileDialog`) |
| Match Mode selector | `horizontal` with three `selectable_value` buttons |
| Min Score slider | `Slider` (Fuzzy mode only) |
| Max Depth text field | `TextEdit::singleline` |
| Top-level only checkbox | `checkbox` |
| Search within results checkbox | `checkbox` — visible only when `results` is non-empty |
| Search button | `Button` inside `add_enabled_ui(!searching && !query.is_empty(), ...)` |
| Export buttons | `horizontal`: JSON + CSV — visible only when `results` is non-empty |

### Central panel — Grid view

Results are displayed as a virtualised grid of 100×100 px thumbnail cells (`GRID_THUMB = 100`, `GRID_GAP = 4`). The column count is computed dynamically from the available panel width:

```rust
let cols = ((avail_w + GRID_GAP) / (GRID_THUMB + GRID_GAP)).floor().max(1.0) as usize;
```

`egui::ScrollArea::show_rows` is used for virtual scrolling — only visible rows are rendered, keeping frame time constant regardless of result count.

Each cell:
- Draws a background rectangle and border (brighter on hover)
- Shows a loaded texture, a ✕ on decode failure, or a 📄 icon for non-image files
- Triggers `ViewMode::Detail(idx)` on click

### Central panel — Detail view

Replaces the grid when `view_mode == ViewMode::Detail(idx)`. Two sub-layouts controlled by `detail_image_enlarged`.

**Normal layout** (`detail_image_enlarged = false`):

```text
[ ◀ Back ]  [ ← Prev ]  [ Next → ]   idx / total
─────────────────────────────────────────────────
┌─ image ─┐  │  filename (bold 16pt)
│ 300×300 │  │  path (cyan small)  [📋 Copy path]
│ preview │  │  Generator: …
└─────────┘  │  Score: …  (fuzzy only)
  (click)    │  ──────────────
             │  Prompt:
             │  (full prompt text, wrapping label)
```

Clicking the thumbnail sets `detail_image_enlarged = true` and fires `request_full_res`. If the thumbnail failed to load (file missing or unreadable), a ✕ placeholder is shown and clicking is disabled.

**Enlarged layout** (`detail_image_enlarged = true`):

```text
[ ◀ Back ]  [ ← Prev ]  [ Next → ]   idx / total
─────────────────────────────────────────────────
┌──────────────────────────────────────────────┐
│                                              │
│          full-res image, aspect-fitted       │
│          centred in available space          │
│                                              │
└──────────────────────────────────────────────┘
              (click to restore)
```

While the full-res image is loading, a spinner and "Loading…" label are shown. If the load fails (file deleted between request and completion), a red "Failed to load image" message is shown instead.

Keyboard events are consumed with `ui.input(|i| ...)` on every frame:

| Key | Action |
|---|---|
| `←` | `Detail(idx - 1)` if `idx > 0` |
| `→` | `Detail(idx + 1)` if `idx + 1 < total` |
| `Esc` | Collapse enlarged image if open; otherwise `Grid` |

---

## 7. Image Loading

Both thumbnail and full-resolution loading share the same `rayon::ThreadPool` (`THUMB_THREADS = 4`) and the same `ThumbState` enum, but use separate channels and caches so they are independently managed.

### 7.1 Thumbnail loading

1. **Request** — `request_thumb(path, ctx)` checks `thumbnails` and `thumb_queued`; if absent from both, inserts into `thumb_queued` and spawns a pool task.
2. **Decode** — pool task calls `load_thumbnail`: opens the file with the `image` crate, scales to at most 300×300 px (`thumbnail(300, 300)`), converts to `RGBA8`, wraps in `egui::ColorImage`.
3. **Return** — task sends `(path, Option<ColorImage>)` over `thumb_result_tx` and calls `ctx.request_repaint()`.
4. **Upload** — `poll_thumbs()` called each frame drains `thumb_rx`, uploads decoded images to GPU via `ctx.load_texture`, stores `ThumbState::Loaded(handle)` or `ThumbState::Failed`.

**Eviction** (grid mode only): after rendering, `thumbnails.retain` and `thumb_queued.retain` discard entries whose paths are not in the currently visible `grid_vis` set. Detail mode skips eviction to avoid re-decoding when navigating back.

### 7.2 Full-resolution loading

Triggered when the user clicks the thumbnail in detail view to enlarge it.

1. **Request** — `request_full_res(path, ctx)` checks `full_res` and `full_res_queued`. If the thumbnail already has `ThumbState::Failed` for this path, the request is skipped entirely (file known missing). Otherwise inserts into `full_res_queued` and spawns a pool task.
2. **Decode** — pool task calls `load_full_res`: opens the file at native resolution (capped at 4096×4096 px to bound memory), converts to `RGBA8`, wraps in `egui::ColorImage`.
3. **Return** — task sends `(path, Option<ColorImage>)` over `full_res_tx`.
4. **Upload** — `poll_full_res()` called each frame drains `full_res_rx`. Before inserting, it calls `full_res_queued.remove(&path)`: if the path is no longer queued (because navigation cleared it), the result is discarded immediately and the texture is never stored. This prevents stale in-flight loads from leaking GPU memory after the user has navigated away.

**Eviction**: `full_res` and `full_res_queued` are both cleared whenever `next_view` is set (Back, Prev, Next, Esc-to-grid). Dropping `ThumbState::Loaded(handle)` releases the `TextureHandle`, which decrements the egui reference count and frees the GPU texture.

### 7.3 Error display

| State | Thumbnail area | Enlarged area |
|---|---|---|
| Loading | ⏳ spinner placeholder | Spinner + "Loading…" label |
| `ThumbState::Failed` | ✕ icon + "File not found" label | Red "Failed to load image" label |
| `ThumbState::Loaded` | Scaled image (clickable) | Full-res image fitted to panel (clickable) |

---

## 8. Export

Both export functions:

1. Open a native save dialog via `rfd::FileDialog::new().save_file()`.
2. Return immediately if the user cancels.
3. Serialize `self.results` and write to the chosen path.

**JSON** uses an inline `#[derive(Serialize)]` struct with `#[serde(skip_serializing_if = "Option::is_none")]` on `score`, so exact-mode results omit the score field entirely.

**CSV** uses `csv::Writer::from_path`. Score is an empty string in non-fuzzy modes.

I/O errors are silently discarded; a future improvement would surface them via `error_msg`.

---

## 9. Threading Model

| Actor | Thread | Blocking operations |
|---|---|---|
| UI event loop | Main thread | None |
| Search | `std::thread::spawn` (one at a time) | `discover_files` (I/O), `par_iter` extraction + matching (CPU) |
| Thumbnail loading | `rayon::ThreadPool` (4 threads) | `image::open` + decode + scale (I/O + CPU) |
| Full-res loading | same `rayon::ThreadPool` | `image::open` + decode at native resolution (I/O + CPU) |

Only one search thread runs at a time — the Search button is disabled while `searching` is true. Thumbnail and full-res loads run concurrently with ongoing searches and UI interaction, sharing the same pool.

---

## 10. Project Structure

```text
ips_gui/
├── Cargo.toml                   # eframe, rfd, rayon, image, walkdir,
│                                # fuzzy-matcher, serde*, csv, regex
├── build.rs                     # Windows: embeds icon via winres
├── icon.svg
├── README.md
├── ips_gui_design_doc.md
├── .github/
│   └── workflows/
│       └── build.yml            # CI + release: Windows / macOS / Linux
└── src/
    ├── main.rs                  # App state, UI, search, export
    └── ips/                     # Embedded search library
        ├── mod.rs
        ├── types.rs             # Config, MatchResult, PromptRecord, Generator, MatchMode
        ├── discovery.rs         # walkdir-based file discovery
        ├── matcher.rs           # exact / fuzzy / regex matching
        └── extract/
            ├── mod.rs           # dispatch by file extension
            ├── png.rs           # tEXt / iTXt chunk parsing
            ├── jpeg.rs          # COM marker, APP1 XMP / EXIF
            ├── webp.rs          # RIFF chunk parsing, XMP / EXIF
            ├── exif.rs          # TIFF/EXIF UserComment decoder
            └── comfyui.rs       # ComfyUI workflow JSON extraction
```

`src/ips/` contains the full extraction and matching pipeline. It is an internal module — not a separate crate — so `ips_gui` builds from a single `cargo build` with no sibling directory required. `ips_cli` continues to maintain its own copy and evolves independently.

---

## 11. Key Design Decisions

### Self-contained module instead of path dependency

Originally `ips_gui` used `ips = { path = "../ips_cli" }`. This required both repos to be present side-by-side and complicated CI (two checkouts). Moving the library code into `src/ips/` makes `ips_gui` fully self-contained: one repo, one `cargo build`, one CI checkout. Both projects remain independently maintained — changes are ported manually when needed.

### Immediate-mode UI (egui)

egui rebuilds the entire UI on every frame from the current state struct. This eliminates data-binding and observer patterns. CPU cost is managed by calling `request_repaint_after(80ms)` only while searching, and relying on egui's own input-driven repaint otherwise.

### Separate thumbnail thread pool

Thumbnail decoding is I/O + CPU bound and must not block the UI thread. A fixed-size `rayon::ThreadPool` (separate from the global pool used by the search thread) provides bounded concurrency. The pool size of 4 balances decode throughput against memory pressure from simultaneous in-flight images.

### Virtual scrolling for the grid

`ScrollArea::show_rows` renders only the rows currently in the viewport. This keeps frame time and memory usage constant regardless of result count (tested with thousands of results). Textures outside the visible set are evicted each frame.

### Search within results

Re-filtering the existing `Vec<MatchResult>` avoids re-scanning the filesystem and re-decoding metadata — useful when narrowing a large result set by chaining queries. The implementation clones only the `Vec<PromptRecord>` values (path + prompt string), which is cheap, and runs `match_record` in parallel via `par_iter`. Thumbnails are intentionally *not* cleared in this path because the visible file set is a subset of the previous one.

### MatchModeOpt local enum

`ips::types::MatchMode` does not derive `PartialEq + Clone`, which `egui::selectable_value` requires. A local `MatchModeOpt` enum is defined in `main.rs` and mapped to `MatchMode` at Config construction time, keeping the GUI decoupled from the library's internal type constraints.

### Separate full-res cache with stale-result guard

Full-resolution images are cached in `full_res: HashMap<PathBuf, ThumbState>`, separate from `thumbnails`, so the two lifecycles do not interfere. When the user navigates away, both `full_res` and `full_res_queued` are cleared immediately. A pool task for the previous image may still be running at that point; when it eventually sends its result, `poll_full_res` uses `full_res_queued.remove(&path)` as an atomic gate — if the path is gone from the queue, the result is discarded and the texture is never uploaded. This prevents a race where an in-flight load completes after navigation and silently leaks a GPU texture.

### Regex validated before thread spawn

Invalid regex is caught on the UI thread before spawning, producing an immediate `error_msg` without involving the background thread.

---

## 12. CI / Release

`.github/workflows/build.yml` runs on every push to `main`, every PR, and every `v*` tag.

| Job | Trigger | Action |
|---|---|---|
| `build` (×3) | always | `cargo build --release` for Windows x86-64, macOS aarch64, Linux x86-64 |
| `release` | `v*` tag only | Downloads artifacts, generates `checksums.txt` (SHA-256), publishes GitHub Release |

Tags containing `-alpha`, `-beta`, or `-rc` are automatically marked as pre-releases.

---

## 13. Future Work

| Feature | Notes |
|---|---|
| Cancellable search | `Arc<AtomicBool>` cancel token checked inside the `flat_map` closure |
| Open image in viewer | `open::that(path)` on path click in detail view |
| Persistent settings | Serialize search parameters to a JSON config file on exit |
| Dark / light theme toggle | `ctx.set_visuals(egui::Visuals::dark())` wired to a button |
| Export error feedback | Surface `std::io::Error` from write operations in `error_msg` |
| Incremental search progress | `SearchMsg::Progress(n)` variants sent as batches complete |
