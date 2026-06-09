// Screenshot engine. The frontend captures the chart canvas and sends raw PNG
// bytes (base64-encoded). This module decodes and persists them locally.

use std::path::{Path, PathBuf};

/// Decode base64 PNG data and write it to `<app_dir>/screenshots/<filename>`.
/// Returns the absolute path of the saved file.
pub fn save_to_disk(app_dir: &Path, filename: &str, image_base64: &str) -> Result<String, String> {
    let screenshots_dir = app_dir.join("screenshots");
    std::fs::create_dir_all(&screenshots_dir)
        .map_err(|e| format!("cannot create screenshots dir: {e}"))?;

    let bytes = decode_base64(image_base64)?;

    let path: PathBuf = screenshots_dir.join(filename);
    std::fs::write(&path, &bytes)
        .map_err(|e| format!("cannot write screenshot: {e}"))?;

    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "screenshot path is not valid UTF-8".into())
}

fn decode_base64(data: &str) -> Result<Vec<u8>, String> {
    // Strip optional data-URL prefix (data:image/png;base64,...)
    let raw = if let Some(pos) = data.find(',') {
        &data[pos + 1..]
    } else {
        data
    };
    base64_decode(raw.as_bytes()).map_err(|e| format!("base64 decode error: {e}"))
}

// Manual base64 decoder so we avoid adding a crate dependency.
// Uses the standard alphabet (A–Z a–z 0–9 + /).
fn base64_decode(input: &[u8]) -> Result<Vec<u8>, String> {
    const TABLE: [i8; 256] = {
        let mut t = [-1i8; 256];
        let mut i = 0usize;
        while i < 26 { t[b'A' as usize + i] = i as i8; i += 1; }
        i = 0;
        while i < 26 { t[b'a' as usize + i] = (26 + i) as i8; i += 1; }
        i = 0;
        while i < 10 { t[b'0' as usize + i] = (52 + i) as i8; i += 1; }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };

    let input: Vec<u8> = input.iter().copied().filter(|&b| b != b'\n' && b != b'\r' && b != b' ').collect();
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut i = 0;
    while i + 3 < input.len() {
        let a = TABLE[input[i]     as usize];
        let b = TABLE[input[i + 1] as usize];
        let c = if input[i + 2] == b'=' { 0 } else { TABLE[input[i + 2] as usize] };
        let d = if input[i + 3] == b'=' { 0 } else { TABLE[input[i + 3] as usize] };
        if a < 0 || b < 0 || c < 0 || d < 0 {
            return Err(format!("invalid base64 char at byte {i}"));
        }
        let n = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);
        out.push((n >> 16) as u8);
        if input[i + 2] != b'=' { out.push((n >> 8) as u8); }
        if input[i + 3] != b'=' { out.push(n as u8); }
        i += 4;
    }
    Ok(out)
}
