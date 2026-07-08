use base64::{Engine as _, engine::general_purpose::STANDARD};

pub const MAX_BLOB_SIZE_BYTES: usize = 15 * 1024 * 1024;
pub const MAX_BLOB_BASE64_BYTES: usize = MAX_BLOB_SIZE_BYTES.div_ceil(3) * 4;

pub fn decode_blob_data_b64(data_b64: &str) -> anyhow::Result<Vec<u8>> {
    if data_b64.len() > MAX_BLOB_BASE64_BYTES {
        anyhow::bail!("blob exceeds 15 MB decoded size cap");
    }
    let data = STANDARD
        .decode(data_b64)
        .map_err(|error| anyhow::anyhow!("invalid base64 blob data: {error}"))?;
    if data.len() > MAX_BLOB_SIZE_BYTES {
        anyhow::bail!("blob exceeds 15 MB decoded size cap");
    }
    Ok(data)
}

pub fn sanitize_blob_name(name: &str) -> String {
    let basename = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let sanitized = basename
        .chars()
        .map(|ch| if ch.is_control() { '_' } else { ch })
        .collect::<String>();
    if sanitized.is_empty() {
        "attachment".to_string()
    } else {
        sanitized
    }
}

pub fn normalize_mime(mime: &str) -> String {
    let mime = mime.trim();
    if mime.is_empty() {
        "application/octet-stream".to_string()
    } else {
        mime.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_blob_name_keeps_basename_only() {
        assert_eq!(sanitize_blob_name("../secret.txt"), "secret.txt");
        assert_eq!(sanitize_blob_name(r"C:\tmp\photo.png"), "photo.png");
        assert_eq!(sanitize_blob_name(""), "attachment");
    }

    #[test]
    fn decode_blob_data_rejects_over_size_payloads_before_allocating() {
        let too_large = "A".repeat(MAX_BLOB_BASE64_BYTES + 4);
        let error = decode_blob_data_b64(&too_large).unwrap_err();
        assert!(error.to_string().contains("15 MB"));
    }
}
