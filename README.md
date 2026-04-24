# ips_gui — Image Prompt Search GUI

A native desktop GUI for searching AI-generated image prompts embedded in PNG, JPEG, and WebP metadata. Supports Stable Diffusion (A1111/Forge), ComfyUI, NovelAI, and InvokeAI. Self-contained — no external library crate required.

## Requirements

- Rust 1.75 or later

## Build

```bash
cargo build --release
./target/release/ips_gui        # macOS / Linux
target\release\ips_gui.exe      # Windows
```

## Pre-built Binaries

Pre-built binaries for Windows (x86-64), macOS (Apple Silicon), and Linux (x86-64) are attached to each [GitHub Release](../../releases).

## Interface

### Grid view

![Grid view](screenshots/ips_gui_thumbnails.jpg)

### Detail view (click any thumbnail)

![Detail view](screenshots/ips_gui_details.jpg)

## Controls

### Search Parameters (left panel)

| Control | Description |
|---|---|
| **Query** | Text to search for. Press Enter or click Search to run. |
| **Directory** | Root directory to search. Type a path or click **Browse…** to pick a folder. Defaults to `.`. |
| **Match Mode** | `Exact` — case-insensitive substring (default). `Fuzzy` — approximate match via Skim algorithm. `Regex` — regular expression (validated before search). |
| **Min Score** | Fuzzy mode only. Slider 0–100; results below the threshold are excluded. Default: 50. |
| **Max Depth** | Limit directory recursion depth. Blank = unlimited. |
| **Top-level only** | Search only the immediate contents of the directory, no subdirectories. |
| **Search within results** | Visible only when results are present. When checked, Search filters the current result set instead of re-scanning the filesystem. Useful for iterative refinement. |
| **Search button** | Disabled while a search is running or the query is empty. |

### Results — Grid view (central panel)

Matched images are displayed as a 100×100 px thumbnail grid, sorted alphabetically by path. Non-image files show a 📄 icon. Click any cell to open the detail view.

### Results — Detail view

Opens when you click a thumbnail. Shows:

- **Back / Prev / Next buttons** — navigate between the grid and adjacent results.
- **Image preview** — up to 300×300 px, loaded asynchronously.
- **File path** — click **📋 Copy path** to copy to clipboard.
- **Generator** — detected source: `a1111`, `comfyui`, `novelai`, `invokeai`, or `unknown`.
- **Score** — visible in Fuzzy mode only.
- **Full prompt** — complete extracted metadata text.

### Keyboard shortcuts

| Key | Action |
|---|---|
| **Enter** (Query focused) | Start search |
| **← / →** | Previous / next result in detail view |
| **Esc** | Return to grid view |

### Status bar (bottom)

Displays a spinner while searching, result count and elapsed time on completion, or a red error for invalid queries (e.g. malformed regex).

### Export (left panel, after a search)

| Button | Output |
|---|---|
| **JSON** | Array of `{ path, generator, prompt, score? }` objects (pretty-printed). |
| **CSV** | RFC 4180, columns: `path`, `generator`, `prompt`, `score`. |

## Supported Generators

| Generator | Formats |
|---|---|
| Stable Diffusion A1111 / Forge | PNG (`parameters` tEXt chunk), JPEG (COM marker) |
| ComfyUI | PNG (`prompt` workflow JSON in tEXt/iTXt) |
| NovelAI | PNG (`Comment` JSON, `Description` chunk) |
| InvokeAI | JPEG / WebP (XMP with `invokeai:` namespace) |
| Generic | JPEG / WebP (XMP `dc:description`, EXIF `UserComment`) |

## Development

```bash
cargo build            # debug build
cargo build --release  # optimized release build
cargo test             # run unit tests
cargo clippy           # lint
```

## CI / Release

Pushing a `v*` tag triggers the GitHub Actions workflow, which builds release binaries for all three platforms and publishes a GitHub Release with SHA-256 checksums:

```bash
git tag v1.0.0
git push origin v1.0.0
```
