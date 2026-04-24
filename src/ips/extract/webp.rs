use std::path::Path;

use crate::ips::types::{Generator, PromptRecord};
use super::{exif, jpeg};

const RIFF_MAGIC: &[u8; 4] = b"RIFF";
const WEBP_MAGIC: &[u8; 4] = b"WEBP";

pub fn extract(path: &Path, verbose: bool) -> Vec<PromptRecord> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            if verbose {
                eprintln!("ips: cannot read {}: {}", path.display(), e);
            }
            return vec![];
        }
    };

    if data.len() < 12 {
        if verbose {
            eprintln!("ips: {}: file too small to be a WebP", path.display());
        }
        return vec![];
    }

    if &data[0..4] != RIFF_MAGIC || &data[8..12] != WEBP_MAGIC {
        if verbose {
            eprintln!("ips: {}: not a valid WebP", path.display());
        }
        return vec![];
    }

    let mut results = Vec::new();
    let mut pos = 12usize;

    while pos + 8 <= data.len() {
        let chunk_id = &data[pos..pos + 4];
        let chunk_size =
            u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                as usize;
        pos += 8;

        if pos + chunk_size > data.len() {
            if verbose {
                eprintln!("ips: {}: truncated WebP chunk", path.display());
            }
            break;
        }

        let chunk_data = &data[pos..pos + chunk_size];
        let padded = chunk_size + (chunk_size & 1);
        pos += padded;

        match chunk_id {
            b"XMP " => {
                if let Some(prompt) = jpeg::extract_xmp_description(chunk_data) {
                    let generator = jpeg::detect_xmp_generator(chunk_data);
                    results.push(PromptRecord {
                        path: path.to_path_buf(),
                        prompt,
                        generator,
                        metadata_key: "XMP".to_string(),
                    });
                }
            }
            b"EXIF" => {
                if let Some(prompt) = exif::extract_user_comment(chunk_data) {
                    results.push(PromptRecord {
                        path: path.to_path_buf(),
                        prompt,
                        generator: Generator::Unknown,
                        metadata_key: "UserComment".to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_webp_with_xmp(xmp: &str) -> Vec<u8> {
        let xmp_bytes = xmp.as_bytes();
        let chunk_size = xmp_bytes.len() as u32;
        let padded_size = (xmp_bytes.len() + (xmp_bytes.len() & 1)) as u32;
        let riff_size = (4 + 8 + padded_size).to_le_bytes();

        let mut webp = Vec::new();
        webp.extend_from_slice(b"RIFF");
        webp.extend_from_slice(&riff_size);
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(b"XMP ");
        webp.extend_from_slice(&chunk_size.to_le_bytes());
        webp.extend_from_slice(xmp_bytes);
        if xmp_bytes.len() % 2 != 0 {
            webp.push(0);
        }
        webp
    }

    #[test]
    fn extracts_xmp_from_webp() {
        let xmp = r#"<rdf:RDF>
  <rdf:Description>
    <dc:description>
      <rdf:Alt>
        <rdf:li xml:lang="x-default">sunset landscape, watercolor</rdf:li>
      </rdf:Alt>
    </dc:description>
  </rdf:Description>
</rdf:RDF>"#;
        let webp_bytes = make_webp_with_xmp(xmp);

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.webp");
        std::fs::write(&path, &webp_bytes).unwrap();

        let records = extract(&path, false);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].prompt, "sunset landscape, watercolor");
    }

    #[test]
    fn rejects_invalid_webp() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.webp");
        std::fs::write(&path, b"not a webp file at all").unwrap();

        let records = extract(&path, true);
        assert!(records.is_empty());
    }
}
