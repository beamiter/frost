use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
};
fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}
pub const COMPRESSED_BYTES: u64 = 3_501_727;
pub const COMPRESSED_SHA: &str = "8dd591c50ade8546d6d438e1c08837ceaf59c2d2e9462bd9209e75987051887b";
pub const RAW_BYTES: u64 = 3_775_893_628;
pub const RAW_SHA: &str = "1f67b934245c3a2d305e65b9527027e6e55b79af5c6b1417ab4ebfc5f2d3af08";
pub const RECORDS: usize = 53_992;
pub const MAX_RECORD: u64 = 92_682;
pub const ACTUAL_COMPRESSED_LIMIT: u64 = 64 * 1024 * 1024;
include!("lanes.rs");
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn bounded_fixture(path: &Path) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() != COMPRESSED_BYTES {
        return Err(invalid("fixture compressed length"));
    }
    let mut data = Vec::with_capacity(COMPRESSED_BYTES as usize);
    file.take(COMPRESSED_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 != COMPRESSED_BYTES || hash(&data) != COMPRESSED_SHA {
        return Err(invalid("fixture compressed SHA/length"));
    }
    Ok(data)
}
struct Records<R: BufRead> {
    reader: R,
    bytes: u64,
    count: usize,
    digest: Sha256,
}
impl<R: BufRead> Records<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            bytes: 0,
            count: 0,
            digest: Sha256::new(),
        }
    }
    fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut record = Vec::new();
        // Bound allocation even if an adversarial stream omits LF entirely.
        self.reader
            .by_ref()
            .take(MAX_RECORD + 1)
            .read_until(b'\n', &mut record)?;
        if record.is_empty() {
            return Ok(None);
        }
        self.bytes = self
            .bytes
            .checked_add(record.len() as u64)
            .ok_or_else(|| invalid("size overflow"))?;
        self.count += 1;
        if record.len() as u64 > MAX_RECORD
            || record.last() != Some(&b'\n')
            || self.bytes > RAW_BYTES
            || self.count > RECORDS
        {
            return Err(invalid("record/total/count bound or missing LF"));
        }
        self.digest.update(&record);
        Ok(Some(record))
    }
    fn checked_finish(self) -> io::Result<R> {
        if self.bytes != RAW_BYTES
            || self.count != RECORDS
            || format!("{:x}", self.digest.finalize()) != RAW_SHA
        {
            return Err(invalid("canonical SHA/length/count mismatch"));
        }
        Ok(self.reader)
    }
}
/// Stream exact canonical bytes, additionally validating per-record schema/keys.
/// At most two bounded JSON records and one small compressed fixture are held.
pub fn compare(fixture: &Path, actual: &Path) -> io::Result<()> {
    let expected = bounded_fixture(fixture)?;
    let actual_file = File::open(actual)?;
    if actual_file.metadata()?.len() > ACTUAL_COMPRESSED_LIMIT {
        return Err(invalid("actual compressed bound"));
    }
    let mut e = zstd::stream::read::Decoder::with_buffer(expected.as_slice())?.single_frame();
    let mut a = zstd::stream::read::Decoder::with_buffer(BufReader::new(
        actual_file.take(ACTUAL_COMPRESSED_LIMIT + 1),
    ))?
    .single_frame();
    e.window_log_max(23)?;
    a.window_log_max(23)?;
    let mut e = Records::new(BufReader::new(e));
    let mut a = Records::new(BufReader::new(a));
    let mut seen = BTreeSet::new();
    loop {
        match (e.next()?, a.next()?) {
            (None, None) => break,
            (Some(want), Some(got)) => {
                // Exact equality also rejects duplicate keys, reorderings and any
                // alternate serialization in actual; the expected stream is SHA sealed.
                if want != got {
                    return Err(invalid(format!(
                        "record {} exact full-state mismatch",
                        e.count
                    )));
                }
                let row: serde_json::Value =
                    serde_json::from_slice(&want).map_err(|x| invalid(x.to_string()))?;
                let row = row
                    .as_array()
                    .filter(|x| x.len() == 2)
                    .ok_or_else(|| invalid("record shape"))?;
                let id = row[0].as_str().ok_or_else(|| invalid("record ID"))?;
                if !seen.insert(id.to_owned()) {
                    return Err(invalid("duplicate ID"));
                }
                let lanes = row[1]
                    .as_object()
                    .ok_or_else(|| invalid("snapshot shape"))?;
                if !lanes.keys().map(String::as_str).eq(LANES.iter().copied()) {
                    return Err(invalid("lane keys"));
                }
            }
            _ => return Err(invalid("record count/order mismatch")),
        }
    }
    let e = e.checked_finish()?.into_inner();
    let a = a.checked_finish()?.into_inner();
    if !e.finish().fill_buf()?.is_empty() || !a.finish().fill_buf()?.is_empty() {
        return Err(invalid("trailing bytes or extra zstd frame"));
    }
    Ok(())
}
/// Replace only the original worker's external gzip subprocess with this writer.
/// JSONL emit_actual is unchanged; no production source guard belongs in replay.
pub fn write_actual(
    output: File,
    emit: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let mut encoder = zstd::stream::write::Encoder::new(io::BufWriter::new(output), 9)?;
    encoder.window_log(23)?;
    encoder.include_checksum(true)?;
    emit(&mut encoder)?;
    let mut output = encoder.finish()?;
    output.flush()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn encode(raw: &[u8], window: u32) -> Vec<u8> {
        let mut e = zstd::stream::write::Encoder::new(Vec::new(), 9).unwrap();
        e.window_log(window).unwrap();
        e.include_checksum(true).unwrap();
        e.write_all(raw).unwrap();
        e.finish().unwrap()
    }
    fn decode_small(bytes: &[u8]) -> io::Result<()> {
        let mut d = zstd::stream::read::Decoder::with_buffer(bytes)?.single_frame();
        d.window_log_max(23)?;
        let mut buf = [0; 16];
        while d.read(&mut buf)? != 0 {}
        if !d.finish().fill_buf()?.is_empty() {
            return Err(invalid("trailing"));
        }
        Ok(())
    }
    #[test]
    fn rejects_every_truncation_tail_frame_and_window() {
        let b = encode(b"one\n", 23);
        decode_small(&b).unwrap();
        for n in 0..b.len() {
            assert!(decode_small(&b[..n]).is_err());
        }
        for extra in [vec![0], encode(b"", 23), encode(b"two\n", 23)] {
            let mut v = b.clone();
            v.extend(extra);
            assert!(decode_small(&v).is_err());
        }
        assert!(decode_small(&encode(b"one\n", 24)).is_err());
    }
    #[test]
    fn records_limit_allocations_missing_lf_and_totals() {
        let huge = vec![b'x'; MAX_RECORD as usize + 1];
        assert!(Records::new(huge.as_slice()).next().is_err());
        assert!(Records::new(&b"x"[..]).next().is_err());
        let mut r = Records::new(&b"x\n"[..]);
        r.bytes = RAW_BYTES;
        assert!(r.next().is_err());
        let mut r = Records::new(&b"x\n"[..]);
        r.count = RECORDS;
        assert!(r.next().is_err());
        let mut r = Records::new(&b"x\n"[..]);
        assert_eq!(r.next().unwrap().unwrap(), b"x\n");
        assert!(r.next().unwrap().is_none());
        assert!(r.checked_finish().is_err());
    }
}

pub mod worker;
