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
    fn rejects_non_blob_strings() {
        assert_eq!(decode_blob("hello"), None);
        assert_eq!(decode_blob("BLOB:1:text"), None);
        assert_eq!(decode_blob("BLOB:1:text/plain:!!!"), None);
    }
}
