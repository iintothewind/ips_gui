//! Metadata fixtures (`tests/fixtures/metadata/*.json`).
//!
//! Each fixture holds the metadata of one image, either copied from a real image (`origin`
//! names its object key) or written by hand for an extraction path no available image
//! exercises (`origin: "synthetic"`). The test rebuilds a minimal image of the stated
//! container from those entries, in order, writes it to a temporary file, extracts it with
//! `extract_prompt` (verbose diagnostics on), and compares the records with `expected`.
//!
//! Entry forms (`text` is stored verbatim; `json` is serialized compactly with non-ASCII
//! escaped, the way ComfyUI writes it; `hex` is an opaque body ips must skip):
//! - png: `{"chunk": "tEXt", "keyword", "text" | "json"}` (tEXt is written as Latin-1),
//!   `{"chunk": "iTXt", "keyword", "compressed": bool, "text" | "json"}`,
//!   `{"chunk": <any type>, "hex"}`
//! - jpeg: `{"segment": "COM" | "XMP", "text" | "json"}`, `{"segment": "Exif", EXIF}`,
//!   `{"marker": "<hex byte>", "hex"}`; without `hex` a bare marker (no length field)
//! - webp: `{"chunk": "XMP ", "text"}`, `{"chunk": "EXIF", "exif_header": bool, EXIF}`,
//!   `{"chunk": <any type>, "hex"}`
//! - EXIF: `"user_comment": UC` and/or `"ifd0_ascii": [{"tag": "0x0110", "text"}]` (ASCII
//!   tags in IFD0, written NUL-terminated; ComfyUI stores `prompt:{…}` / `workflow:{…}`
//!   there), plus `"byte_order"` when there is no UC to take it from
//! - UC: `{"byte_order": "II" | "MM", "ifd": "exif" | "ifd0",
//!   "charset": "UNICODE" | "ASCII" | "undefined" | "none",
//!   "utf16": "be" | "le" | "bom-be" | "bom-le" (UNICODE only), "text" | "json"}`
//!
//! Damaged images: `"truncate_to": <byte count>` / `"truncate_by": <byte count>` cut the
//! built image to that length / by that many bytes, and `"patch": [{"offset", "hex"}]`
//! overwrites bytes in it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::ZlibEncoder;
use flate2::Compression;
use ips_gui::ips::extract;
use ips_gui::ips::types::PromptRecord;
use serde_json::{json, Map, Value};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/metadata")
}

