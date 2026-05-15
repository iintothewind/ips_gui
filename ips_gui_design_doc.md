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
2. **Search thread** — spawned by `std::thread::spawn` per search. Runs the full discovery → extraction → matching pipeline, sends one `(Vec<MatchResult>, elapsed_seconds)` message back when complete.
3. **Thumbnail pool** — a dedicated `rayon::ThreadPool` (4 threads) that loads and decodes both thumbnails and full-resolution images on demand. Results are sent back to the UI thread via two separate `mpsc` channels.

The UI thread also handles drag-and-drop events. Single-image drops are parsed synchronously because they only touch one file; directory drops only update the search path and never start a filesystem scan. The UI thread never blocks on full searches or image decoding. It uses `try_recv()` on every frame for all channels.

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
    match_mode: MatchMode,          // Exact | Fuzzy | Regex
    min_score: i64,                 // fuzzy threshold, 0–100
    no_recursive: bool,
    depth_str: String,              // raw text, parsed to Option<usize> at search time
    search_within_results: bool,

    // Search runtime
    searching: bool,
    results: Vec<MatchResult>,
    status_msg: String,
    error_msg: Option<String>,
    rx: Option<Receiver<(Vec<MatchResult>, f64)>>,

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

    // Drag-and-drop
    drag_over_params_panel: bool,
}
```

`depth_str` is a raw string so the text field can hold partially-typed input without losing characters. Parsed at search time with `.trim().parse().ok()`.

`ThumbState` is `Loaded(TextureHandle) | Failed`. Thumbnail textures are evicted when no longer visible in the grid (via `retain`). Full-res textures are dropped when navigating away from a detail item.

`PromptRecord` is the metadata carrier in the result list. It keeps the original searchable `prompt` string for backwards-compatible matching, plus optional structured fields:

```rust
struct PromptRecord {
    path: PathBuf,
    prompt: String,
    model: Option<String>,
    loras: Vec<LoraInfo>,
    positive_prompt: Option<String>,
    negative_prompt: Option<String>,
    generator: Generator,
    metadata_key: &'static str,
}
```

These structured fields are best-effort. Missing, incomplete, or malformed image metadata leaves the field blank instead of failing extraction.

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
  tx.send((results, elapsed))
  ctx.request_repaint()
       │
       ▼  (UI thread, next frame)
  rx.try_recv() → Ok((results, elapsed))
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
  tx.send((results, elapsed))
  ctx.request_repaint()
```

Skips `discover_files` and `extract_prompt` entirely. Works on already-extracted `PromptRecord` values. Can be chained multiple times to progressively narrow a result set.

---

### 5.3 Drag-and-drop shortcuts

Drag-and-drop is handled after the left Search Parameters panel has been rendered, so the panel rectangle can be used to decide whether the drop applies to search parameters.

| Drop item on left panel | Behavior |
|---|---|
| One supported image file (`png`, `jpg`, `jpeg`, `webp`) | Clears current results, extracts metadata for that one file, creates one or more `MatchResult` rows, and switches directly to `ViewMode::Detail(0)`. If the image has no metadata, an empty `PromptRecord` is still created so the image and path can be inspected. |
| Multiple supported image files | Rejected with `Please drop one image at a time.` |
| One directory | Sets `search_path` to that folder and updates the status text. It does not scan or search automatically. |

This keeps the application focused on prompt search instead of turning directory drops into a bulk image browser. Large folders are only scanned when the user explicitly enters a query and clicks Search.

---

## 6. Metadata Extraction Model

The low-level container parsers remain intentionally tolerant:

| Parser | Existing responsibility |
|---|---|
| `png.rs` | Reads PNG `tEXt` / `iTXt` chunks before `IDAT`; handles invalid or truncated chunks by returning partial/empty results. |
| `jpeg.rs` | Reads JPEG COM and APP1 XMP/EXIF segments; stops safely on scan data or malformed segments. |
| `webp.rs` | Reads RIFF/WebP chunks; handles `XMP ` and `EXIF`, skips image data chunks without loading them. |
| `exif.rs` | Decodes TIFF/EXIF `UserComment` in ASCII or UTF-16, big-endian or little-endian. Empty or invalid comments return `None`. |

Structured metadata parsing is layered on top of the text/JSON extracted by those parsers:

| Source | Structured extraction |
|---|---|
| A1111 / Forge-style text | `a1111.rs` splits `positive_prompt` and `negative_prompt` at `Negative prompt:`, parses `Model:`, and extracts LoRA tags of the form `<lora:name:weight>`. |
| ComfyUI workflow JSON | `comfyui.rs` parses generation model names from common checkpoint/UNET loader nodes, LoRA entries from `LoraLoader` and `Power Lora Loader (rgthree)`, and positive/negative prompts by tracing `KSampler` / `CFGGuider` conditioning inputs to text encode nodes. |

All structured fields are optional. If JSON is missing, truncated, plugin-specific, or not parseable, the original `prompt` string is preserved for search and the structured fields remain blank.

