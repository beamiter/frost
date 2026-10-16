//! Request-local bounded state. This module never accesses a display or logs data.
use std::io::{self, ErrorKind};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_REQUESTS: usize = 8;
pub const TIMEOUT: Duration = Duration::from_secs(3);
pub const WORK_BYTES: usize = 64 * 1024;
pub type Completion = Box<dyn FnOnce(io::Result<String>) + Send + 'static>;
pub type Cancelled = Box<dyn Fn() -> bool + Send + 'static>;

pub struct Request {
    pub id: u64,
    pub limit: usize,
    pub deadline: Instant,
    cancelled: Cancelled,
    complete: Option<Completion>,
    admission: Arc<AtomicUsize>,
}

impl Request {
    pub fn admit(
        id: u64,
        limit: usize,
        deadline: Instant,
        cancelled: Cancelled,
        complete: Completion,
        admission: &Arc<AtomicUsize>,
    ) -> io::Result<Self> {
        loop {
            let count = admission.load(Ordering::Acquire);
            if count >= MAX_REQUESTS {
                return Err(io::Error::new(ErrorKind::WouldBlock, "clipboard busy"));
            }
            if admission
                .compare_exchange_weak(count, count + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        Ok(Self {
            id,
            limit: limit.min(MAX_BYTES),
            deadline: deadline.min(Instant::now() + TIMEOUT),
            cancelled,
            complete: Some(complete),
            admission: admission.clone(),
        })
    }

    pub fn check(&self) -> io::Result<()> {
        if catch_unwind(AssertUnwindSafe(|| (self.cancelled)())).unwrap_or(true) {
            Err(io::Error::new(ErrorKind::Interrupted, "clipboard cancelled"))
        } else if Instant::now() >= self.deadline {
            Err(io::Error::new(ErrorKind::TimedOut, "clipboard timed out"))
        } else {
            Ok(())
        }
    }

    pub fn reject(mut self) {
        self.complete.take();
    }

    pub fn finish(mut self, result: io::Result<String>) {
        // Final success gate includes any MIME normalization after pipe EOF.
        let result = result.and_then(|text| self.check().map(|()| text));
        if let Some(callback) = self.complete.take() {
            let _ = catch_unwind(AssertUnwindSafe(|| callback(result)));
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        self.admission.fetch_sub(1, Ordering::AcqRel);
        if let Some(callback) = self.complete.take() {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                callback(Err(io::Error::new(ErrorKind::BrokenPipe, "clipboard worker stopped")))
            }));
        }
    }
}

pub enum ReadStep {
    Pending,
    Complete(String),
}

/// One fair nonblocking dispatch of the actual pipe-reader state machine.
pub fn read_available(
    reader: &mut impl io::Read,
    request: &Request,
    bytes: &mut Vec<u8>,
) -> io::Result<ReadStep> {
    let mut scratch = [0u8; 4096];
    let mut work = 0;
    loop {
        request.check()?;
        if work >= WORK_BYTES {
            return Ok(ReadStep::Pending);
        }
        let remaining = request.limit.checked_sub(bytes.len()).ok_or_else(too_large)?;
        let amount = remaining.clamp(1, scratch.len());
        match reader.read(&mut scratch[..amount]) {
            Ok(0) => {
                let text = decode_bounded(std::mem::take(bytes), request.limit)?;
                request.check()?;
                return Ok(ReadStep::Complete(text));
            },
            Ok(n) => {
                if n > remaining {
                    return Err(too_large());
                }
                bytes.extend_from_slice(&scratch[..n]);
                work += n;
            },
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(ReadStep::Pending),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Err(io::Error::other("selection pipe failed")),
        }
    }
}

