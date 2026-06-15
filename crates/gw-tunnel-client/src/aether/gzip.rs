use bytes::Bytes;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};

pub const COMPRESS_MIN_SIZE: usize = 512;

pub fn maybe_compress(payload: Bytes) -> Result<(Bytes, bool), std::io::Error> {
    if payload.len() < COMPRESS_MIN_SIZE {
        return Ok((payload, false));
    }

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&payload)?;
    let compressed = encoder.finish()?;
    if compressed.len() < payload.len() {
        Ok((Bytes::from(compressed), true))
    } else {
        Ok((payload, false))
    }
}

pub fn decompress(payload: &[u8]) -> Result<Bytes, std::io::Error> {
    let mut decoder = GzDecoder::new(payload);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(Bytes::from(out))
}