#[test]
fn metadata_fixtures_extract_expected_records() {
    let mut paths: Vec<PathBuf> = fs::read_dir(fixture_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures found");

    let tmp = tempfile::TempDir::new().unwrap();
    let mut failures = Vec::new();
    for path in &paths {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let fixture: Value = serde_json::from_str(&fs::read_to_string(path).unwrap())
            .unwrap_or_else(|e| panic!("{name}: invalid fixture JSON: {e}"));
        let (ext, mut bytes) = build_image(&fixture);
        damage(&fixture, &mut bytes);

        let file = tmp.path().join(format!("{name}.{ext}"));
        fs::write(&file, &bytes).unwrap();
        let records = extract::extract_prompt(&file, true);

        let expected = fixture["expected"].clone();
        let actual = Value::Array(records.iter().map(record_json).collect());
        if actual != expected {
            failures.push(format!("{name}:\n  expected: {expected}\n  actual:   {actual}"));
        }
    }
    assert!(failures.is_empty(), "{} fixture mismatch(es):\n{}", failures.len(), failures.join("\n"));
}

/// The record as fixtures spell it: no path, empty fields omitted.
fn record_json(record: &PromptRecord) -> Value {
    let mut map = Map::new();
    map.insert("metadata_key".into(), json!(record.metadata_key));
    map.insert("generator".into(), json!(record.generator.to_string()));
    map.insert("prompt".into(), json!(record.prompt));
    if let Some(model) = &record.model {
        map.insert("model".into(), json!(model));
    }
    if !record.loras.is_empty() {
        map.insert("loras".into(), serde_json::to_value(&record.loras).unwrap());
    }
    if let Some(positive) = &record.positive_prompt {
        map.insert("positive_prompt".into(), json!(positive));
    }
    if let Some(negative) = &record.negative_prompt {
        map.insert("negative_prompt".into(), json!(negative));
    }
    Value::Object(map)
}

fn damage(fixture: &Value, bytes: &mut Vec<u8>) {
    if let Some(patches) = fixture["patch"].as_array() {
        for patch in patches {
            let offset = patch["offset"].as_u64().expect("patch offset") as usize;
            let data = hex(&patch["hex"]);
            bytes[offset..offset + data.len()].copy_from_slice(&data);
        }
    }
    if let Some(len) = fixture["truncate_to"].as_u64() {
        bytes.truncate(len as usize);
    }
    if let Some(cut) = fixture["truncate_by"].as_u64() {
        bytes.truncate(bytes.len() - cut as usize);
    }
}

fn hex(value: &Value) -> Vec<u8> {
    let text = value.as_str().expect("hex string");
    assert_eq!(text.len() % 2, 0, "odd hex length");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex digit"))
        .collect()
}

fn fourcc(value: &Value) -> [u8; 4] {
    value.as_str().expect("chunk type").as_bytes().try_into().expect("4-byte chunk type")
}

fn build_image(fixture: &Value) -> (&'static str, Vec<u8>) {
    let entries = fixture["metadata"].as_array().expect("metadata array");
    match fixture["container"].as_str() {
        Some("png") => ("png", build_png(entries)),
        Some("jpeg") => ("jpg", build_jpeg(entries)),
        Some("webp") => ("webp", build_webp(entries)),
        other => panic!("unknown container {other:?}"),
    }
}

/// `text` verbatim, or `json` serialized the way ComfyUI writes it (ASCII-only).
fn entry_text(entry: &Value) -> String {
    if let Some(text) = entry["text"].as_str() {
        return text.to_string();
    }
    let json = &entry["json"];
    assert!(!json.is_null(), "entry needs text or json: {entry}");
    let mut out = String::new();
    for c in serde_json::to_string(json).unwrap().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

fn build_png(entries: &[Value]) -> Vec<u8> {
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0; 4]); // CRC is not checked
        out
    }
    fn latin1(text: &str) -> Vec<u8> {
        text.chars()
            .map(|c| u8::try_from(u32::from(c)).expect("tEXt holds Latin-1 only"))
            .collect()
    }

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
    for entry in entries {
        if entry.get("hex").is_some() {
            png.extend(chunk(&fourcc(&entry["chunk"]), &hex(&entry["hex"])));
            continue;
        }
        let keyword = entry["keyword"].as_str().expect("keyword");
        let text = entry_text(entry);
        let mut data = latin1(keyword);
        data.push(0);
        match entry["chunk"].as_str() {
            Some("tEXt") => {
                data.extend(latin1(&text));
                png.extend(chunk(b"tEXt", &data));
            }
            Some("iTXt") => {
                let compressed = entry["compressed"].as_bool().unwrap_or(false);
                data.extend_from_slice(&[u8::from(compressed), 0, 0, 0]); // flag, method, lang, translated keyword
                if compressed {
                    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                    encoder.write_all(text.as_bytes()).unwrap();
                    data.extend(encoder.finish().unwrap());
                } else {
                    data.extend_from_slice(text.as_bytes());
                }
                png.extend(chunk(b"iTXt", &data));
            }
            other => panic!("unknown png chunk {other:?}"),
        }
    }
    png.extend(chunk(b"IDAT", &[0; 16]));
    png.extend(chunk(b"IEND", &[]));
    png
}

fn build_jpeg(entries: &[Value]) -> Vec<u8> {
    fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
        let len = u16::try_from(body.len() + 2).expect("JPEG segment over 64 KB");
        let mut out = vec![0xFF, marker];
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    let mut jpeg = vec![0xFF, 0xD8];
    for entry in entries {
        if let Some(marker) = entry["marker"].as_str() {
            let marker = u8::from_str_radix(marker, 16).expect("marker byte");
            if entry.get("hex").is_some() {
                jpeg.extend(segment(marker, &hex(&entry["hex"])));
            } else {
                jpeg.extend_from_slice(&[0xFF, marker]);
            }
            continue;
        }
        match entry["segment"].as_str() {
            Some("COM") => jpeg.extend(segment(0xFE, entry_text(entry).as_bytes())),
            Some("XMP") => {
                let mut body = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
                body.extend_from_slice(entry_text(entry).as_bytes());
                jpeg.extend(segment(0xE1, &body));
            }
            Some("Exif") => {
                let mut body = b"Exif\0\0".to_vec();
                body.extend(build_tiff(entry));
                jpeg.extend(segment(0xE1, &body));
            }
            other => panic!("unknown jpeg segment {other:?}"),
        }
    }
    jpeg.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
    jpeg.extend_from_slice(&[0x12, 0x34, 0xFF, 0xD9]);
    jpeg
}