pub fn decode_bounded(bytes: Vec<u8>, limit: usize) -> io::Result<String> {
    // Count the exact lossy UTF-8 expansion before allocating a String. UTF-8
    // replacement can triple an invalid byte stream, despite a bounded wire read.
    let mut rest = bytes.as_slice();
    let mut length = 0usize;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(_) => {
                length = length.saturating_add(rest.len());
                break;
            },
            Err(error) => {
                length = length.saturating_add(error.valid_up_to()).saturating_add(3);
                rest = &rest[error.valid_up_to()
                    + error.error_len().unwrap_or(rest.len() - error.valid_up_to())..];
            },
        }
        if length > limit {
            return Err(too_large());
        }
    }
    if length > limit {
        return Err(too_large());
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) => {
            let mut rest = error.as_bytes();
            let mut text = String::with_capacity(length);
            while !rest.is_empty() {
                match std::str::from_utf8(rest) {
                    Ok(valid) => {
                        text.push_str(valid);
                        break;
                    },
                    Err(invalid) => {
                        text.push_str(
                            std::str::from_utf8(&rest[..invalid.valid_up_to()])
                                .expect("validated UTF-8 prefix"),
                        );
                        text.push('\u{fffd}');
                        rest = &rest[invalid.valid_up_to()
                            + invalid.error_len().unwrap_or(rest.len() - invalid.valid_up_to())..];
                    },
                }
            }
            Ok(text)
        },
    }
}

/// Normalize in place, so CR/CRLF conversion cannot trigger String growth.
pub fn normalize_lf(text: String) -> String {
    let mut bytes = text.into_bytes();
    let mut read = 0;
    let mut write = 0;
    while read < bytes.len() {
        if bytes[read] == b'\r' {
            bytes[write] = b'\n';
            read += 1;
            if bytes.get(read) == Some(&b'\n') {
                read += 1;
            }
        } else {
            bytes[write] = bytes[read];
            read += 1;
        }
        write += 1;
    }
    bytes.truncate(write);
    String::from_utf8(bytes).expect("ASCII newline normalization preserves UTF-8")
}

