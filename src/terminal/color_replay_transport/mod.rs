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
// Immutable original-production seal, independently reviewed before candidate replay.
pub const COMPRESSED_BYTES: u64 = 3_480_287;
pub const COMPRESSED_SHA: &str = "7952986f56d95e465cd110d849cb731e2e43bd7217faab521fdf1f177f530a62";
pub const RAW_BYTES: u64 = 3_342_082_180;
pub const RAW_SHA: &str = "3873ad6432c022f10bf1fecabbe9b360da902d32e481c226325dbe512752db0e";
pub const RECORDS: usize = 44_720;
pub const MAX_RECORD: u64 = 85_791;
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
#[derive(Clone, Copy)]
struct Limits {
    bytes: u64,
    records: usize,
    max_record: u64,
    sha: &'static str,
}
const SEALED_LIMITS: Limits = Limits {
    bytes: RAW_BYTES,
    records: RECORDS,
    max_record: MAX_RECORD,
    sha: RAW_SHA,
};
struct Records<R: BufRead> {
    limits: Limits,
    reader: R,
    bytes: u64,
    count: usize,
    digest: Sha256,
}
impl<R: BufRead> Records<R> {
    fn new(reader: R) -> Self {
        Self::with_limits(reader, SEALED_LIMITS)
    }
    fn with_limits(reader: R, limits: Limits) -> Self {
        Self {
            limits,
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
            .take(self.limits.max_record + 1)
            .read_until(b'\n', &mut record)?;
        if record.is_empty() {
            return Ok(None);
        }
        self.bytes = self
            .bytes
            .checked_add(record.len() as u64)
            .ok_or_else(|| invalid("size overflow"))?;
        self.count += 1;
        if record.len() as u64 > self.limits.max_record
            || record.last() != Some(&b'\n')
            || self.bytes > self.limits.bytes
            || self.count > self.limits.records
        {
            return Err(invalid("record/total/count bound or missing LF"));
        }
        self.digest.update(&record);
        Ok(Some(record))
    }
    fn checked_finish(self) -> io::Result<R> {
        if self.bytes != self.limits.bytes
            || self.count != self.limits.records
            || format!("{:x}", self.digest.finalize()) != self.limits.sha
        {
            return Err(invalid("canonical SHA/length/count mismatch"));
        }
        Ok(self.reader)
    }
}
/// Stream exact canonical bytes, additionally validating per-record schema/keys.
/// At most two bounded JSON records and one small compressed fixture are held.
pub fn compare(fixture: &Path, actual: &Path) -> io::Result<()> {
    if COMPRESSED_BYTES == 0 {
        return Err(invalid("unsealed color fixture"));
    }
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
// Actual output has hard ceilings and can never replace the immutable fixture.
struct CaptureBound<W> {
    inner: W,
    total: u64,
    line: u64,
    records: usize,
}
impl<W: Write> Write for CaptureBound<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let total = self
            .total
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("capture total overflow"))?;
        if total > 16 * 1024 * 1024 * 1024 {
            return Err(invalid("capture total ceiling"));
        }
        let mut line = self.line;
        let mut records = self.records;
        for byte in bytes {
            line += 1;
            if line > 4 * 1024 * 1024 {
                return Err(invalid("capture record ceiling"));
            }
            if *byte == b'\n' {
                records += 1;
                line = 0;
                if records > 44_720 {
                    return Err(invalid("capture record count ceiling"));
                }
            }
        }
        self.inner.write_all(bytes)?;
        self.total = total;
        self.line = line;
        self.records = records;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
pub fn write_actual(
    output: File,
    emit: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let mut encoder = zstd::stream::write::Encoder::new(io::BufWriter::new(output), 9)?;
    encoder.window_log(23)?;
    encoder.include_checksum(true)?;
    let mut bounded = CaptureBound {
        inner: &mut encoder,
        total: 0,
        line: 0,
        records: 0,
    };
    emit(&mut bounded)?;
    if bounded.line != 0 || bounded.records != 44_720 {
        return Err(invalid("capture incomplete record/count"));
    }
    bounded.flush()?;
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
        let limits = Limits {
            bytes: 2,
            records: 1,
            max_record: 2,
            sha: "73cb3858a687a8494ca3323053016282f3dad39d42cf62ca4e79dda2aac7d9ac",
        };
        assert!(Records::with_limits(&b"xxx"[..], limits).next().is_err());
        assert!(Records::with_limits(&b"x"[..], limits).next().is_err());
        let mut r = Records::with_limits(&b"x\n"[..], limits);
        r.bytes = 2;
        assert!(r.next().is_err());
        let mut r = Records::with_limits(&b"x\n"[..], limits);
        r.count = 1;
        assert!(r.next().is_err());
        let mut r = Records::with_limits(&b"x\n"[..], limits);
        assert_eq!(r.next().unwrap().unwrap(), b"x\n");
        assert!(r.next().unwrap().is_none());
        r.checked_finish().unwrap();
        assert!(Records::new(&b"x\n"[..]).checked_finish().is_err());
        assert!(compare(Path::new("absent"), Path::new("absent")).is_err());
    }
}

pub mod worker;
