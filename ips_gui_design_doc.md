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
| PNG `iTXt` decompression | `flate2` v1 | zlib-compressed `iTXt` values |

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

`extract_prompt(path, verbose)` dispatches by file extension to one of three container parsers. Each reads only the metadata headers of a local file, never the image data, and returns `0..N` `PromptRecord`s. The extraction code is kept in step with `ips_cli` v0.3.0; the metadata fixtures (`tests/fixtures/metadata/`, shared byte-for-byte with `ips_cli`) define the expected records.

### 6.1 Container parsers

#### PNG (`png.rs`)

PNG stores metadata in ancillary text chunks **before** image data. The reader validates the 8-byte signature, then loops:

```
for each chunk:
    read length (BE u32) + type (4 bytes) + data + CRC (4 bytes, not validated)
    if type == IDAT → stop (no metadata after image data in normal AI PNGs)
    if type in [tEXt, iTXt]:
        parse (keyword, value)
        process_keyword(keyword, value)
```

Chunk data is read with `read_exact_vec`, which grows the buffer with the bytes actually received, so a bogus length field cannot force a huge allocation. A truncated chunk stops the loop and keeps the records collected so far.

| Chunk | Layout | Notes |
|---|---|---|
| `tEXt` | `keyword\0value` | Latin-1 by spec (Pillow writes it so); valid UTF-8 is read as UTF-8 |
| `iTXt` | `keyword\0compression_flag(1) compression_method(1) language\0 translated_keyword\0 text` | UTF-8; zlib-compressed (`compression_flag == 1`) and plain both supported. `zTXt` is not read. |

| Keyword | Expected generator | Value format | Handler |
|---|---|---|---|
| `parameters` | A1111 / Forge | Plain A1111 parameters text | `a1111::extract_details(value)` → generator `A1111` |
| `prompt` | ComfyUI | API prompt JSON `{ "node_id": { "class_type", "inputs" }, ... }`, or wrapped `{"prompt": …}` | See §6.3 |
| `workflow` | ComfyUI | UI workflow JSON (`nodes` + `links`), or wrapped `{"workflow": …}` | Converted to API form, then §6.3. Muted (mode 2) and bypassed (mode 4) nodes keep their links but contribute no values. |
| `Comment` | NovelAI | JSON with `"prompt"` field | Strict `serde_json`; `prompt` field → `a1111::extract_details` → generator `NovelAI`; no `prompt` field → no record; invalid JSON → raw text, generator `Unknown` |
| `Description` | NovelAI (alt) | Plain text | `a1111::extract_details(value)` → generator `NovelAI` |
| *(other)* | — | — | Silently skipped |

#### JPEG (`jpeg.rs`)

JPEG metadata lives in markers between SOI (`0xFFD8`) and SOS (`0xFFDA`, start of scan data).

```
loop markers:
    if SOS → stop
    read segment length (BE u16, includes length field itself)
    match marker type:
        0xFFFE (COM)  → body as UTF-8 lossy text
        0xFFE1 (APP1) → if XMP header → extract dc:description
                      → if Exif\0\0 header → EXIF block (below)
        else          → skip
```

| Segment | Generator (heuristic) | Handler |
|---|---|---|
| COM (`0xFFFE`) | A1111 | JSON object → ComfyUI (§6.3); else full body → `a1111::extract_details` |
| APP1 XMP | InvokeAI if `invokeai:` in XML; else Unknown | `extract_xmp_description` → `a1111::extract_details` |
| APP1 EXIF | Unknown | `exif::extract_user_comment` → JSON object → ComfyUI (§6.3); else `a1111::extract_details`; plus IFD0 `prompt:` / `workflow:` tags (below) |

JPEG save nodes such as ComfyUI-JPG-Metadata store `json.dumps({"prompt": …, "workflow": …})` in UserComment; a graph larger than the 64 KB segment limit arrives cut off and is recovered up to the cut (§6.2). Truncated segments, `seg_len < 2`, and lost marker sync stop the loop and keep partial results.

#### WebP (`webp.rs`)

