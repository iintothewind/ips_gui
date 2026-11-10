const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_USER_COMMENT: u16 = 0x9286;
const TYPE_ASCII: u16 = 2;
const EXIF_PREFIX: &[u8] = b"Exif\0\0";

fn tiff_data(data: &[u8]) -> &[u8] {
    data.strip_prefix(EXIF_PREFIX).unwrap_or(data)
}

pub fn extract_user_comment(data: &[u8]) -> Option<String> {
    ExifReader::new(tiff_data(data))?.user_comment()
}

/// ComfyUI's WebP (and AVIF) savers store each graph in an IFD0 ASCII tag as `key:{json}`:
/// Model (0x0110) holds `prompt:…`, Make (0x010F) `workflow:…`. Returns `(key, json text)`
/// for the `prompt` and `workflow` keys, in tag order. A value cut short by the end of the
/// data is returned as far as it goes.
pub fn extract_comfyui_tags(data: &[u8]) -> Vec<(&'static str, String)> {
    ExifReader::new(tiff_data(data))
        .and_then(|reader| reader.comfyui_tags())
        .unwrap_or_default()
}

struct ExifReader<'a> {
    data: &'a [u8],
    le: bool,
}

impl<'a> ExifReader<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let le = match &data[0..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let magic = read_u16(data, 2, le)?;
        if magic != 42 {
            return None;
        }
        Some(Self { data, le })
    }

    fn u16(&self, off: usize) -> Option<u16> {
        read_u16(self.data, off, self.le)
    }

    fn u32(&self, off: usize) -> Option<u32> {
        read_u32(self.data, off, self.le)
    }

    fn user_comment(&self) -> Option<String> {
        let ifd0 = self.u32(4)? as usize;
        self.scan_ifd(ifd0, 0)
    }

    fn comfyui_tags(&self) -> Option<Vec<(&'static str, String)>> {
        let ifd0 = self.u32(4)? as usize;
        let entry_count = self.u16(ifd0)? as usize;
        let mut tags = Vec::new();
        for i in 0..entry_count {
            let e = ifd0 + 2 + i * 12;
            if e + 12 > self.data.len() {
                break;
            }
            if self.u16(e + 2)? != TYPE_ASCII {
                continue;
            }
            let count = self.u32(e + 4)? as usize;
            let offset = if count <= 4 { e + 8 } else { self.u32(e + 8)? as usize };
            let Some(rest) = self.data.get(offset..) else {
                continue;
            };
            let text = String::from_utf8_lossy(&rest[..count.min(rest.len())]);
            if let Some((key, json)) = text.trim_end_matches('\0').split_once(':') {
                let key = match key {
                    "prompt" => "prompt",
                    "workflow" => "workflow",
                    _ => continue,
                };
                tags.push((key, json.to_string()));
            }
        }
        Some(tags)
    }

    fn scan_ifd(&self, offset: usize, depth: u8) -> Option<String> {
        if depth > 4 {
            return None;
        }
        let entry_count = self.u16(offset)? as usize;
        let base = offset + 2;

        let mut exif_ifd: Option<usize> = None;

        for i in 0..entry_count {
            let e = base + i * 12;
            if e + 12 > self.data.len() {
                break;
            }
            let tag = self.u16(e)?;
            let count = self.u32(e + 4)? as usize;

            match tag {
                TAG_USER_COMMENT => {
                    // Values of up to 4 bytes are stored in the entry itself.
                    let val_off = if count <= 4 { e + 8 } else { self.u32(e + 8)? as usize };
                    if let Some(text) = self.decode_user_comment(val_off, count) {
                        return Some(text);
                    }
                }
                TAG_EXIF_IFD => {
                    exif_ifd = Some(self.u32(e + 8)? as usize);
                }
                _ => {}
            }
        }

        if let Some(off) = exif_ifd {
            return self.scan_ifd(off, depth + 1);
        }

        None
    }

    fn decode_user_comment(&self, offset: usize, count: usize) -> Option<String> {
        if offset >= self.data.len() {
            return None;
        }
        let count = count.min(self.data.len() - offset);
        let value = &self.data[offset..offset + count];

        // The 8-byte character code is optional in practice: Pillow and ComfyUI JPEG savers
        // store a plain string, which must keep its first 8 bytes.
        let text = match value.get(..8) {
            Some(b"UNICODE\0") => decode_utf16(&value[8..], self.le),
            Some(b"ASCII\0\0\0") | Some(b"JIS\0\0\0\0\0") | Some(b"\0\0\0\0\0\0\0\0") => {
                String::from_utf8_lossy(&value[8..]).into_owned()
            }
            _ => String::from_utf8_lossy(value).into_owned(),
        };

        let text = text.trim_matches('\0').trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    }
}

