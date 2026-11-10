pub mod a1111;
pub mod comfyui;
pub mod exif;
pub mod jpeg;
pub mod json_repair;
pub mod png;
pub mod webp;

use std::io::{self, Read};
use std::path::Path;

use crate::ips::types::{Generator, PromptRecord};

/// Outcome of reading a metadata value as ComfyUI JSON.
enum ComfyJson {
    /// The JSON (possibly recovered from a truncated document) yielded a prompt record.
    Record(PromptRecord),
    /// Valid JSON without any prompt, model or LoRA.
    Empty,
    /// Not JSON, and nothing recoverable.
    NotJson,
}

/// Parse `text` as a ComfyUI API prompt or UI workflow. Truncated JSON is recovered up to
/// its last complete value.
fn comfyui_json(origin: &Path, key: &'static str, text: &str, verbose: bool) -> ComfyJson {
    let Some((json, repaired)) = json_repair::parse_lenient(text) else {
        return ComfyJson::NotJson;
    };
    if repaired && verbose {
        eprintln!("ips: {}: {}: truncated JSON, read up to the cut", origin.display(), key);
    }
    let (prompt, details) = comfyui::extract_workflow(&json);
    if comfyui::has_extractable_content(&prompt, &details) {
        ComfyJson::Record(PromptRecord::with_details(
            origin.to_path_buf(),
            prompt,
            Generator::ComfyUI,
            key,
            details,
        ))
    } else {
        ComfyJson::Empty
    }
}

/// Record for a value stored under ComfyUI's own `prompt` / `workflow` keys (PNG text
/// chunks, EXIF `key:{json}` tags). Text that is not JSON is kept as plain parameters text.
pub(crate) fn comfyui_keyword_record(
    origin: &Path,
    key: &'static str,
    text: &str,
    verbose: bool,
) -> Option<PromptRecord> {
    match comfyui_json(origin, key, text, verbose) {
        ComfyJson::Record(record) => Some(record),
        ComfyJson::Empty => None,
        ComfyJson::NotJson if text.trim().is_empty() => None,
        ComfyJson::NotJson => Some(PromptRecord::with_details(
            origin.to_path_buf(),
            text.to_string(),
            Generator::Unknown,
            key,
            a1111::extract_details(text),
        )),
    }
}

/// Records from an EXIF block (JPEG APP1, WebP EXIF chunk): the UserComment, then ComfyUI
/// graphs stored as `prompt:{…}` / `workflow:{…}` IFD0 tags.
pub(crate) fn exif_records(origin: &Path, exif_data: &[u8], verbose: bool) -> Vec<PromptRecord> {
    let mut records = Vec::new();
    if let Some(text) = exif::extract_user_comment(exif_data) {
        records.push(text_record(origin, "UserComment", text, Generator::Unknown, verbose));
    }
    for (key, text) in exif::extract_comfyui_tags(exif_data) {
        records.extend(comfyui_keyword_record(origin, key, &text, verbose));
    }
    records
}

/// Record for free-form text metadata (JPEG COM, EXIF UserComment). A JSON object is read as
/// ComfyUI data, as written by JPEG save nodes that store `{"prompt": …, "workflow": …}`;
/// anything else is A1111-style parameters text attributed to `generator`.
pub(crate) fn text_record(
    origin: &Path,
    key: &'static str,
    text: String,
    generator: Generator,
    verbose: bool,
) -> PromptRecord {
    if text.trim_start().starts_with('{') {
        if let ComfyJson::Record(record) = comfyui_json(origin, key, &text, verbose) {
            return record;
        }
    }
    let details = a1111::extract_details(&text);
    PromptRecord::with_details(origin.to_path_buf(), text, generator, key, details)
}

/// Read exactly `len` bytes. The buffer grows with the bytes actually received, so a
/// bogus length field cannot force a multi-gigabyte allocation.
pub(crate) fn read_exact_vec<R: Read>(reader: &mut R, len: usize) -> io::Result<Vec<u8>> {
    const MAX_PREALLOC: usize = 1 << 20;
    let mut buf = Vec::with_capacity(len.min(MAX_PREALLOC));
    reader.by_ref().take(len as u64).read_to_end(&mut buf)?;
    if buf.len() == len {
        Ok(buf)
    } else {
        Err(io::ErrorKind::UnexpectedEof.into())
    }
}

pub fn extract_prompt(path: &Path, verbose: bool) -> Vec<PromptRecord> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());

    match ext.as_deref() {
        Some("png") => png::extract(path, verbose),
        Some("jpg") | Some("jpeg") => jpeg::extract(path, verbose),
        Some("webp") => webp::extract(path, verbose),
        _ => vec![],
    }
}