Walks the RIFF chunks after the `RIFF….WEBP` header. `XMP ` and `EXIF` chunks follow the same rules as JPEG APP1 XMP / EXIF; the `EXIF` chunk is accepted with or without the `Exif\0\0` prefix. Their bodies are read with `read_exact_vec` (bogus length fields cannot force a huge allocation); all other chunks (`VP8`, `VP8L`, `VP8X`, `ANIM`, `ANMF`, …) are skipped by seeking, without reading them.

#### EXIF (`exif.rs`, JPEG APP1 / WebP EXIF chunk)

A minimal TIFF IFD walker — no external EXIF library. One EXIF block yields up to three records, in this order:

```
Exif\0\0 prefix (optional) → TIFF header (II/MM + magic 42)
→ UserComment (tag 0x9286) in IFD0, or in the ExifIFD (tag 0x8769) → decode charset prefix + payload
→ IFD0 ASCII tags of the form `prompt:{json}` / `workflow:{json}` → §6.3, keys `prompt` / `workflow`
```

ComfyUI's WebP and AVIF savers (`comfy_api/latest/_ui.py`, `_create_webp_metadata`) write `prompt:` into Model (0x0110) and each `extra_pnginfo` entry into Make (0x010F), then 0x010E and down — `workflow:` first. Other keys and plain camera values (no `key:` prefix) are ignored. These values follow the PNG `prompt` / `workflow` rules: JSON that is not recoverable is kept as plain text, generator `Unknown`.

| Charset tag | Decoding |
|---|---|
| `ASCII\0\0\0`, `JIS\0\0\0\0\0`, 8 zero bytes | Rest as lossy UTF-8 |
| `UNICODE\0` | UTF-16. Byte order: BOM if present; else guessed from where the zero bytes of ASCII characters fall (piexif and most ComfyUI savers write big-endian even in little-endian TIFF); else the TIFF byte order |
| No charset tag (Pillow writes a plain string) | Whole value as lossy UTF-8 |

**Safety limits:**
- IFD recursion depth capped at **4**
- Every read checks `offset + length <= buffer.len()`
- Oversized `count` field (declared length > available bytes) → decode what fits, no panic

UserComment text that is a JSON object goes through §6.3 (ComfyUI); other text through `a1111::extract_details`, because UserComment often contains full A1111-style parameter blocks.

### 6.2 Lenient JSON (`json_repair.rs`)

ComfyUI values (`prompt` / `workflow` keywords and tags, JSON objects in COM / UserComment) are parsed leniently:

- A document cut off before its end is recovered up to its last complete value: every value completed before the cut is kept and the objects and arrays left open around it are closed.
- Python's `NaN` / `Infinity` / `-Infinity` tokens, which `json.dumps` writes for non-finite floats, are read as `null`.

NovelAI `Comment` JSON is still parsed strictly (see §6.5).

### 6.3 Structured extraction

Structured metadata parsing is layered on top of the text/JSON extracted by the container parsers:

| Source | Structured extraction |
|---|---|
| A1111 / Forge-style text | `a1111.rs` splits `positive_prompt` and `negative_prompt` at `Negative prompt:`, extracts LoRA tags of the form `<lora:name:weight>`, and reads the model with `parse_model()`: `, Model:` / `\nModel:` / `Model:` in that order, value up to the next comma or newline, quoted values (`Model: "name, with comma.safetensors"`) supported, and a `Model hash:` match skipped. |
| ComfyUI JSON | `comfyui.rs`, described below. |

**Accepted top-level shapes:** an object whose values are nodes with `class_type` + `inputs`; `{"prompt": <API or UI>}`; `{"workflow": <UI>}`; a UI workflow (`nodes` + `links`). Anything else yields no record. Truncated JSON is first recovered up to its last complete value (§6.2).

Two parallel extractions merge into one `(prompt, PromptDetails)` (`comfyui::extract_workflow`):

**A. Searchable prompt text** (for matching):
1. Scan all nodes; collect non-empty strings from text-encode nodes (`CLIPTextEncode`, `CLIPTextEncodeSDXL`, `CLIPTextEncodeFlux`, `TextEncodeQwenImageEdit*`, any `class_type` containing `Prompt` or `TextEncode`).
2. Fields checked per node: `text_g`, `text_l` (SDXL), `clip_l`, `t5xxl` (Flux), `text`, `prompt`, plus negative fields `neg`, `negative_prompt`.
3. Deduplicate identical strings; join with `" | "`.
4. **Fallback:** if step 1–3 yield nothing, join sampler-traced `positive_prompt` and `negative_prompt` with `" | "`.