fn build_webp(entries: &[Value]) -> Vec<u8> {
    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    let mut body = b"WEBP".to_vec();
    body.extend(chunk(b"VP8X", &[0x0C, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
    // Odd-sized image data before the metadata, as real files have, so the parser must
    // skip it including its padding byte.
    body.extend(chunk(b"VP8 ", &[0x55; 1025]));
    for entry in entries {
        if entry.get("hex").is_some() {
            body.extend(chunk(&fourcc(&entry["chunk"]), &hex(&entry["hex"])));
            continue;
        }
        match entry["chunk"].as_str() {
            Some("XMP ") => body.extend(chunk(b"XMP ", entry_text(entry).as_bytes())),
            Some("EXIF") => {
                let mut data = Vec::new();
                if entry["exif_header"].as_bool().unwrap_or(false) {
                    data.extend_from_slice(b"Exif\0\0");
                }
                data.extend(build_tiff(entry));
                body.extend(chunk(b"EXIF", &data));
            }
            other => panic!("unknown webp chunk {other:?}"),
        }
    }
    let mut webp = b"RIFF".to_vec();
    webp.extend_from_slice(&(body.len() as u32).to_le_bytes());
    webp.extend(body);
    webp
}

/// One IFD entry; `data` is the value's bytes (stored inline when 4 bytes or fewer).
struct Tag {
    tag: u16,
    kind: u16,
    count: u32,
    data: Vec<u8>,
}

/// TIFF structure for an EXIF entry: IFD0 with an Orientation tag (as real files have, so
/// the reader steps over other tags), the `ifd0_ascii` tags and the UserComment, which sits
/// in IFD0 or in an Exif sub-IFD. Values longer than 4 bytes follow the IFDs.
fn build_tiff(entry: &Value) -> Vec<u8> {
    let uc = &entry["user_comment"];
    let byte_order = entry["byte_order"].as_str().or(uc["byte_order"].as_str());
    let le = match byte_order {
        Some("II") => true,
        Some("MM") => false,
        other => panic!("unknown byte_order {other:?}"),
    };
    let u16b = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let u32b = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };

    let mut ifd0 = vec![Tag { tag: 0x0112, kind: 3, count: 1, data: u16b(1).to_vec() }];
    for ascii in entry["ifd0_ascii"].as_array().into_iter().flatten() {
        let tag = u16::from_str_radix(ascii["tag"].as_str().expect("tag").trim_start_matches("0x"), 16)
            .expect("hex tag");
        let mut data = entry_text(ascii).into_bytes();
        data.push(0);
        ifd0.push(Tag { tag, kind: 2, count: data.len() as u32, data });
    }
    let mut exif_ifd = Vec::new();
    if !uc.is_null() {
        let value = user_comment_value(uc);
        let user_comment = Tag { tag: 0x9286, kind: 7, count: value.len() as u32, data: value };
        match uc["ifd"].as_str() {
            Some("ifd0") => ifd0.push(user_comment),
            Some("exif") => exif_ifd.push(user_comment),
            other => panic!("unknown ifd {other:?}"),
        }
    }

    let ifd_size = |n: usize| (2 + 12 * n + 4) as u32;
    let ifd0_len = ifd0.len() + usize::from(!exif_ifd.is_empty());
    let exif_offset = 8 + ifd_size(ifd0_len);
    if !exif_ifd.is_empty() {
        ifd0.push(Tag { tag: 0x8769, kind: 4, count: 1, data: u32b(exif_offset).to_vec() });
    }
    ifd0.sort_by_key(|tag| tag.tag);
    let mut data_offset = exif_offset + if exif_ifd.is_empty() { 0 } else { ifd_size(exif_ifd.len()) };

    let mut tiff = if le { b"II".to_vec() } else { b"MM".to_vec() };
    tiff.extend_from_slice(&u16b(42));
    tiff.extend_from_slice(&u32b(8));
    let mut values = Vec::new();
    for ifd in [&ifd0, &exif_ifd] {
        if ifd.is_empty() {
            continue;
        }
        tiff.extend_from_slice(&u16b(ifd.len() as u16));
        for tag in ifd {
            tiff.extend_from_slice(&u16b(tag.tag));
            tiff.extend_from_slice(&u16b(tag.kind));
            tiff.extend_from_slice(&u32b(tag.count));
            if tag.data.len() <= 4 {
                let mut inline = [0u8; 4];
                inline[..tag.data.len()].copy_from_slice(&tag.data);
                tiff.extend_from_slice(&inline);
            } else {
                tiff.extend_from_slice(&u32b(data_offset));
                data_offset += tag.data.len() as u32;
                values.extend_from_slice(&tag.data);
            }
        }
        tiff.extend_from_slice(&u32b(0));
    }
    tiff.extend(values);
    tiff
}

fn user_comment_value(uc: &Value) -> Vec<u8> {
    let text = entry_text(uc);
    let mut value = Vec::new();
    match uc["charset"].as_str() {
        Some("UNICODE") => {
            value.extend_from_slice(b"UNICODE\0");
            let order = uc["utf16"].as_str().expect("utf16 byte order");
            let big_endian = match order {
                "be" | "bom-be" => true,
                "le" | "bom-le" => false,
                other => panic!("unknown utf16 order {other:?}"),
            };
            let units = order.starts_with("bom").then_some(0xFEFF).into_iter().chain(text.encode_utf16());
            for unit in units {
                value.extend_from_slice(&if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() });
            }
        }
        Some("ASCII") => {
            value.extend_from_slice(b"ASCII\0\0\0");
            value.extend_from_slice(text.as_bytes());
        }
        Some("undefined") => {
            value.extend_from_slice(&[0; 8]);
            value.extend_from_slice(text.as_bytes());
        }
        Some("none") => value.extend_from_slice(text.as_bytes()),
        other => panic!("unknown charset {other:?}"),
    }
    assert!(value.len() > 4, "UserComment fixtures must not fit inline in the IFD entry");
    value
}
