//! BLOB wire format for `BYTES` values: `BLOB:<size>:<mime>:<base64>`.
//! Same format as the PostgreSQL plugin (`bytea`, OID 17).

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

/// Bytes base64-encoded into a read/preview response. The header still
/// reports the true size.
pub const MAX_BLOB_PREVIEW_SIZE: usize = 10_240;

fn mime(data: &[u8]) -> &'static str {
    infer::get(data).map_or("application/octet-stream", |k| k.mime_type())
}

/// Truncated preview used for result grids.
pub fn encode_blob(data: &[u8]) -> String {
    let preview = &data[..data.len().min(MAX_BLOB_PREVIEW_SIZE)];
    format!(
        "BLOB:{}:{}:{}",
        data.len(),
        mime(preview),
        STANDARD.encode(preview)
    )
}

/// Full payload, for exports and row-editor previews.
pub fn encode_blob_full(data: &[u8]) -> String {
    format!(
        "BLOB:{}:{}:{}",
        data.len(),
        mime(data),
        STANDARD.encode(data)
    )
}

/// Default cap on a BLOB file read into memory (100 MiB), the host's default.
/// The host sends its configured `maxBlobSize` with every write.
pub const DEFAULT_MAX_BLOB_SIZE: u64 = 100 * 1024 * 1024;

/// Read the file named by `BLOB_FILE_REF:<size>:<mime>:<path>`, the form the
/// host sends when a user picks a file for a BYTES cell. Larger files are
/// refused before they are read, so a huge pick cannot exhaust memory.
pub fn resolve_blob_file_ref(value: &str, max_size: u64) -> Result<Vec<u8>, String> {
    let rest = value
        .strip_prefix("BLOB_FILE_REF:")
        .ok_or("not a BLOB_FILE_REF")?;
    let mut parts = rest.splitn(3, ':');
    let (size, _mime, path) = (parts.next(), parts.next(), parts.next());
    let (Some(size), Some(path)) = (size, path) else {
        return Err("invalid BLOB_FILE_REF format".into());
    };
    let declared: u64 = size
        .parse()
        .map_err(|_| "invalid file size in BLOB_FILE_REF")?;
    if declared > max_size {
        return Err(format!(
            "file size ({declared} bytes) exceeds the maximum allowed size ({max_size} bytes / {} MB); choose a smaller file",
            max_size / (1024 * 1024)
        ));
    }
    // The declared size comes from the host; check the file itself as well.
    let on_disk = std::fs::metadata(path)
        .map_err(|e| format!("cannot read BLOB file '{path}': {e}"))?
        .len();
    if on_disk > max_size {
        return Err(format!(
            "file size ({on_disk} bytes) exceeds the maximum allowed size ({max_size} bytes / {} MB); choose a smaller file",
            max_size / (1024 * 1024)
        ));
    }
    std::fs::read(path).map_err(|e| format!("cannot read BLOB file '{path}': {e}"))
}

/// Decode the wire format back to bytes. `None` when `value` is not in that
/// format, so callers can treat it as a plain string.
pub fn decode_blob(value: &str) -> Option<Vec<u8>> {
    let rest = value.strip_prefix("BLOB:")?;
    let (_, rest) = rest.split_once(':')?;
    let (_, data) = rest.split_once(':')?;
    STANDARD.decode(data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let bytes = [1u8, 2, 3, 255];
        let s = encode_blob_full(&bytes);
        assert!(s.starts_with("BLOB:4:application/octet-stream:"));
        assert_eq!(decode_blob(&s), Some(bytes.to_vec()));
    }

    #[test]
    fn sniffs_mime() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
        assert!(encode_blob(&png).starts_with("BLOB:12:image/png:"));
    }

    #[test]
    fn preview_truncates_but_reports_true_size() {
        let big = vec![7u8; MAX_BLOB_PREVIEW_SIZE + 100];
        let s = encode_blob(&big);
        assert!(s.starts_with(&format!("BLOB:{}:", big.len())));
        let payload = decode_blob(&s).unwrap();
        assert_eq!(payload.len(), MAX_BLOB_PREVIEW_SIZE);
        assert_eq!(
            decode_blob(&encode_blob_full(&big)).unwrap().len(),
            big.len()
        );
    }

    #[test]
    fn reads_a_blob_file_ref() {
        let dir = std::env::temp_dir().join(format!("blobref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.bin");
        std::fs::write(&file, [9u8, 8, 7]).unwrap();
        let value = format!(
            "BLOB_FILE_REF:3:application/octet-stream:{}",
            file.display()
        );
        assert_eq!(resolve_blob_file_ref(&value, 100).unwrap(), vec![9, 8, 7]);
        // A path may itself contain ':'.
        let colon = dir.join("a:b.bin");
        std::fs::write(&colon, [1u8]).unwrap();
        let value = format!("BLOB_FILE_REF:1:x/y:{}", colon.display());
        assert_eq!(resolve_blob_file_ref(&value, 100).unwrap(), vec![1]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_oversized_missing_and_malformed_file_refs() {
        let dir = std::env::temp_dir().join(format!("blobref-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.bin");
        std::fs::write(&file, vec![0u8; 50]).unwrap();
        // Declared size over the cap: refused before reading.
        let declared = format!("BLOB_FILE_REF:5000:x/y:{}", file.display());
        assert!(resolve_blob_file_ref(&declared, 100)
            .unwrap_err()
            .contains("exceeds"));
        // Declared small, actually large: the file itself is checked.
        let lying = format!("BLOB_FILE_REF:1:x/y:{}", file.display());
        assert!(resolve_blob_file_ref(&lying, 10)
            .unwrap_err()
            .contains("exceeds"));
        assert!(
            resolve_blob_file_ref("BLOB_FILE_REF:1:x/y:/no/such/file", 100)
                .unwrap_err()
                .contains("cannot read")
        );
        for bad in [
            "BLOB_FILE_REF:",
            "BLOB_FILE_REF:1",
            "BLOB_FILE_REF:1:x",
            "BLOB_FILE_REF:abc:x:/p",
            "BLOB:1:x:AA==",
        ] {
            assert!(resolve_blob_file_ref(bad, 100).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_non_blob_strings() {
        assert_eq!(decode_blob("hello"), None);
        assert_eq!(decode_blob("BLOB:1:text"), None);
        assert_eq!(decode_blob("BLOB:1:text/plain:!!!"), None);
    }
}