/// Decode EXIF "UNICODE" text. A BOM decides the byte order; otherwise the order is guessed
/// from where the zero bytes of ASCII characters fall, because writers disagree (piexif and
/// most ComfyUI savers always write big-endian, even inside little-endian TIFF), and the
/// TIFF byte order is only the fallback.
fn decode_utf16(bytes: &[u8], tiff_le: bool) -> String {
    let (le, data) = match bytes.get(0..2) {
        Some(&[0xFF, 0xFE]) => (true, &bytes[2..]),
        Some(&[0xFE, 0xFF]) => (false, &bytes[2..]),
        _ => (guess_utf16_le(bytes).unwrap_or(tiff_le), bytes),
    };

    let words: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| if le {
            u16::from_le_bytes([c[0], c[1]])
        } else {
            u16::from_be_bytes([c[0], c[1]])
        })
        .collect();

    String::from_utf16_lossy(&words)
}

/// `Some(true)` for little-endian, `Some(false)` for big-endian, `None` when the bytes give
/// no hint (no zero bytes, as in text without any ASCII).
fn guess_utf16_le(bytes: &[u8]) -> Option<bool> {
    let (mut even, mut odd) = (0usize, 0usize);
    for pair in bytes.chunks_exact(2) {
        even += usize::from(pair[0] == 0);
        odd += usize::from(pair[1] == 0);
    }
    match even.cmp(&odd) {
        std::cmp::Ordering::Greater => Some(false),
        std::cmp::Ordering::Less => Some(true),
        std::cmp::Ordering::Equal => None,
    }
}

fn read_u16(data: &[u8], off: usize, le: bool) -> Option<u16> {
    if off + 2 > data.len() {
        return None;
    }
    Some(if le {
        u16::from_le_bytes([data[off], data[off + 1]])
    } else {
        u16::from_be_bytes([data[off], data[off + 1]])
    })
}

fn read_u32(data: &[u8], off: usize, le: bool) -> Option<u32> {
    if off + 4 > data.len() {
        return None;
    }
    Some(if le {
        u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
    } else {
        u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_exif_user_comment(comment: &str) -> Vec<u8> {
        let charset = b"ASCII\0\0\0";
        let text = comment.as_bytes();
        let count = (8 + text.len()) as u32;
        let value_offset: u32 = 26;

        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&TAG_USER_COMMENT.to_le_bytes());
        buf.extend_from_slice(&7u16.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&value_offset.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(charset);
        buf.extend_from_slice(text);
        buf
    }

    #[test]
    fn ascii_user_comment() {
        let exif = make_exif_user_comment("1girl, masterpiece");
        assert_eq!(extract_user_comment(&exif).unwrap(), "1girl, masterpiece");
    }

    #[test]
    fn with_exif_prefix() {
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&make_exif_user_comment("sunset landscape"));
        assert_eq!(extract_user_comment(&exif).unwrap(), "sunset landscape");
    }

    #[test]
    fn returns_none_for_empty_comment() {
        let exif = make_exif_user_comment("   ");
        assert!(extract_user_comment(&exif).is_none());
    }

    #[test]
    fn returns_none_for_garbage() {
        assert!(extract_user_comment(b"not exif data").is_none());
    }

    #[test]
    fn unicode_le_user_comment() {
        let text = "cyberpunk city";
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let count = (8 + utf16.len()) as u32;
        let value_offset: u32 = 26;

        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&TAG_USER_COMMENT.to_le_bytes());
        buf.extend_from_slice(&7u16.to_le_bytes());
        buf.extend_from_slice(&count.to_le_bytes());
        buf.extend_from_slice(&value_offset.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(b"UNICODE\0");
        buf.extend_from_slice(&utf16);

        assert_eq!(extract_user_comment(&buf).unwrap(), "cyberpunk city");
    }

    #[test]
    fn unicode_be_user_comment() {
        let text = "score_9, masterpiece";
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|c| c.to_be_bytes()).collect();
        let count = (8 + utf16.len()) as u32;
        let value_offset: u32 = 26;

        let mut buf = Vec::new();
        buf.extend_from_slice(b"MM");
        buf.extend_from_slice(&42u16.to_be_bytes());
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&TAG_USER_COMMENT.to_be_bytes());
        buf.extend_from_slice(&7u16.to_be_bytes());
        buf.extend_from_slice(&count.to_be_bytes());
        buf.extend_from_slice(&value_offset.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(b"UNICODE\0");
        buf.extend_from_slice(&utf16);

        assert_eq!(extract_user_comment(&buf).unwrap(), "score_9, masterpiece");
    }

    #[test]
    fn unicode_bom_overrides_tiff_order() {
        let text = "landscape";
        let utf16: Vec<u8> = text.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let bom = [0xFF_u8, 0xFE];
        let count = (8 + 2 + utf16.len()) as u32;
        let value_offset: u32 = 26;

        let mut buf = Vec::new();
        buf.extend_from_slice(b"MM");
        buf.extend_from_slice(&42u16.to_be_bytes());
        buf.extend_from_slice(&8u32.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&TAG_USER_COMMENT.to_be_bytes());
        buf.extend_from_slice(&7u16.to_be_bytes());
        buf.extend_from_slice(&count.to_be_bytes());
        buf.extend_from_slice(&value_offset.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(b"UNICODE\0");
        buf.extend_from_slice(&bom);
        buf.extend_from_slice(&utf16);

        assert_eq!(extract_user_comment(&buf).unwrap(), "landscape");
    }
}