---

## 7. UI Layout

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
| Drag-and-drop target | Panel response rectangle. One image opens detail view; one folder fills Directory; multiple images are rejected. |

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
             │  Model:
             │  LoRA:
             │  Positive prompt:
             │  Negative prompt:
```

Clicking the thumbnail sets `detail_image_enlarged = true` and fires `request_full_res`. If the thumbnail failed to load (file missing or unreadable), a ✕ placeholder is shown and clicking is disabled.

The structured metadata fields are rendered as blank labels when parsing did not produce a value.

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

## 8. Image Loading

Both thumbnail and full-resolution loading share the same `rayon::ThreadPool` (`THUMB_THREADS = 4`) and the same `ThumbState` enum, but use separate channels and caches so they are independently managed.

### 8.1 Thumbnail loading

1. **Request** — `request_thumb(path, ctx)` checks `thumbnails` and `thumb_queued`; if absent from both, inserts into `thumb_queued` and spawns a pool task.
2. **Decode** — pool task calls `load_thumbnail`: opens the file with the `image` crate, scales to at most 300×300 px (`thumbnail(300, 300)`), converts to `RGBA8`, wraps in `egui::ColorImage`.
3. **Return** — task sends `(path, Option<ColorImage>)` over `thumb_result_tx` and calls `ctx.request_repaint()`.
4. **Upload** — `poll_thumbs()` called each frame drains `thumb_rx`, uploads decoded images to GPU via `ctx.load_texture`, stores `ThumbState::Loaded(handle)` or `ThumbState::Failed`.

**Eviction** (grid mode only): after rendering, `thumbnails.retain` and `thumb_queued.retain` discard entries whose paths are not in the currently visible `grid_vis` set. Detail mode skips eviction to avoid re-decoding when navigating back.

### 8.2 Full-resolution loading

Triggered when the user clicks the thumbnail in detail view to enlarge it.

1. **Request** — `request_full_res(path, ctx)` checks `full_res` and `full_res_queued`. If the thumbnail already has `ThumbState::Failed` for this path, the request is skipped entirely (file known missing). Otherwise inserts into `full_res_queued` and spawns a pool task.
2. **Decode** — pool task calls `load_full_res`: opens the file at native resolution (capped at 4096×4096 px to bound memory), converts to `RGBA8`, wraps in `egui::ColorImage`.
3. **Return** — task sends `(path, Option<ColorImage>)` over `full_res_tx`.
4. **Upload** — `poll_full_res()` called each frame drains `full_res_rx`. Before inserting, it calls `full_res_queued.remove(&path)`: if the path is no longer queued (because navigation cleared it), the result is discarded immediately and the texture is never stored. This prevents stale in-flight loads from leaking GPU memory after the user has navigated away.

**Eviction**: `full_res` and `full_res_queued` are both cleared whenever `next_view` is set (Back, Prev, Next, Esc-to-grid). Dropping `ThumbState::Loaded(handle)` releases the `TextureHandle`, which decrements the egui reference count and frees the GPU texture.

### 8.3 Error display

| State | Thumbnail area | Enlarged area |
|---|---|---|
| Loading | ⏳ spinner placeholder | Spinner + "Loading…" label |
| `ThumbState::Failed` | ✕ icon + "File not found" label | Red "Failed to load image" label |
| `ThumbState::Loaded` | Scaled image (clickable) | Full-res image fitted to panel (clickable) |

---

## 9. Export

Both export functions:

1. Open a native save dialog via `rfd::FileDialog::new().save_file()`.
2. Return immediately if the user cancels.
3. Serialize `self.results` and write to the chosen path.

**JSON** uses an inline `#[derive(Serialize)]` struct. Each row includes `path`, `generator`, `prompt`, `model`, `loras`, `positive_prompt`, `negative_prompt`, and optional `score`. Optional fields are omitted when empty.

**CSV** uses `csv::Writer::from_path`. Columns are `path`, `generator`, `model`, `loras`, `positive_prompt`, `negative_prompt`, `prompt`, and `score`. LoRAs are joined as `name: weight | name: weight`. Score is an empty string in non-fuzzy modes.

I/O errors are silently discarded; a future improvement would surface them via `error_msg`.

---

## 10. Threading Model

| Actor | Thread | Blocking operations |
|---|---|---|
| UI event loop | Main thread | None |
| Search | `std::thread::spawn` (one at a time) | `discover_files` (I/O), `par_iter` extraction + matching (CPU) |
| Thumbnail loading | `rayon::ThreadPool` (4 threads) | `image::open` + decode + scale (I/O + CPU) |
| Full-res loading | same `rayon::ThreadPool` | `image::open` + decode at native resolution (I/O + CPU) |

Only one search thread runs at a time — the Search button is disabled while `searching` is true. Thumbnail and full-res loads run concurrently with ongoing searches and UI interaction, sharing the same pool.

---

