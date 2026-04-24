use std::io::{BufReader, Read};
use std::path::Path;

use crate::ips::types::{Generator, PromptRecord};
use super::comfyui;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

pub fn extract(path: &Path, verbose: bool) -> Vec<PromptRecord> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            if verbose {
                eprintln!("ips: cannot read {}: {}", path.display(), e);
            }
            return vec![];
        }
    };

    let mut reader = BufReader::new(file);

    let mut sig = [0u8; 8];
    if reader.read_exact(&mut sig).is_err() || &sig != PNG_SIGNATURE {
        if verbose {
            eprintln!("ips: {}: not a valid PNG", path.display());
        }
        return vec![];
    }

    let mut results = Vec::new();

    loop {
        let mut length_buf = [0u8; 4];
        match reader.read_exact(&mut length_buf) {
            Ok(_) => {}
            Err(_) => break,
        }
        let length = u32::from_be_bytes(length_buf) as usize;

        let mut chunk_type = [0u8; 4];
        if reader.read_exact(&mut chunk_type).is_err() {
            break;
        }

        if &chunk_type == b"IDAT" {
            break;
        }

        let mut chunk_data = vec![0u8; length];
        if reader.read_exact(&mut chunk_data).is_err() {
            if verbose {
                eprintln!("ips: {}: truncated chunk", path.display());
            }
            break;
        }

        let mut crc_buf = [0u8; 4];
        if reader.read_exact(&mut crc_buf).is_err() {
            break;
        }

        match &chunk_type {
            b"tEXt" => {
                if let Some((keyword, value)) = parse_text_chunk(&chunk_data) {
                    process_keyword(path, &keyword, &value, &mut results);
                }
            }
            b"iTXt" => {
                if let Some((keyword, value)) = parse_itxt_chunk(&chunk_data) {
                    process_keyword(path, &keyword, &value, &mut results);
                }
            }
            _ => {}
        }
    }

    results
}

fn parse_text_chunk(data: &[u8]) -> Option<(String, String)> {
    let null_pos = data.iter().position(|&b| b == 0)?;
    let keyword = String::from_utf8_lossy(&data[..null_pos]).into_owned();
    let value = String::from_utf8_lossy(&data[null_pos + 1..]).into_owned();
    Some((keyword, value))
}

fn parse_itxt_chunk(data: &[u8]) -> Option<(String, String)> {
    let kw_end = data.iter().position(|&b| b == 0)?;
    let keyword = String::from_utf8_lossy(&data[..kw_end]).into_owned();

    let mut pos = kw_end + 3;
    if pos > data.len() {
        return None;
    }

    let lang_end = data[pos..].iter().position(|&b| b == 0)?;
    pos += lang_end + 1;

    let trans_end = data[pos..].iter().position(|&b| b == 0)?;
    pos += trans_end + 1;

    let value = String::from_utf8_lossy(&data[pos..]).into_owned();
    Some((keyword, value))
}

fn process_keyword(path: &Path, keyword: &str, value: &str, results: &mut Vec<PromptRecord>) {
    match keyword {
        "parameters" => {
            results.push(PromptRecord {
                path: path.to_path_buf(),
                prompt: value.to_string(),
                generator: Generator::A1111,
                metadata_key: "parameters",
            });
        }
        "prompt" => {
            match serde_json::from_str::<serde_json::Value>(value) {
                Ok(json) => {
                    let prompts = comfyui::extract_from_workflow(&json);
                    if !prompts.is_empty() {
                        results.push(PromptRecord {
                            path: path.to_path_buf(),
                            prompt: prompts.join(" | "),
                            generator: Generator::ComfyUI,
                            metadata_key: "prompt",
                        });
                    }
                }
                Err(_) => {
                    if !value.trim().is_empty() {
                        results.push(PromptRecord {
                            path: path.to_path_buf(),
                            prompt: value.to_string(),
                            generator: Generator::Unknown,
                            metadata_key: "prompt",
                        });
                    }
                }
            }
        }
        "Comment" => {
            match serde_json::from_str::<serde_json::Value>(value) {
                Ok(json) => {
                    if let Some(prompt) = json.get("prompt").and_then(|v| v.as_str()) {
                        results.push(PromptRecord {
                            path: path.to_path_buf(),
                            prompt: prompt.to_string(),
                            generator: Generator::NovelAI,
                            metadata_key: "Comment",
                        });
                    }
                }
                Err(_) => {
                    if !value.trim().is_empty() {
                        results.push(PromptRecord {
                            path: path.to_path_buf(),
                            prompt: value.to_string(),
                            generator: Generator::Unknown,
                            metadata_key: "Comment",
                        });
                    }
                }
            }
        }
        "Description" => {
            if !value.trim().is_empty() {
                results.push(PromptRecord {
                    path: path.to_path_buf(),
                    prompt: value.to_string(),
                    generator: Generator::NovelAI,
                    metadata_key: "Description",
                });
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_png_chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let length = (data.len() as u32).to_be_bytes();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&length);
        chunk.extend_from_slice(chunk_type);
        chunk.extend_from_slice(data);
        chunk.extend_from_slice(&[0u8; 4]);
        chunk
    }

    fn make_png_with_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut png = Vec::new();
        png.extend_from_slice(PNG_SIGNATURE);
        for chunk in chunks {
            png.extend_from_slice(chunk);
        }
        png.extend_from_slice(&make_png_chunk(b"IEND", &[]));
        png
    }

    #[test]
    fn parse_text_chunk_basic() {
        let data = b"parameters\x00a1111 prompt text";
        let result = parse_text_chunk(data).unwrap();
        assert_eq!(result.0, "parameters");
        assert_eq!(result.1, "a1111 prompt text");
    }

    #[test]
    fn extracts_a1111_parameters() {
        let chunk_data = b"parameters\x00masterpiece, 1girl, solo".to_vec();
        let chunks = vec![make_png_chunk(b"tEXt", &chunk_data)];
        let png_bytes = make_png_with_chunks(&chunks);

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        std::fs::write(&path, &png_bytes).unwrap();

        let records = extract(&path, false);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].generator, Generator::A1111);
        assert_eq!(records[0].prompt, "masterpiece, 1girl, solo");
    }

    #[test]
    fn extracts_novelai_comment() {
        let json = r#"{"prompt":"1girl, masterpiece","steps":28}"#;
        let mut data = b"Comment\x00".to_vec();
        data.extend_from_slice(json.as_bytes());
        let chunks = vec![make_png_chunk(b"tEXt", &data)];
        let png_bytes = make_png_with_chunks(&chunks);

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        std::fs::write(&path, &png_bytes).unwrap();

        let records = extract(&path, false);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].generator, Generator::NovelAI);
        assert_eq!(records[0].prompt, "1girl, masterpiece");
    }

    #[test]
    fn stops_at_idat() {
        let chunk_before = make_png_chunk(b"tEXt", b"parameters\x00before idat");
        let idat = make_png_chunk(b"IDAT", b"\x00\x00");
        let chunk_after = make_png_chunk(b"tEXt", b"parameters\x00after idat");

        let mut png = Vec::new();
        png.extend_from_slice(PNG_SIGNATURE);
        png.extend_from_slice(&chunk_before);
        png.extend_from_slice(&idat);
        png.extend_from_slice(&chunk_after);

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        std::fs::write(&path, &png).unwrap();

        let records = extract(&path, false);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].prompt, "before idat");
    }
}
