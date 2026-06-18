use crate::{Error, Result};

pub(crate) fn zstd_encode(p: &[u8]) -> Result<Vec<u8>> {
    zstd::bulk::compress(p, 9).map_err(|err| Error::Compression(err.to_string()))
}

/// Compresses bytes with the same zstd level the writer uses for text clusters.
pub fn compress(p: &[u8]) -> Result<Vec<u8>> {
    zstd_encode(p)
}

pub(crate) fn zstd_decode(p: &[u8]) -> Result<Vec<u8>> {
    zstd::stream::decode_all(p).map_err(|err| Error::Compression(err.to_string()))
}

pub(crate) fn is_text_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/json"
            | "application/xml"
            | "application/javascript"
            | "application/x-javascript"
    ) || mime.starts_with("text/")
        || mime.ends_with("+xml")
}