## 11. Project Structure

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
        ├── types.rs             # Config, MatchResult, PromptRecord, PromptDetails, LoraInfo, Generator, MatchMode
        ├── discovery.rs         # walkdir-based file discovery
        ├── matcher.rs           # exact / fuzzy / regex matching
        └── extract/
            ├── mod.rs           # dispatch by file extension
            ├── a1111.rs         # A1111/Forge text metadata parsing
            ├── png.rs           # tEXt / iTXt chunk parsing
            ├── jpeg.rs          # COM marker, APP1 XMP / EXIF
            ├── webp.rs          # RIFF chunk parsing, XMP / EXIF
            ├── exif.rs          # TIFF/EXIF UserComment decoder
            └── comfyui.rs       # ComfyUI workflow JSON and structured metadata extraction
```

`src/ips/` contains the full extraction and matching pipeline. It is an internal module — not a separate crate — so `ips_gui` builds from a single `cargo build` with no sibling directory required. `ips_cli` continues to maintain its own copy and evolves independently.

---

## 12. Key Design Decisions

### Self-contained module instead of path dependency

Originally `ips_gui` used `ips = { path = "../ips_cli" }`. This required both repos to be present side-by-side and complicated CI (two checkouts). Moving the library code into `src/ips/` makes `ips_gui` fully self-contained: one repo, one `cargo build`, one CI checkout. Both projects remain independently maintained — changes are ported manually when needed.

### Immediate-mode UI (egui)

egui rebuilds the entire UI on every frame from the current state struct. This eliminates data-binding and observer patterns. CPU cost is managed by calling `request_repaint_after(80ms)` only while searching, and relying on egui's own input-driven repaint otherwise.

### Separate thumbnail thread pool

Thumbnail decoding is I/O + CPU bound and must not block the UI thread. A fixed-size `rayon::ThreadPool` (separate from the global pool used by the search thread) provides bounded concurrency. The pool size of 4 balances decode throughput against memory pressure from simultaneous in-flight images.

### Virtual scrolling for the grid

`ScrollArea::show_rows` renders only the rows currently in the viewport. This keeps frame time and memory usage constant regardless of result count (tested with thousands of results). Textures outside the visible set are evicted each frame.

### Search within results

Re-filtering the existing `Vec<MatchResult>` avoids re-scanning the filesystem and re-decoding metadata — useful when narrowing a large result set by chaining queries. The implementation clones only the existing `PromptRecord` values and runs `match_record` in parallel via `par_iter`. Thumbnails are intentionally *not* cleared in this path because the visible file set is a subset of the previous one.

### Structured metadata is best-effort

`PromptRecord.prompt` remains the canonical searchable text so existing exact/fuzzy/regex behavior stays stable. `model`, `loras`, `positive_prompt`, and `negative_prompt` are optional display/export fields populated only when the parser can infer them with confidence. This is important for JPEG/WebP files, where metadata may be truncated, missing, or saved in generator-specific formats.

### Drag-and-drop stays narrow

Dropping one image is a shortcut for inspecting that image's metadata. Dropping one folder only fills the Directory field. Multiple images are rejected. This avoids turning a prompt search tool into a bulk gallery browser and prevents accidental large-directory scans.

### Separate full-res cache with stale-result guard

Full-resolution images are cached in `full_res: HashMap<PathBuf, ThumbState>`, separate from `thumbnails`, so the two lifecycles do not interfere. When the user navigates away, both `full_res` and `full_res_queued` are cleared immediately. A pool task for the previous image may still be running at that point; when it eventually sends its result, `poll_full_res` uses `full_res_queued.remove(&path)` as an atomic gate — if the path is gone from the queue, the result is discarded and the texture is never uploaded. This prevents a race where an in-flight load completes after navigation and silently leaks a GPU texture.

### Regex validated before thread spawn

Invalid regex is caught on the UI thread before spawning, producing an immediate `error_msg` without involving the background thread.

---

## 13. CI / Release

`.github/workflows/build.yml` runs on every push to `main`, every PR, and every `v*` tag.

| Job | Trigger | Action |
|---|---|---|
| `build` (×3) | always | `cargo build --release` for Windows x86-64, macOS aarch64, Linux x86-64 |
| `release` | `v*` tag only | Downloads artifacts, generates `checksums.txt` (SHA-256), publishes GitHub Release |

Tags containing `-alpha`, `-beta`, or `-rc` are automatically marked as pre-releases.

---

## 14. Future Work

| Feature | Notes |
|---|---|
| Cancellable search | `Arc<AtomicBool>` cancel token checked inside the `flat_map` closure |
| Open image in viewer | `open::that(path)` on path click in detail view |
| Persistent settings | Serialize search parameters to a JSON config file on exit |
| Dark / light theme toggle | `ctx.set_visuals(egui::Visuals::dark())` wired to a button |
| Export error feedback | Surface `std::io::Error` from write operations in `error_msg` |
| Incremental search progress | Progress messages sent as batches complete |