**B. Structured details** (`PromptDetails`):

| Field | Source |
|---|---|
| `model` | `CheckpointLoader*` / `UNETLoader` / `UnetLoaderGGUF` nodes → `inputs.ckpt_name` or `inputs.unet_name`; otherwise any node carrying one of those fields (custom loaders) |
| `loras` | Any node whose `class_type` contains `"lora"` (case-insensitive). Standard `LoraLoader`: `lora_name` + weights. rgthree `Power Lora Loader`: object-valued inputs with `"lora"` key; skip if `"on": false`. |
| `positive_prompt` / `negative_prompt` | First `KSampler*` / `SamplerCustom*` or guider (`CFGGuider`, `BasicGuider`, `DualCFGGuider`) that resolves → follow the positive input (`positive`, or a guider's `conditioning` / `cond1`) and `negative` node refs (id **and output slot**) through the conditioning graph |

A record is created when the prompt text is non-empty or a model / LoRA / sampler-traced prompt exists (`has_extractable_content`).

**Conditioning graph walk** (`resolve_conditioning_text`):
- Node refs are `["node_id", slot_index]` arrays; the conditioning **output slot** travels with the reference (`CondRef { id, index }`): slot 0 is the positive stream, slot ≥ 1 is the negative stream.
- Slot ≥ 1 reads only `neg` / `negative_prompt` / `negative`, never the shared positive field. A node that encodes both prompts in one pass (`TextEncodeQwenImage21`, which emits positive and negative conditioning from one node) therefore resolves to its `prompt` and `negative_prompt` fields respectively instead of reporting the positive prompt twice — and to nothing when `negative_prompt` is empty.
- Slot 0 encodes the node's main prompt whichever sampler input it feeds: standard fields `text_g` / `text_l` / `clip_l` / `t5xxl` / `text` / `prompt` first, then a role-named fallback (`positive`, or `neg` / `negative_prompt` / `negative` for a negative ref).
- If target is text-encode node → extract the fields above.
- If target is `ZeroOut` → return `None` (empty negative conditioning).
- Else follow role-specific input fields: positive → `positive`, `conditioning`, `conditioning_1`, `conditioning_2` (ConditioningCombine), `conditioning_to`, `conditioning_from` (ConditioningAverage/Concat), `conditioning1`, `conditioning2`, `cond`; negative → `negative`, `neg`, then the same chain fields. Role-named inputs reset the role for the next hop; neutral chain fields keep it.
- **Cycle guard:** `HashSet<(String, u64)>` of visited node ID + slot pairs.

All structured fields are optional. Truncated JSON is recovered (§6.2); a value that cannot be parsed as JSON at all keeps its original text as the searchable `prompt` (generator `Unknown`, A1111-style parse), and the structured fields stay blank when nothing can be read from it.

### 6.4 Routing matrix

```
PNG keyword "parameters"  ──→ a1111::extract_details          [A1111]
PNG keyword "prompt" / "workflow" ──→ lenient JSON OK  → comfyui::extract_workflow  [ComfyUI]
                                 └→ not JSON        → a1111::extract_details      [Unknown]
PNG keyword "Comment"   ──→ serde_json OK  → json["prompt"] → a1111       [NovelAI]
                         └→ serde_json ERR → a1111::extract_details      [Unknown]
PNG keyword "Description" ─→ a1111::extract_details          [NovelAI]

JPEG COM                ──→ JSON object → comfyui  [ComfyUI]; else a1111  [A1111]
JPEG APP1 XMP           ──→ extract_xmp_description → a1111 [InvokeAI/Unknown]
JPEG APP1 EXIF          ──→ UserComment: JSON object → comfyui [ComfyUI]; else a1111 [Unknown]
                         └→ IFD0 "prompt:" / "workflow:" tags → (same as PNG "prompt" / "workflow")

WebP XMP chunk          ──→ (same as JPEG XMP)
WebP EXIF chunk         ──→ (same as JPEG EXIF)

"Lenient JSON": truncated documents are recovered up to the last complete value, and
Python's NaN / Infinity / -Infinity tokens are read as null.
```

### 6.5 Known limitations

| Limitation | Impact | Workaround |
|---|---|---|
| ComfyUI UI subgraphs | Nodes inside `definitions.subgraphs` are not expanded, so the `workflow` record can miss prompts and models held there; the `prompt` record is unaffected | Search the `prompt` record |
| AVIF | Extension not supported, so ComfyUI AVIF output (same `prompt:` / `workflow:` EXIF tags as WebP) is never read | Save as PNG or WebP |
| TIFF / GIF / BMP | Skipped (unsupported extension) | — |
| `zTXt` PNG chunks | Not read | — |
| PNG metadata after `IDAT` | Never read (parsing stops at image data) | Re-save with metadata before image data |
| WebP EXIF / XMP chunk cut off by end of file | The whole chunk is dropped; unlike JPEG UserComment JSON, no partial recovery | — |
| JPEG Extended XMP (`http://ns.adobe.com/xmp/extension/` APP1 segments) | Skipped | — |
| XMP formats without `<dc:description><rdf:Alt><rdf:li>` | `None` returned | — |
| Multiple XMP `<rdf:li>` entries | Only first extracted | — |
| NovelAI `Comment` JSON | Parsed strictly: truncated JSON or `NaN` falls back to plain text instead of the lenient ComfyUI parser | — |
| A1111-style text in EXIF UserComment | Generator reported as `unknown`, not `a1111`, even when the text has A1111 parameters | Model / prompts are still parsed |
| `DualCFGGuider` | Positive prompt taken from `cond1` only; `cond2` is ignored | — |
| UI workflow node with an `id` that is neither number nor string | Conversion of the whole `workflow` aborts (no record from it) | — |

### 6.6 Multi-record files

The GUI shows one grid cell per record, not per file: a ComfyUI image carrying both `prompt` and `workflow` (standard ComfyUI PNG, official ComfyUI WebP) appears as two results, the same records `ips_cli` reports.

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
│                                # fuzzy-matcher, serde*, csv, regex, flate2
├── build.rs                     # Windows: embeds icon via winres
├── icon.svg
├── README.md
├── ips_gui_design_doc.md
├── .github/
│   └── workflows/
│       └── build.yml            # CI + release: Windows / macOS / Linux
├── tests/
│   ├── metadata_fixtures.rs     # builds an image per fixture, compares extract_prompt output
│   └── fixtures/
│       ├── metadata/            # metadata fixtures (*.json), identical to ips_cli's
│       └── qwen21_shared_encode.json
└── src/
    ├── main.rs                  # App state, UI, search, export
    ├── lib.rs                   # lib target: `pub mod ips;`
    └── ips/                     # Embedded search library
        ├── mod.rs
        ├── types.rs             # Config, MatchResult, PromptRecord, PromptDetails, LoraInfo, Generator, MatchMode
        ├── discovery.rs         # walkdir-based file discovery
        ├── matcher.rs           # exact / fuzzy / regex matching
        └── extract/
            ├── mod.rs           # dispatch by file extension; shared record constructors
            ├── a1111.rs         # A1111/Forge text metadata parsing
            ├── png.rs           # tEXt / iTXt chunk parsing
            ├── jpeg.rs          # COM marker, APP1 XMP / EXIF
            ├── json_repair.rs   # lenient JSON: truncation recovery, NaN / Infinity
            ├── webp.rs          # RIFF chunk parsing, XMP / EXIF
            ├── exif.rs          # TIFF/EXIF UserComment and IFD0 prompt:/workflow: tags
            └── comfyui.rs       # ComfyUI workflow JSON and structured metadata extraction
```

`src/ips/` contains the full extraction and matching pipeline. It is the lib target of the same package (`ips_gui::ips`, root `src/lib.rs`) — not a separate crate — so `ips_gui` builds from a single `cargo build` with no sibling directory required; the bin (`src/main.rs`) and the integration tests both use it through that path. `ips_cli` continues to maintain its own copy and evolves independently.

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
