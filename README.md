# ips_gui — Image Prompt Search GUI

A native desktop GUI for ips_cli (image prompt search), the AI image prompt search tool. Search PNG, JPEG, and WebP image metadata from Stable Diffusion (A1111/Forge), ComfyUI, NovelAI, and InvokeAI — with a point-and-click interface, live match highlighting, and one-click export.

## Requirements

- Rust 1.75 or later
- The `ips` library crate located at `../ips_cli` (included in the workspace)

## Installation

Build a debug binary:

```bash
cd ips_gui
cargo build
./target/debug/ips_gui
```

Build an optimized release binary:

```bash
cargo build --release
./target/release/ips_gui
```

## Interface

```
┌─────────────────────┬──────────────────────────────────────────────┐
│  Search Parameters  │  Results (42)                                │
│  ─────────────────  │  ──────────────────────────────────────────  │
│  Query:             │  ┌──────────────────────────────────────────┐│
│  [cyberpunk city  ] │  │ ./art/city01.png  [a1111]                ││
│                     │  │ ...a photo of a cyberpunk city at         ││
│  Directory:         │  │ night, neon lights, rain...               ││
│  [./images ] Browse │  └──────────────────────────────────────────┘│
│                     │  ┌──────────────────────────────────────────┐│
│  Match Mode:        │  │ ./art/city02.png  [comfyui]              ││
│  Exact Fuzzy Regex  │  │ ...futuristic cyberpunk cityscape,        ││
│                     │  │ ultra detailed...                         ││
│  Max Depth:         │  └──────────────────────────────────────────┘│
│  [          ]       │                                              │
│                     │                                              │
│  Options:           │                                              │
│  [ ] Show full      │                                              │
│  [ ] Top-level only │                                              │
│  [ ] Verbose        │                                              │
│                     │                                              │
│  [     Search     ] │                                              │
│                     │                                              │
│  Export: JSON  CSV  │                                              │
├─────────────────────┴──────────────────────────────────────────────┤
│  Found 42 result(s) in 0.84s                                       │
└────────────────────────────────────────────────────────────────────┘
```

## Controls

### Search Parameters (left panel)

| Control | Description |
|---|---|
| **Query** | Text to search for. Press Enter or click Search to run. |
| **Directory** | Root directory to search. Type a path or click **Browse…** to pick a folder with a native dialog. Defaults to `.` (current directory). |
| **Match Mode** | `Exact` — case-insensitive substring match (default). `Fuzzy` — approximate matching using the Skim algorithm. `Regex` — regular expression match (validated before search starts). |
| **Min Score** | Appears only in Fuzzy mode. Slider from 0 to 100; results below the threshold are excluded. Default: 50. |
| **Max Depth** | Limit directory recursion depth. Leave blank for unlimited. |
| **Show full prompt** | When unchecked, prompts longer than 500 characters are truncated with `…`. Check to see the complete text. |
| **Top-level only** | When checked, only the immediate contents of the directory are searched (no subdirectories). |
| **Verbose** | Log skipped and corrupt files to stderr (useful for debugging). |
| **Search button** | Disabled while a search is already running or the query field is empty. |

### Results (central panel)

Each result is displayed as a card showing:

- **File path** (cyan, bold) — the absolute or relative path to the image file.
- **Generator tag** (grey) — detected source: `a1111`, `comfyui`, `novelai`, `invokeai`, or `unknown`.
- **Fuzzy score** (green, small) — visible only in Fuzzy mode.
- **Prompt text** — the extracted metadata, with matched portions highlighted in yellow. Prompts are truncated at 500 characters unless **Show full prompt** is checked.

Results are sorted alphabetically by file path.

### Status bar (bottom)

Displays a spinner and "Searching…" while a search is running, the result count and elapsed time after completion, or a red error message if the query is invalid (e.g. malformed regex).

### Export (left panel, appears after a search)

| Button | Description |
|---|---|
| **JSON** | Opens a native save dialog. Writes a JSON array with `path`, `generator`, `prompt`, and `score` (fuzzy mode only) fields. |
| **CSV** | Opens a native save dialog. Writes RFC 4180 CSV with the same four columns. |

## Keyboard Shortcut

Pressing **Enter** while the Query field is focused starts the search, equivalent to clicking the Search button.

## Supported Generators

Inherits full generator support from the `ips` library:

| Generator | Formats |
|---|---|
| Stable Diffusion A1111 / Forge | PNG (`parameters` chunk), JPEG (COM marker) |
| ComfyUI | PNG (`prompt` workflow JSON) |
| NovelAI | PNG (`Comment` JSON, `Description` chunk) |
| InvokeAI | JPEG / WebP (XMP with `invokeai:` namespace) |
| Various | JPEG / WebP (XMP `dc:description`) |

## Relationship to ips CLI

`ips_gui` is a thin GUI shell over the `ips` library. It reuses the same discovery, extraction, and matching pipeline as the CLI — no logic is duplicated. The CLI remains the better choice for scripting, piping output, or batch processing; the GUI is optimised for interactive browsing.

## Development

```bash
cargo build          # debug build
cargo build --release  # optimized release build
cargo clippy         # lint
```
