//! Core `.dict` / `.dict.dz` reader over an in-memory byte slice.
//!
//! The `stardict` crate's `Dict` only opens a `BufReader<File>`, which cannot be
//! handed an APK byte range. This module reads a plain `.dict` directly from the
//! mapped slice, and a `.dict.dz` by parsing the gzip `FEXTRA` "RA" subfield
//! (chunk length + per-chunk compressed sizes) and raw-inflating only the chunks
//! a requested block spans. The last decoded chunk is kept, since a lookup (and
//! the search walk, whose blocks are mostly stored in order) tends to revisit
//! neighbouring chunks.

use std::io::Read;
use std::sync::Arc;

use flate2::read::DeflateDecoder;

use crate::source::Bytes;
use crate::Error;

const GZIP_ID: u16 = 0x8b1f;
const COMPRESSION_METHOD_DEFLATE: u8 = 0x08;
const FLAG_CRC: u8 = 0x02;
const FLAG_EXTRA: u8 = 0x04;
const FLAG_NAME: u8 = 0x08;
const FLAG_COMMENT: u8 = 0x10;
const RA_ID: u16 = 0x4152;

/// A dictzip error, surfaced through the crate's generic StarDict error.
fn bad(msg: &str) -> Error {
    Error::Stardict(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        msg,
    )))
}

