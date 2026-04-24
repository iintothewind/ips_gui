use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::ips::types::{Generator, PromptRecord};
use super::{exif, jpeg};

const RIFF_MAGIC: &[u8; 4] = b"RIFF";
const WEBP_MAGIC: &[u8; 4] = b"WEBP";

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

    // Validate RIFF/WEBP header (12 bytes) without reading the whole file.
    let mut header = [0u8; 12];
    if reader.read_exact(&mut header).is_err() {
        if verbose {
            eprintln!("ips: {}: file too small to be a WebP", path.display());
        }
        return vec![];
    }

    if &header[0..4] != RIFF_MAGIC || &header[8..12] != WEBP_MAGIC {
        if verbose {
            eprintln!("ips: {}: not a valid WebP", path.display());
        }
        return vec![];
    }

    let mut results = Vec::new();

    loop {
        let mut chunk_header = [0u8; 8];
        if reader.read_exact(&mut chunk_header).is_err() {
            break;
        }

        let chunk_id = &chunk_header[..4];
        let chunk_size = u32::from_le_bytes([
            chunk_header[4], chunk_header[5], chunk_header[6], chunk_header[7],
        ]) as usize;
        // RIFF chunks are padded to even byte boundaries.
        let padded_size = chunk_size + (chunk_size & 1);

        match chunk_id {
            b"XMP " => {
                let mut chunk_data = vec![0u8; chunk_size];
                if reader.read_exact(&mut chunk_data).is_err() {
                    if verbose {
                        eprintln!("ips: {}: truncated XMP chunk", path.display());
                    }
                    break;
                }
                if chunk_size & 1 != 0 {
                    let _ = reader.seek(SeekFrom::Current(1));
                }
                if let Some(prompt) = jpeg::extract_xmp_description(&chunk_data) {
                    let generator = jpeg::detect_xmp_generator(&chunk_data);
                    results.push(PromptRecord {
                        path: path.to_path_buf(),
                        prompt,
                        generator,
                        metadata_key: "XMP",
                    });
                }
            }
            b"EXIF" => {
                let mut chunk_data = vec![0u8; chunk_size];
                if reader.read_exact(&mut chunk_data).is_err() {
                    if verbose {
                        eprintln!("ips: {}: truncated EXIF chunk", path.display());
                    }
                    break;
                }
                if chunk_size & 1 != 0 {
                    let _ = reader.seek(SeekFrom::Current(1));
                }
                if let Some(prompt) = exif::extract_user_comment(&chunk_data) {
                    results.push(PromptRecord {
                        path: path.to_path_buf(),
                        prompt,
                        generator: Generator::Unknown,
                        metadata_key: "UserComment",
                    });
                }
            }
            _ => {
                // Skip image data chunks (VP8, VP8L, VP8X, ANIM, ANMF, …)
                // without reading them into memory.
                if reader.seek(SeekFrom::Current(padded_size as i64)).is_err() {
                    break;
                }
            }
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
    fn skips_vp8_chunk_before_xmp() {
        // Simulate a realistic WebP: VP8L (image data) followed by XMP metadata.
        let xmp = r#"<rdf:RDF><rdf:Description><dc:description><rdf:Alt>
            <rdf:li xml:lang="x-default">after image data</rdf:li>
            </rdf:Alt></dc:description></rdf:Description></rdf:RDF>"#;
        let xmp_bytes = xmp.as_bytes();
        let fake_vp8l = vec![0u8; 1024]; // 1 KB of fake image data

        let xmp_chunk_size = xmp_bytes.len() as u32;
        let vp8l_chunk_size = fake_vp8l.len() as u32;
        let riff_size = (4u32 + 8 + vp8l_chunk_size + 8 + xmp_chunk_size).to_le_bytes();

        let mut webp = Vec::new();
        webp.extend_from_slice(b"RIFF");
        webp.extend_from_slice(&riff_size);
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(b"VP8L");
        webp.extend_from_slice(&vp8l_chunk_size.to_le_bytes());
        webp.extend_from_slice(&fake_vp8l);
        webp.extend_from_slice(b"XMP ");
        webp.extend_from_slice(&xmp_chunk_size.to_le_bytes());
        webp.extend_from_slice(xmp_bytes);

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.webp");
        std::fs::write(&path, &webp).unwrap();

        let records = extract(&path, false);
        assert_eq!(records.len(), 1);
        assert!(records[0].prompt.contains("after image data"));
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