pub fn too_large() -> io::Error {
    io::Error::new(ErrorKind::FileTooLarge, "clipboard exceeds byte limit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn eight_including_queued_and_release_after_panic() {
        let n = Arc::new(AtomicUsize::new(0));
        let mut requests = Vec::new();
        for id in 0..8 {
            requests.push(
                Request::admit(
                    id,
                    MAX_BYTES + 1,
                    Instant::now() + TIMEOUT * 2,
                    Box::new(|| false),
                    Box::new(|_| {}),
                    &n,
                )
                .unwrap(),
            );
        }
        assert!(
            Request::admit(
                9,
                1,
                Instant::now() + TIMEOUT,
                Box::new(|| false),
                Box::new(|_| {}),
                &n
            )
            .is_err()
        );
        assert_eq!(requests[0].limit, MAX_BYTES);
        drop(requests);
        assert_eq!(n.load(Ordering::Acquire), 0);
        Request::admit(
            10,
            1,
            Instant::now(),
            Box::new(|| false),
            Box::new(|_| panic!("synthetic callback")),
            &n,
        )
        .unwrap()
        .finish(Ok(String::new()));
        assert_eq!(n.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cancellation_and_queued_expiry_are_request_local() {
        let n = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let request = Request::admit(
            1,
            10,
            Instant::now() + TIMEOUT,
            Box::new(move || flag.load(Ordering::Acquire)),
            Box::new(|_| {}),
            &n,
        )
        .unwrap();
        assert!(request.check().is_ok());
        cancel.store(true, Ordering::Release);
        assert_eq!(request.check().unwrap_err().kind(), ErrorKind::Interrupted);
        let expired =
            Request::admit(2, 10, Instant::now(), Box::new(|| false), Box::new(|_| {}), &n)
                .unwrap();
        assert_eq!(expired.check().unwrap_err().kind(), ErrorKind::TimedOut);
    }

    #[test]
    fn decoded_cap_precedes_string_allocation_and_preserves_lossy_behavior() {
        assert_eq!(decode_bounded(vec![b'a'; MAX_BYTES], MAX_BYTES).unwrap().len(), MAX_BYTES);
        assert!(decode_bounded(vec![b'a'; MAX_BYTES + 1], MAX_BYTES).is_err());
        assert!(decode_bounded(vec![0xff; MAX_BYTES], MAX_BYTES).is_err());
        for bytes in [vec![0xff], vec![0xe2, 0x82], vec![b'a', 0xff, b'b'], vec![0xed, 0xa0, 0x80]]
        {
            assert_eq!(
                decode_bounded(bytes.clone(), 100).unwrap(),
                String::from_utf8_lossy(&bytes)
            );
        }
        assert_eq!(decode_bounded(vec![0xff], 3).unwrap(), "\u{fffd}");
        assert!(decode_bounded(vec![0xff], 2).is_err());
    }
    fn request(limit: usize) -> Request {
        Request::admit(
            1,
            limit,
            Instant::now() + TIMEOUT,
            Box::new(|| false),
            Box::new(|_| {}),
            &Arc::new(AtomicUsize::new(0)),
        )
        .unwrap()
    }

    #[test]
    fn exact_cap_eof_probe_and_cap_plus_one_reject_without_append() {
        for length in [0, 1, 4096, MAX_BYTES] {
            let mut input = io::Cursor::new(vec![b'a'; length]);
            let request = request(length);
            let mut bytes = Vec::with_capacity(length);
            loop {
                match read_available(&mut input, &request, &mut bytes).unwrap() {
                    ReadStep::Pending => {},
                    ReadStep::Complete(text) => {
                        assert_eq!(text.len(), length);
                        break;
                    },
                }
            }
        }
        let request = request(4096);
        let mut bytes = Vec::with_capacity(request.limit);
        let mut input = io::Cursor::new(vec![b'a'; request.limit + 1]);
        assert_eq!(
            read_available(&mut input, &request, &mut bytes).err().unwrap().kind(),
            ErrorKind::FileTooLarge
        );
        assert_eq!(bytes.len(), request.limit);
    }

    #[test]
    fn continuous_producer_yields_and_cancel_before_read_does_not_touch_source() {
        struct Endless(usize);
        impl io::Read for Endless {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.0 += bytes.len();
                bytes.fill(b'x');
                Ok(bytes.len())
            }
        }
        let mut input = Endless(0);
        let request = request(MAX_BYTES);
        let mut bytes = Vec::with_capacity(request.limit);
        assert!(matches!(
            read_available(&mut input, &request, &mut bytes).unwrap(),
            ReadStep::Pending
        ));
        assert_eq!(input.0, WORK_BYTES);
        let cancelled = Request::admit(
            2,
            10,
            Instant::now() + TIMEOUT,
            Box::new(|| true),
            Box::new(|_| {}),
            &Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();
        assert_eq!(
            read_available(&mut input, &cancelled, &mut Vec::new()).err().unwrap().kind(),
            ErrorKind::Interrupted
        );
        assert_eq!(input.0, WORK_BYTES);
    }

    #[test]
    fn stalled_pipe_returns_pending_and_deadline_is_not_reset() {
        struct Stalled;
        impl io::Read for Stalled {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(ErrorKind::WouldBlock))
            }
        }
        let mut request = request(20);
        assert!(matches!(
            read_available(&mut Stalled, &request, &mut Vec::new()).unwrap(),
            ReadStep::Pending
        ));
        request.deadline = Instant::now();
        assert_eq!(
            read_available(&mut Stalled, &request, &mut Vec::new()).err().unwrap().kind(),
            ErrorKind::TimedOut
        );
    }

    #[test]
    fn late_invalid_byte_does_not_geometrically_grow_result() {
        let mut bytes = vec![b'a'; MAX_BYTES - 3];
        bytes.push(0xff);
        let result = decode_bounded(bytes, MAX_BYTES).unwrap();
        assert_eq!(result.len(), MAX_BYTES);
        assert_eq!(result.capacity(), MAX_BYTES);
        assert_eq!(normalize_lf("a\r\nb\rc\n\u{fffd}".into()), "a\nb\nc\n\u{fffd}");
    }
    #[test]
    fn completion_rechecks_cancellation_and_deadline_after_normalization() {
        for expired in [false, true] {
            let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = cancelled.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let mut request = Request::admit(
                1,
                20,
                Instant::now() + TIMEOUT,
                Box::new(move || flag.load(Ordering::Acquire)),
                Box::new(move |result| {
                    tx.send(result).unwrap();
                }),
                &Arc::new(AtomicUsize::new(0)),
            )
            .unwrap();
            let text = normalize_lf("synthetic\r\n".into());
            if expired {
                request.deadline = Instant::now();
            } else {
                cancelled.store(true, Ordering::Release);
            }
            request.finish(Ok(text));
            assert_eq!(
                rx.recv().unwrap().unwrap_err().kind(),
                if expired { ErrorKind::TimedOut } else { ErrorKind::Interrupted }
            );
        }
    }
}