fn rd_u16(data: &[u8], off: usize) -> Result<u16, Error> {
    let b = data
        .get(off..off + 2)
        .ok_or_else(|| bad("dictzip: truncated header"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

/// Parsed dictzip geometry: the uncompressed chunk length, the compressed size
/// of each chunk, and where the compressed chunk data begins.
struct DictZipHeader {
    chunk_length: usize,
    chunks: Vec<u16>,
    data_offset: usize,
}

/// Advance past a NUL-terminated string starting at `off`, returning the offset
/// just after the NUL.
fn skip_cstr(data: &[u8], off: usize) -> Result<usize, Error> {
    let rel = data[off..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| bad("dictzip: unterminated header string"))?;
    Ok(off + rel + 1)
}

fn parse_header(data: &[u8]) -> Result<DictZipHeader, Error> {
    // Fixed gzip header: id(2) method(1) flags(1) mtime(4) xfl(1) os(1).
    let id = rd_u16(data, 0)?;
    let method = *data.get(2).ok_or_else(|| bad("dictzip: truncated header"))?;
    let flags = *data.get(3).ok_or_else(|| bad("dictzip: truncated header"))?;
    if id != GZIP_ID {
        return Err(bad("dictzip: bad gzip id"));
    }
    if method != COMPRESSION_METHOD_DEFLATE {
        return Err(bad("dictzip: unsupported compression method"));
    }
    if flags & FLAG_EXTRA == 0 {
        return Err(bad("dictzip: missing extra field"));
    }

    let mut p = 10usize;
    let xlen = rd_u16(data, p)? as usize;
    p += 2;
    let extra_end = p
        .checked_add(xlen)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| bad("dictzip: truncated extra field"))?;

    let mut ra: Option<(usize, usize)> = None;
    while p + 4 <= extra_end {
        // gzip subfield header: SI1+SI2 (2 bytes) then LEN (2 bytes).
        let id = rd_u16(data, p)?;
        let sub_len = rd_u16(data, p + 2)? as usize;
        p += 4;
        let sub_end = p
            .checked_add(sub_len)
            .filter(|&e| e <= extra_end)
            .ok_or_else(|| bad("dictzip: truncated subfield"))?;
        if id == RA_ID {
            ra = Some((p, sub_len));
        }
        p = sub_end;
    }
    let (ra_off, ra_len) =
        ra.ok_or_else(|| bad("dictzip: no extra RA field"))?;

    let version = rd_u16(data, ra_off)?;
    if version != 1 {
        return Err(bad("dictzip: unsupported RA version"));
    }
    let chunk_length = rd_u16(data, ra_off + 2)? as usize;
    let chunk_count = rd_u16(data, ra_off + 4)? as usize;
    if ra_len < 6 + chunk_count * 2 {
        return Err(bad("dictzip: truncated chunk table"));
    }
    let mut chunks = Vec::with_capacity(chunk_count);
    for i in 0..chunk_count {
        chunks.push(rd_u16(data, ra_off + 6 + i * 2)?);
    }

    // Optional trailing header fields, in gzip order.
    let mut p = extra_end;
    if flags & FLAG_NAME != 0 {
        p = skip_cstr(data, p)?;
    }
    if flags & FLAG_COMMENT != 0 {
        p = skip_cstr(data, p)?;
    }
    if flags & FLAG_CRC != 0 {
        p += 2;
    }
    if p > data.len() {
        return Err(bad("dictzip: truncated header"));
    }

    Ok(DictZipHeader {
        chunk_length,
        chunks,
        data_offset: p,
    })
}

/// A `.dict` reader over a mapped or owned byte slice.
pub struct DictReader {
    data: Arc<Bytes>,
    dz: Option<DictZipHeader>,
    last_chunk: Option<(usize, Vec<u8>)>,
}

impl DictReader {
    /// Like a fresh reader, but over an already-shared mapping, so another
    /// handle to the same dictionary reuses the map instead of reopening it.
    pub fn new_shared(data: Arc<Bytes>, compressed: bool) -> Result<DictReader, Error> {
        let dz = if compressed {
            Some(parse_header(data.as_slice())?)
        } else {
            None
        };
        Ok(DictReader {
            data,
            dz,
            last_chunk: None,
        })
    }

    /// The raw bytes of the block at `[offset, offset + size)`. Returns `None`
    /// (rather than an error) when the span lies outside the dictionary, so a
    /// bad `.idx` entry is skipped instead of aborting a whole iteration.
    pub fn block(&mut self, offset: usize, size: usize) -> Result<Option<Vec<u8>>, Error> {
        if self.dz.is_none() {
            let data = self.data.as_slice();
            return match offset.checked_add(size) {
                Some(end) if end <= data.len() => Ok(Some(data[offset..end].to_vec())),
                _ => Ok(None),
            };
        }
        self.read_compressed(offset, size)
    }

    fn read_compressed(&mut self, offset: usize, size: usize) -> Result<Option<Vec<u8>>, Error> {
        if size == 0 {
            return Ok(Some(Vec::new()));
        }
        let (chunk_length, chunk_count) = {
            let h = self.dz.as_ref().unwrap();
            (h.chunk_length, h.chunks.len())
        };
        if chunk_length == 0 {
            return Err(bad("dictzip: zero chunk length"));
        }
        let first = offset / chunk_length;
        let last = offset
            .checked_add(size)
            .and_then(|end| end.checked_sub(1))
            .map(|end| end / chunk_length);
        let Some(last) = last else {
            return Ok(None);
        };
        if last >= chunk_count {
            return Ok(None);
        }
        let start_in_chunk = offset - first * chunk_length;
        let mut out = Vec::with_capacity(size.saturating_add(chunk_length));
        for i in first..=last {
            out.extend_from_slice(self.chunk(i)?);
        }
        let end = start_in_chunk + size;
        if end > out.len() {
            return Ok(None);
        }
        Ok(Some(out[start_in_chunk..end].to_vec()))
    }

    /// Decompress chunk `i`, caching the most recent one.
    fn chunk(&mut self, i: usize) -> Result<&[u8], Error> {
        if self.last_chunk.as_ref().is_some_and(|(idx, _)| *idx == i) {
            return Ok(&self.last_chunk.as_ref().unwrap().1);
        }
        let (off, len) = {
            let h = self.dz.as_ref().unwrap();
            let mut off = h.data_offset;
            for &c in &h.chunks[..i] {
                off += c as usize;
            }
            (off, h.chunks[i] as usize)
        };
        let data = self.data.as_slice();
        let compressed = data
            .get(off..off + len)
            .ok_or_else(|| bad("dictzip: truncated chunk"))?;
        let mut out = Vec::new();
        DeflateDecoder::new(compressed)
            .read_to_end(&mut out)
            .map_err(|_| bad("dictzip: failed to inflate chunk"))?;
        self.last_chunk = Some((i, out));
        Ok(&self.last_chunk.as_ref().unwrap().1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a dictzip container (`chunk_length` = the length of every chunk but
    /// the last) with raw-deflated chunks.
    fn make_dictzip(chunk_length: u16, chunks: &[&[u8]]) -> Vec<u8> {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut compressed = Vec::new();
        let mut sizes = Vec::new();
        for c in chunks {
            let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
            e.write_all(c).unwrap();
            let z = e.finish().unwrap();
            sizes.push(z.len() as u16);
            compressed.extend_from_slice(&z);
        }

        // Fixed gzip header: id, method (deflate), flags (FEXTRA), mtime, xfl, os.
        let mut out = vec![0x1f, 0x8b, 0x08, FLAG_EXTRA, 0, 0, 0, 0, 0x00, 0xff];
        let ra_len = 6 + sizes.len() * 2;
        let xlen = 4 + ra_len; // SI1+SI2 (2) + LEN (2) + RA data
        out.extend_from_slice(&(xlen as u16).to_le_bytes());
        out.extend_from_slice(&RA_ID.to_le_bytes());
        out.extend_from_slice(&(ra_len as u16).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // RA version
        out.extend_from_slice(&chunk_length.to_le_bytes());
        out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
        for s in &sizes {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out.extend_from_slice(&compressed);
        out
    }

    #[test]
    fn dictzip_reads_spans_across_chunks() {
        let container = make_dictzip(8, &[b"abcdefgh", b"ijklmnop", b"qrstuvwx"]);
        let mut reader = reader(container);

        // Within the first chunk.
        assert_eq!(reader.block(2, 3).unwrap().unwrap(), b"cde");
        // Across a chunk boundary (last two of chunk 0 + first two of chunk 1).
        assert_eq!(reader.block(6, 4).unwrap().unwrap(), b"ghij");
        // Across the second boundary too.
        assert_eq!(reader.block(7, 3).unwrap().unwrap(), b"hij");
        // The whole span (all three chunks).
        assert_eq!(reader.block(0, 24).unwrap().unwrap(), b"abcdefghijklmnopqrstuvwx");
        // Out of range is a miss, not an error.
        assert!(reader.block(23, 5).unwrap().is_none());
    }

    fn reader(container: Vec<u8>) -> DictReader {
        DictReader::new_shared(Arc::new(Bytes::Owned(Arc::from(container))), true).unwrap()
    }

    #[test]
    fn truncated_dictzip_header_errors_not_panics() {
        // Empty and short buffers, and a buffer with the wrong gzip id.
        for bytes in [vec![], vec![0x1f], vec![0x1f, 0x8b, 0x08, FLAG_EXTRA]] {
            assert!(DictReader::new_shared(
                Arc::new(Bytes::Owned(Arc::from(bytes))),
                true
            )
            .is_err());
        }
        let mut wrong = make_dictzip(8, &[b"abcdefgh"]);
        wrong[0] = 0x00;
        assert!(DictReader::new_shared(
            Arc::new(Bytes::Owned(Arc::from(wrong))),
            true
        )
        .is_err());

        // A valid header whose chunk table is longer than the buffer.
        let mut truncated = make_dictzip(8, &[b"abcdefgh"]);
        truncated.truncate(14);
        assert!(DictReader::new_shared(
            Arc::new(Bytes::Owned(Arc::from(truncated))),
            true
        )
        .is_err());
    }
}
