use std::io::{BufReader, Read};
use std::path::Path;

use crate::ips::types::{Generator, PromptRecord};
use super::exif;

const SOI_MARKER: [u8; 2] = [0xFF, 0xD8];
const MARKER_PREFIX: u8 = 0xFF;
const SOS_TYPE: u8 = 0xDA;
const COM_TYPE: u8 = 0xFE;
const APP1_TYPE: u8 = 0xE1;
const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const EXIF_HEADER: &[u8] = b"Exif\0\0";

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

    let mut soi = [0u8; 2];
    if reader.read_exact(&mut soi).is_err() || soi != SOI_MARKER {
        if verbose {
            eprintln!("ips: {}: not a valid JPEG", path.display());
        }
        return vec![];
    }

    let mut results = Vec::new();

    loop {
        let mut marker = [0u8; 2];
        if reader.read_exact(&mut marker).is_err() {
            break;
        }

        if marker[0] != MARKER_PREFIX {
            break;
        }

        let marker_type = marker[1];

        if marker_type == 0xD8 || marker_type == 0xD9 {
            if marker_type == 0xD9 {
                break;
            }
            continue;
        }

        if marker_type == SOS_TYPE {
            break;
        }

        let mut len_buf = [0u8; 2];
        if reader.read_exact(&mut len_buf).is_err() {
            break;
        }
        let seg_len = u16::from_be_bytes(len_buf) as usize;
        if seg_len < 2 {
            break;
        }

        let body_len = seg_len - 2;
        let mut body = vec![0u8; body_len];
        if reader.read_exact(&mut body).is_err() {
            if verbose {
                eprintln!("ips: {}: truncated JPEG segment", path.display());
            }
            break;
        }

        match marker_type {
            COM_TYPE => {
                let text = String::from_utf8_lossy(&body).trim().to_string();
                if !text.is_empty() {
                    results.push(PromptRecord {
                        path: path.to_path_buf(),
                        prompt: text,
                        generator: Generator::A1111,
                        metadata_key: "COM".to_string(),
                    });
                }
            }
            APP1_TYPE => {
                if body.starts_with(XMP_HEADER) {
                    let xmp_body = &body[XMP_HEADER.len()..];
                    if let Some(prompt) = extract_xmp_description(xmp_body) {
                        let generator = detect_xmp_generator(xmp_body);
                        results.push(PromptRecord {
                            path: path.to_path_buf(),
                            prompt,
                            generator,
                            metadata_key: "XMP".to_string(),
                        });
                    }
                } else if body.starts_with(EXIF_HEADER) {
                    if let Some(prompt) = exif::extract_user_comment(&body) {
                        results.push(PromptRecord {
                            path: path.to_path_buf(),
                            prompt,
                            generator: Generator::Unknown,
                            metadata_key: "UserComment".to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    results
}

pub fn extract_xmp_description(xmp_bytes: &[u8]) -> Option<String> {
    let xmp = String::from_utf8_lossy(xmp_bytes);

    let dc_start = xmp.find("<dc:description>")?;
    let after_dc = &xmp[dc_start + "<dc:description>".len()..];

    let li_start = after_dc.find("<rdf:li")?;
    let li_content = &after_dc[li_start..];

    let content_start = li_content.find('>')?;
    let text = &li_content[content_start + 1..];

    let end = text.find("</rdf:li>")?;
    let description = text[..end].trim().to_string();
    if description.is_empty() {
        None
    } else {
        Some(description)
    }
}

pub fn detect_xmp_generator(xmp_bytes: &[u8]) -> Generator {
    let xmp = String::from_utf8_lossy(xmp_bytes);
    if xmp.contains("invokeai:") {
        Generator::InvokeAI
    } else {
        Generator::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_description_extraction() {
        let xmp = r#"<?xpacket?>
<rdf:RDF>
  <rdf:Description>
    <dc:description>
      <rdf:Alt>
        <rdf:li xml:lang="x-default">cyberpunk city, neon lights</rdf:li>
      </rdf:Alt>
    </dc:description>
  </rdf:Description>
</rdf:RDF>"#;
        let result = extract_xmp_description(xmp.as_bytes()).unwrap();
        assert_eq!(result, "cyberpunk city, neon lights");
    }

    #[test]
    fn xmp_description_returns_none_when_absent() {
        let xmp = b"<rdf:RDF><rdf:Description></rdf:Description></rdf:RDF>";
        assert!(extract_xmp_description(xmp).is_none());
    }

    fn make_jpeg_with_com(comment: &str) -> Vec<u8> {
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(&[0xFF, 0xD8]);
        jpeg.extend_from_slice(&[0xFF, 0xFE]);
        let body = comment.as_bytes();
        let len = (body.len() as u16 + 2).to_be_bytes();
        jpeg.extend_from_slice(&len);
        jpeg.extend_from_slice(body);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        jpeg
    }

    #[test]
    fn extracts_com_comment() {
        let jpeg_bytes = make_jpeg_with_com("portrait of a woman, oil painting");

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.jpg");
        std::fs::write(&path, &jpeg_bytes).unwrap();

        let records = extract(&path, false);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].generator, Generator::A1111);
        assert_eq!(records[0].prompt, "portrait of a woman, oil painting");
    }
}
