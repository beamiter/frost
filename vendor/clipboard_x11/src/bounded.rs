//! Opt-in owned reader. No server connection is created after initial setup.
#![forbid(unsafe_code)]
use crate::{Atoms, Error};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use std::io::{self, IoSlice};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::errors::ConnectError;
use x11rb::protocol::{
    xproto::{self, AtomEnum, Window},
    Event,
};
use x11rb::rust_connection::{DefaultStream, PollMode, RustConnection, Stream};
use x11rb::utils::RawFdContainer;

pub(crate) const MAX_BYTES: usize = 1024 * 1024;
const MAX_REQUESTS: usize = 8;
const MAX_TIME: Duration = Duration::from_secs(3);
const TICK: Duration = Duration::from_millis(10);
type Cancelled = Box<dyn Fn() -> bool + Send + 'static>;
type Complete = Box<dyn FnOnce(io::Result<String>) + Send + 'static>;
fn failure(kind: io::ErrorKind) -> io::Error {
    io::Error::new(kind, "clipboard read unavailable")
}
fn protocol_error(_: impl std::fmt::Display) -> io::Error {
    failure(io::ErrorKind::InvalidData)
}

struct Control {
    deadline: Instant,
    cancelled: Mutex<Option<Cancelled>>,
}
impl Control {
    fn check(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(failure(io::ErrorKind::TimedOut));
        }
        // Never invoke caller code while holding a callback lock. Reentrant
        // checks fail closed while the callback is temporarily taken.
        let callback = self.cancelled.lock().ok().and_then(|mut slot| slot.take());
        let cancelled = match callback {
            Some(callback) => {
                let result = catch_unwind(AssertUnwindSafe(|| callback())).unwrap_or(true);
                if let Ok(mut slot) = self.cancelled.lock() {
                    *slot = Some(callback);
                }
                result
            }
            None => true,
        };
        if cancelled {
            Err(failure(io::ErrorKind::Other))
        } else {
            Ok(())
        }
    }
}
struct Shared {
    stop: AtomicBool,
    closed: AtomicBool,
    admitted: AtomicUsize,
}
struct Permit(Arc<Shared>);
impl Permit {
    fn acquire(shared: &Arc<Shared>) -> io::Result<Self> {
        if shared.closed.load(Ordering::Acquire) {
            return Err(failure(io::ErrorKind::BrokenPipe));
        }
        loop {
            let count = shared.admitted.load(Ordering::Acquire);
            if count >= MAX_REQUESTS {
                return Err(failure(io::ErrorKind::WouldBlock));
            }
            if shared
                .admitted
                .compare_exchange_weak(count, count + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        Ok(Self(shared.clone()))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.admitted.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Request {
    primary: bool,
    cap: usize,
    control: Arc<Control>,
    complete: Complete,
    _permit: Permit,
}

pub(crate) struct Reader {
    tx: Option<mpsc::SyncSender<Request>>,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl Reader {
    pub(crate) fn start(endpoint: Endpoint) -> io::Result<Self> {
        let shared = endpoint.shared.clone();
        let (tx, rx) = mpsc::sync_channel::<Request>(MAX_REQUESTS);
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("clipboard-x11-read".into())
            .spawn(move || {
                let mut endpoint = Some(endpoint);
                while let Ok(request) = rx.recv() {
                    let result = if worker_shared.closed.load(Ordering::Acquire)
                        || worker_shared.stop.load(Ordering::Acquire)
                    {
                        Err(failure(io::ErrorKind::BrokenPipe))
                    } else {
                        match catch_unwind(AssertUnwindSafe(|| {
                            endpoint.as_ref().expect("live endpoint").read(&request)
                        })) {
                            Ok(result) => result,
                            Err(_) => {
                                worker_shared.closed.store(true, Ordering::Release);
                                Err(failure(io::ErrorKind::Other))
                            }
                        }
                    };
                    if worker_shared.closed.load(Ordering::Acquire)
                        || worker_shared.stop.load(Ordering::Acquire)
                    {
                        endpoint.take();
                    }
                    // Permit lives through completion, including a panicking callback.
                    let _ = catch_unwind(AssertUnwindSafe(|| (request.complete)(result)));
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            shared,
            worker: Some(worker),
        })
    }
    pub(crate) fn submit(
        &self,
        primary: bool,
        cap: usize,
        deadline: Instant,
        cancelled: Cancelled,
        complete: Complete,
    ) -> io::Result<()> {
        let deadline = deadline.min(Instant::now() + MAX_TIME);
        let permit = Permit::acquire(&self.shared)?;
        let request = Request {
            primary,
            cap: cap.min(MAX_BYTES),
            control: Arc::new(Control {
                deadline,
                cancelled: Mutex::new(Some(cancelled)),
            }),
            complete,
            _permit: permit,
        };
        self.tx
            .as_ref()
            .ok_or_else(|| failure(io::ErrorKind::BrokenPipe))?
            .try_send(request)
            .map_err(|err| match err {
                mpsc::TrySendError::Full(_) => failure(io::ErrorKind::WouldBlock),
                mpsc::TrySendError::Disconnected(_) => failure(io::ErrorKind::BrokenPipe),
            })
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct EventPollGuard<'a>(&'a AtomicBool);
impl Drop for EventPollGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct DeadlineStream {
    inner: DefaultStream,
    shared: Arc<Shared>,
    active: Mutex<Option<Arc<Control>>>,
    event_poll: AtomicBool,
}
impl DeadlineStream {
    fn check(&self) -> io::Result<()> {
        let result = if self.shared.stop.load(Ordering::Acquire) {
            Err(failure(io::ErrorKind::BrokenPipe))
        } else {
            let active = self
                .active
                .lock()
                .map_err(|_| failure(io::ErrorKind::Other))?
                .clone();
            active.map_or(Ok(()), |control| control.check())
        };
        if result.is_err() {
            self.shared.closed.store(true, Ordering::Release);
        }
        result
    }
    fn record<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if let Err(ref error) = result {
            if error.kind() != io::ErrorKind::WouldBlock {
                self.shared.closed.store(true, Ordering::Release);
            }
        }
        result
    }
}
impl Stream for DeadlineStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        loop {
            self.check()?;
            let mut flags = PollFlags::empty();
            if mode.readable() {
                flags |= PollFlags::IN;
            }
            if mode.writable() {
                flags |= PollFlags::OUT;
            }
            let mut fds = [PollFd::new(&self.inner, flags)];
            let remaining = self
                .active
                .lock()
                .map_err(protocol_error)?
                .as_ref()
                .map(|control| control.deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(TICK)
                .min(TICK);
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: remaining.as_nanos() as _,
            };
            match poll(&mut fds, Some(&timeout)) {
                Ok(_) => {
                    self.check()?;
                    return Ok(());
                }
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return self.record(Err(error.into())),
            }
        }
    }
    fn read(&self, buf: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        // x11rb's nonblocking event poll preserves partial packets itself.
        // Do not inject an error that would poison that healthy connection.
        // The caller checks cancellation immediately around this operation.
        if !self.event_poll.load(Ordering::Acquire) {
            self.check()?;
        }
        self.record(self.inner.read(buf, fds))
    }
    fn write(&self, buf: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.record(self.inner.write(buf, fds))
    }
    fn write_vectored(
        &self,
        bufs: &[IoSlice<'_>],
        fds: &mut Vec<RawFdContainer>,
    ) -> io::Result<usize> {
        self.check()?;
        self.record(self.inner.write_vectored(bufs, fds))
    }
}

pub(crate) struct Endpoint {
    connection: RustConnection<DeadlineStream>,
    screen: usize,
    atoms: Atoms,
    shared: Arc<Shared>,
}
impl Endpoint {
    pub(crate) fn connect() -> Result<Self, Error> {
        Self::connect_display(None)
    }

    fn connect_display(display_name: Option<&str>) -> Result<Self, Error> {
        // Same address ordering and xauth fallback as RustConnection::connect.
        let display = x11rb_protocol::parse_display::parse_display(display_name)
            .map_err(ConnectError::from)?;
        let screen = usize::from(display.screen);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            admitted: AtomicUsize::new(0),
        });
        let mut last = None;
        for addr in display.connect_instruction() {
            match DefaultStream::connect(&addr) {
                Ok((inner, (family, address))) => {
                    let (name, data) =
                        x11rb_protocol::xauth::get_auth(family, &address, display.display)
                            .unwrap_or(None)
                            .unwrap_or_else(|| (Vec::new(), Vec::new()));
                    let stream = DeadlineStream {
                        inner,
                        shared: shared.clone(),
                        active: Mutex::new(None),
                        event_poll: AtomicBool::new(false),
                    };
                    let connection = RustConnection::connect_to_stream_with_auth_info(
                        stream, screen, name, data,
                    )?;
                    let atom = |name: &[u8]| -> Result<u32, Error> {
                        Ok(xproto::intern_atom(&connection, false, name)?.reply()?.atom)
                    };
                    let atoms = Atoms {
                        primary: AtomEnum::PRIMARY.into(),
                        clipboard: atom(b"CLIPBOARD")?,
                        property: atom(b"THIS_CLIPBOARD_OUT")?,
                        targets: atom(b"TARGETS")?,
                        string: AtomEnum::STRING.into(),
                        utf8_string: atom(b"UTF8_STRING")?,
                        incr: atom(b"INCR")?,
                    };
                    return Ok(Self {
                        connection,
                        screen,
                        atoms,
                        shared,
                    });
                }
                Err(error) => last = Some(error),
            }
        }
        Err(Error::ConnectionFailed(match last {
            Some(error) => ConnectError::IoError(error),
            None => x11rb_protocol::errors::DisplayParsingError::Unknown.into(),
        }))
    }

    fn transport_error(&self, _: impl std::fmt::Display) -> io::Error {
        self.shared.closed.store(true, Ordering::Release);
        failure(io::ErrorKind::BrokenPipe)
    }

    fn read(&self, request: &Request) -> io::Result<String> {
        request.control.check()?;
        *self
            .connection
            .stream()
            .active
            .lock()
            .map_err(|e| self.transport_error(e))? = Some(request.control.clone());
        let window = self
            .connection
            .generate_id()
            .map_err(|e| self.transport_error(e))?;
        let root = &self.connection.setup().roots[self.screen];
        let result = (|| {
            xproto::create_window(
                &self.connection,
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root.root,
                0,
                0,
                1,
                1,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                root.root_visual,
                &xproto::CreateWindowAux::new().event_mask(xproto::EventMask::PROPERTY_CHANGE),
            )
            .map_err(|e| self.transport_error(e))?;
            let selection = if request.primary {
                self.atoms.primary
            } else {
                self.atoms.clipboard
            };
            let owner = self.selection_owner(selection)?;
            xproto::convert_selection(
                &self.connection,
                window,
                selection,
                self.atoms.utf8_string,
                self.atoms.property,
                x11rb::CURRENT_TIME,
            )
            .map_err(|e| self.transport_error(e))?;
            self.connection
                .flush()
                .map_err(|e| self.transport_error(e))?;
            let result = self.transfer(request, window, selection)?;
            request.control.check()?;
            if self.selection_owner(selection)? != owner {
                return Err(failure(io::ErrorKind::Other));
            }
            request.control.check()?;
            Ok(result)
        })();
        // Ordinary owner timeout is detected between nonblocking event polls,
        // not injected into the stream. Retire its window, without acknowledging
        // any pending INCR chunk. Cleanup itself has a finite transport deadline.
        *self
            .connection
            .stream()
            .active
            .lock()
            .map_err(|e| self.transport_error(e))? = Some(Arc::new(Control {
            deadline: Instant::now() + Duration::from_millis(100),
            cancelled: Mutex::new(Some(Box::new(|| false))),
        }));
        if !self.shared.closed.load(Ordering::Acquire) && !self.shared.stop.load(Ordering::Acquire)
        {
            let cleanup = xproto::destroy_window(&self.connection, window)
                .map_err(|e| self.transport_error(e))
                .and_then(|_| self.connection.flush().map_err(|e| self.transport_error(e)));
            if cleanup.is_err() {
                self.shared.closed.store(true, Ordering::Release);
            }
        }
        *self
            .connection
            .stream()
            .active
            .lock()
            .map_err(|e| self.transport_error(e))? = None;
        result
    }

    fn selection_owner(&self, selection: u32) -> io::Result<Window> {
        Ok(xproto::get_selection_owner(&self.connection, selection)
            .map_err(|e| self.transport_error(e))?
            .reply()
            .map_err(|e| self.transport_error(e))?
            .owner)
    }

    fn transfer(&self, request: &Request, window: Window, selection: u32) -> io::Result<String> {
        let mut incr = false;
        let mut bytes = Vec::new();
        loop {
            request.control.check()?;
            if self.shared.stop.load(Ordering::Acquire) {
                return Err(failure(io::ErrorKind::BrokenPipe));
            }
            self.connection
                .stream()
                .event_poll
                .store(true, Ordering::Release);
            let event = {
                let _guard = EventPollGuard(&self.connection.stream().event_poll);
                self.connection.poll_for_event()
            };
            let event = event.map_err(|e| self.transport_error(e))?;
            request.control.check()?;
            if self.shared.stop.load(Ordering::Acquire) {
                return Err(failure(io::ErrorKind::BrokenPipe));
            }
            let event = match event {
                Some(event) => event,
                None => {
                    thread::park_timeout(
                        TICK.min(
                            request
                                .control
                                .deadline
                                .saturating_duration_since(Instant::now()),
                        ),
                    );
                    continue;
                }
            };
            match event {
                Event::SelectionNotify(event)
                    if !incr
                        && selection_identity(
                            &event,
                            window,
                            selection,
                            self.atoms.utf8_string,
                            self.atoms.property,
                        ) =>
                {
                    if event.property == AtomEnum::NONE.into() {
                        return Ok(String::new());
                    }
                    if event.property != self.atoms.property {
                        continue;
                    }
                    let reply = self.property(window, request.cap, true)?;
                    if reply.type_ == self.atoms.incr {
                        validate_incr(&reply, request.cap)?;
                        incr = true;
                        self.ack(window)?;
                    } else {
                        validate_text(&reply, self.atoms.utf8_string, request.cap)?;
                        request.control.check()?;
                        return String::from_utf8(reply.value).map_err(protocol_error);
                    }
                }
                Event::PropertyNotify(event)
                    if incr
                        && event.window == window
                        && event.atom == self.atoms.property
                        && event.state == xproto::Property::NEW_VALUE =>
                {
                    let reply = self.property(window, request.cap - bytes.len(), false)?;
                    validate_text(&reply, self.atoms.utf8_string, request.cap - bytes.len())?;
                    if reply.value.is_empty() {
                        request.control.check()?;
                        return String::from_utf8(bytes).map_err(protocol_error);
                    }
                    request.control.check()?;
                    bytes.reserve_exact(reply.value.len());
                    bytes.extend_from_slice(&reply.value);
                    self.ack(window)?;
                }
                _ => {}
            }
        }
    }
    fn ack(&self, window: Window) -> io::Result<()> {
        xproto::delete_property(&self.connection, window, self.atoms.property)
            .map_err(|e| self.transport_error(e))?;
        self.connection.flush().map_err(|e| self.transport_error(e))
    }
    fn property(
        &self,
        window: Window,
        remaining: usize,
        initial: bool,
    ) -> io::Result<xproto::GetPropertyReply> {
        let probe = xproto::get_property(
            &self.connection,
            false,
            window,
            self.atoms.property,
            AtomEnum::ANY,
            0,
            0,
        )
        .map_err(|e| self.transport_error(e))?
        .reply()
        .map_err(|e| self.transport_error(e))?;
        // INCR has exactly one u32 size hint, including when the content cap is zero.
        let limit = if initial && probe.type_ == self.atoms.incr {
            4
        } else {
            remaining
        };
        let units = property_units(probe.bytes_after, limit)?;
        let reply = xproto::get_property(
            &self.connection,
            false,
            window,
            self.atoms.property,
            AtomEnum::ANY,
            0,
            units,
        )
        .map_err(|e| self.transport_error(e))?
        .reply()
        .map_err(|e| self.transport_error(e))?;
        if reply.type_ != probe.type_
            || reply.format != probe.format
            || reply.bytes_after != 0
            || reply.value.len() > limit
            || reply.value.len() != probe.bytes_after as usize
        {
            return Err(failure(io::ErrorKind::InvalidData));
        }
        Ok(reply)
    }
}
fn selection_identity(
    event: &xproto::SelectionNotifyEvent,
    window: Window,
    selection: u32,
    target: u32,
    property: u32,
) -> bool {
    event.requestor == window
        && event.selection == selection
        && event.target == target
        && (event.property == property || event.property == u32::from(AtomEnum::NONE))
}
fn property_units(bytes: u32, remaining: usize) -> io::Result<u32> {
    if bytes as usize > remaining {
        return Err(failure(io::ErrorKind::InvalidData));
    }
    Ok(bytes / 4 + u32::from(bytes % 4 != 0))
}
fn validate_text(
    reply: &xproto::GetPropertyReply,
    target: u32,
    remaining: usize,
) -> io::Result<()> {
    if reply.type_ != target
        || reply.format != 8
        || reply.bytes_after != 0
        || reply.value.len() > remaining
        || reply.value_len as usize != reply.value.len()
    {
        Err(failure(io::ErrorKind::InvalidData))
    } else {
        Ok(())
    }
}
fn validate_incr(reply: &xproto::GetPropertyReply, cap: usize) -> io::Result<()> {
    if reply.format != 32
        || reply.value_len != 1
        || reply.value.len() != 4
        || reply.bytes_after != 0
    {
        return Err(failure(io::ErrorKind::InvalidData));
    }
    let hint = reply
        .value32()
        .and_then(|mut values| values.next())
        .ok_or_else(|| failure(io::ErrorKind::InvalidData))?;
    if hint as usize > cap {
        Err(failure(io::ErrorKind::InvalidData))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            stop: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            admitted: AtomicUsize::new(0),
        })
    }
    fn control(deadline: Instant, cancelled: Cancelled) -> Control {
        Control {
            deadline,
            cancelled: Mutex::new(Some(cancelled)),
        }
    }
    fn reply(format: u8, value_len: u32, value: Vec<u8>) -> xproto::GetPropertyReply {
        xproto::GetPropertyReply {
            format,
            sequence: 0,
            length: (value.len() as u32 + 3) / 4,
            type_: 42,
            bytes_after: 0,
            value_len,
            value,
        }
    }
    #[test]
    fn eight_including_active_and_ninth_rejected() {
        let shared = shared();
        let mut permits: Vec<_> = (0..8).map(|_| Permit::acquire(&shared).unwrap()).collect();
        assert_eq!(
            Permit::acquire(&shared).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        permits.pop();
        assert!(Permit::acquire(&shared).is_ok());
        drop(permits);
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
        shared.closed.store(true, Ordering::Release);
        assert_eq!(
            Permit::acquire(&shared).err().unwrap().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
    #[test]
    fn permit_retires_on_completion_panic() {
        let shared = shared();
        let permit = Permit::acquire(&shared).unwrap();
        let _ = catch_unwind(AssertUnwindSafe(move || {
            let _permit = permit;
            panic!("synthetic completion panic")
        }));
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    }
    #[test]
    fn absolute_expiry_and_cancel_and_panic_are_terminal() {
        assert_eq!(
            control(Instant::now(), Box::new(|| false))
                .check()
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(control(Instant::now() + MAX_TIME, Box::new(|| true))
            .check()
            .is_err());
        assert!(control(
            Instant::now() + MAX_TIME,
            Box::new(|| panic!("synthetic cancel panic"))
        )
        .check()
        .is_err());
    }
    #[test]
    fn stale_window_selection_target_and_property_cannot_complete() {
        let event = xproto::SelectionNotifyEvent {
            response_type: xproto::SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: 0,
            requestor: 10,
            selection: 11,
            target: 12,
            property: 13,
        };
        assert!(selection_identity(&event, 10, 11, 12, 13));
        for (window, selection, target, property) in [
            (9, 11, 12, 13),
            (10, 9, 12, 13),
            (10, 11, 9, 13),
            (10, 11, 12, 9),
        ] {
            assert!(!selection_identity(
                &event, window, selection, target, property
            ));
        }
        let refusal = xproto::SelectionNotifyEvent {
            property: 0,
            ..event
        };
        assert!(selection_identity(&refusal, 10, 11, 12, 13));
        assert!(!selection_identity(&refusal, 9, 11, 12, 13));
    }
    #[test]
    fn property_lengths_are_four_byte_units_not_bytes() {
        for (bytes, units) in [
            (0, 0),
            (1, 1),
            (3, 1),
            (4, 1),
            (5, 2),
            (MAX_BYTES as u32, MAX_BYTES as u32 / 4),
        ] {
            assert_eq!(property_units(bytes, MAX_BYTES).unwrap(), units);
        }
        assert!(property_units(MAX_BYTES as u32 + 1, MAX_BYTES).is_err());
        assert!(property_units(u32::MAX, MAX_BYTES).is_err());
        assert!(property_units(1, 0).is_err());
    }
    #[test]
    fn direct_and_incr_validation_before_append() {
        assert!(validate_text(&reply(8, 3, b"abc".to_vec()), 42, 3).is_ok());
        assert!(validate_text(&reply(8, 3, b"abc".to_vec()), 42, 2).is_err());
        assert!(validate_text(&reply(16, 3, b"abc".to_vec()), 42, 3).is_err());
        assert!(validate_text(&reply(8, 2, b"abc".to_vec()), 42, 3).is_err());
        assert!(validate_text(&reply(8, 0, vec![]), 42, 0).is_ok());
        assert!(validate_incr(&reply(32, 1, 4u32.to_ne_bytes().to_vec()), 4).is_ok());
        assert!(validate_incr(&reply(32, 1, 5u32.to_ne_bytes().to_vec()), 4).is_err());
        assert!(validate_incr(&reply(8, 4, 4u32.to_ne_bytes().to_vec()), 4).is_err());
        assert!(validate_incr(&reply(32, 0, vec![]), 4).is_err());
    }
    #[test]
    fn submission_clamps_and_failure_never_calls_completion() {
        let shared = shared();
        let (tx, rx) = mpsc::sync_channel(8);
        let reader = Reader {
            tx: Some(tx),
            shared: shared.clone(),
            worker: None,
        };
        let before = Instant::now();
        reader
            .submit(
                false,
                usize::MAX,
                before + Duration::from_secs(60),
                Box::new(|| false),
                Box::new(|_| {}),
            )
            .unwrap();
        let request = rx.recv().unwrap();
        assert_eq!(request.cap, MAX_BYTES);
        assert!(request.control.deadline <= Instant::now() + MAX_TIME);
        assert!(request.control.deadline >= before + MAX_TIME);
        drop(request);
        drop(rx);
        let completed = Arc::new(AtomicBool::new(false));
        let flag = completed.clone();
        assert!(reader
            .submit(
                false,
                1,
                Instant::now(),
                Box::new(|| false),
                Box::new(move |_| {
                    flag.store(true, Ordering::Release);
                })
            )
            .is_err());
        assert!(!completed.load(Ordering::Acquire));
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    }
}

// Run only against a dedicated synthetic X server. Never use a personal DISPLAY.
#[cfg(test)]
mod native_tests {
    use super::*;
    use x11rb::wrapper::ConnectionExt;
    enum Offer {
        Direct(Vec<u8>, Duration),
        Incr(Vec<Vec<u8>>, u32),
        Silent,
        StaleThenDirect,
        OwnerReplaced,
    }
    fn next_event(context: &crate::Context, deadline: Instant) -> Event {
        loop {
            assert!(Instant::now() < deadline, "synthetic owner event timeout");
            if let Some(event) = context.connection.poll_for_event().unwrap() {
                return event;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn owner(display: &str, primary: bool, offer: Offer) -> JoinHandle<()> {
        let context = crate::Context::new(Some(display)).unwrap();
        let selection = if primary {
            context.atoms.primary
        } else {
            context.atoms.clipboard
        };
        xproto::set_selection_owner(
            &context.connection,
            context.window,
            selection,
            x11rb::CURRENT_TIME,
        )
        .unwrap();
        context.connection.flush().unwrap();
        assert_eq!(
            xproto::get_selection_owner(&context.connection, selection)
                .unwrap()
                .reply()
                .unwrap()
                .owner,
            context.window
        );
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let request = loop {
                if let Event::SelectionRequest(event) = next_event(&context, deadline) {
                    break event;
                }
            };
            let notify = |requestor| {
                xproto::send_event(
                    &context.connection,
                    false,
                    request.requestor,
                    xproto::EventMask::NO_EVENT,
                    xproto::SelectionNotifyEvent {
                        response_type: xproto::SELECTION_NOTIFY_EVENT,
                        sequence: 0,
                        time: request.time,
                        requestor,
                        selection: request.selection,
                        target: request.target,
                        property: request.property,
                    },
                )
                .unwrap();
                context.connection.flush().unwrap();
            };
            let data = |bytes: &[u8]| {
                context
                    .connection
                    .change_property8(
                        xproto::PropMode::REPLACE,
                        request.requestor,
                        request.property,
                        request.target,
                        bytes,
                    )
                    .unwrap();
                context.connection.flush().unwrap();
            };
            xproto::change_window_attributes(
                &context.connection,
                request.requestor,
                &xproto::ChangeWindowAttributesAux::new().event_mask(
                    xproto::EventMask::PROPERTY_CHANGE | xproto::EventMask::STRUCTURE_NOTIFY,
                ),
            )
            .unwrap();
            match offer {
                Offer::Silent => return,
                Offer::OwnerReplaced => {
                    xproto::set_selection_owner(
                        &context.connection,
                        x11rb::NONE,
                        selection,
                        x11rb::CURRENT_TIME,
                    )
                    .unwrap();
                    data(b"stale");
                    notify(request.requestor);
                }
                Offer::Direct(bytes, delay) => {
                    thread::sleep(delay);
                    data(&bytes);
                    notify(request.requestor);
                }
                Offer::StaleThenDirect => {
                    notify(context.window);
                    thread::sleep(Duration::from_millis(30));
                    data(b"fresh");
                    notify(request.requestor);
                }
                Offer::Incr(chunks, hint) => {
                    xproto::change_window_attributes(
                        &context.connection,
                        request.requestor,
                        &xproto::ChangeWindowAttributesAux::new().event_mask(
                            xproto::EventMask::PROPERTY_CHANGE
                                | xproto::EventMask::STRUCTURE_NOTIFY,
                        ),
                    )
                    .unwrap();
                    context
                        .connection
                        .change_property32(
                            xproto::PropMode::REPLACE,
                            request.requestor,
                            request.property,
                            context.atoms.incr,
                            &[hint],
                        )
                        .unwrap();
                    notify(request.requestor);
                    // Empty chunk list models a header that must be rejected without acknowledgement.
                    if !chunks.is_empty() {
                        for bytes in chunks.iter().chain(std::iter::once(&Vec::new())) {
                            loop {
                                match next_event(&context, deadline) {
                                    Event::PropertyNotify(event)
                                        if event.window == request.requestor
                                            && event.atom == request.property
                                            && event.state == xproto::Property::DELETE =>
                                    {
                                        break
                                    }
                                    Event::DestroyNotify(event)
                                        if event.window == request.requestor =>
                                    {
                                        return
                                    }
                                    _ => {}
                                }
                            }
                            data(bytes);
                        }
                    }
                }
            }
            // Keep the synthetic owner alive through the final ownership check.
            loop {
                if let Event::DestroyNotify(event) = next_event(&context, deadline) {
                    if event.window == request.requestor {
                        break;
                    }
                }
            }
        })
    }
    fn read(reader: &Reader, primary: bool, cap: usize, duration: Duration) -> io::Result<String> {
        let (tx, rx) = mpsc::channel();
        reader
            .submit(
                primary,
                cap,
                Instant::now() + duration,
                Box::new(|| false),
                Box::new(move |result| {
                    tx.send(result).unwrap();
                }),
            )
            .unwrap();
        rx.recv_timeout(Duration::from_secs(4)).unwrap()
    }
    #[test]
    #[ignore = "requires dedicated FROST_X11_TEST_DISPLAY and native AF_UNIX permission"]
    fn synthetic_direct_primary_incr_stale_and_expiry() {
        let display =
            std::env::var("FROST_X11_TEST_DISPLAY").expect("dedicated synthetic display required");
        let reader = Reader::start(Endpoint::connect_display(Some(&display)).unwrap()).unwrap();
        for primary in [false, true] {
            let worker = owner(
                &display,
                primary,
                Offer::Direct(b"synthetic".to_vec(), Duration::from_millis(30)),
            );
            assert_eq!(read(&reader, primary, 9, MAX_TIME).unwrap(), "synthetic");
            worker.join().unwrap();
        }
        let worker = owner(
            &display,
            false,
            Offer::Incr(vec![b"ab".to_vec(), b"cde".to_vec()], 5),
        );
        assert_eq!(read(&reader, false, 5, MAX_TIME).unwrap(), "abcde");
        worker.join().unwrap();
        let worker = owner(&display, false, Offer::Incr(vec![], 100));
        assert!(read(&reader, false, 5, MAX_TIME).is_err());
        worker.join().unwrap();
        let worker = owner(
            &display,
            false,
            Offer::Direct(b"oversized".to_vec(), Duration::ZERO),
        );
        assert!(read(&reader, false, 5, MAX_TIME).is_err());
        worker.join().unwrap();
        let worker = owner(
            &display,
            false,
            Offer::Incr(vec![b"ab".to_vec(), b"cdef".to_vec()], 5),
        );
        assert!(read(&reader, false, 5, MAX_TIME).is_err());
        worker.join().unwrap();
        let worker = owner(&display, false, Offer::Direct(vec![255], Duration::ZERO));
        assert!(read(&reader, false, 5, MAX_TIME).is_err());
        worker.join().unwrap();
        let worker = owner(&display, false, Offer::OwnerReplaced);
        assert!(read(&reader, false, 5, MAX_TIME).is_err());
        worker.join().unwrap();
        let worker = owner(&display, false, Offer::StaleThenDirect);
        assert_eq!(read(&reader, false, 5, MAX_TIME).unwrap(), "fresh");
        worker.join().unwrap();
        let worker = owner(&display, false, Offer::Silent);
        assert_eq!(
            read(&reader, false, 5, Duration::from_millis(60))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        worker.join().unwrap();
        // A normal owner timeout must not poison the healthy connection.
        let worker = owner(
            &display,
            false,
            Offer::Direct(b"alive".to_vec(), Duration::ZERO),
        );
        assert_eq!(read(&reader, false, 5, MAX_TIME).unwrap(), "alive");
        worker.join().unwrap();
    }
    #[test]
    #[ignore = "requires dedicated FROST_X11_TEST_DISPLAY and native AF_UNIX permission"]
    fn synthetic_cancel_saturation_and_teardown() {
        let display =
            std::env::var("FROST_X11_TEST_DISPLAY").expect("dedicated synthetic display required");
        let reader = Reader::start(Endpoint::connect_display(Some(&display)).unwrap()).unwrap();
        let worker = owner(&display, false, Offer::Silent);
        let cancelled = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        for _ in 0..8 {
            let cancelled = cancelled.clone();
            let tx = tx.clone();
            reader
                .submit(
                    false,
                    5,
                    Instant::now() + MAX_TIME,
                    Box::new(move || cancelled.load(Ordering::Acquire)),
                    Box::new(move |result| {
                        tx.send(result).unwrap();
                    }),
                )
                .unwrap();
        }
        assert_eq!(
            reader
                .submit(
                    false,
                    5,
                    Instant::now() + MAX_TIME,
                    Box::new(|| false),
                    Box::new(|_| panic!("unadmitted completion"))
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        worker.join().unwrap();
        cancelled.store(true, Ordering::Release);
        for _ in 0..8 {
            assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap().is_err());
        }
        let worker = owner(
            &display,
            false,
            Offer::Direct(b"alive".to_vec(), Duration::ZERO),
        );
        assert_eq!(read(&reader, false, 5, MAX_TIME).unwrap(), "alive");
        worker.join().unwrap();
        let worker = owner(&display, false, Offer::Silent);
        reader
            .submit(
                false,
                5,
                Instant::now() + MAX_TIME,
                Box::new(|| false),
                Box::new(|_| {}),
            )
            .unwrap();
        worker.join().unwrap();
        let start = Instant::now();
        drop(reader);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    #[ignore = "native AF_UNIX socket permission required; do not bypass sandbox denial"]
    fn stalled_stream_cancellation_and_deadline() {
        use std::os::unix::net::UnixStream;
        let (socket, _peer) = UnixStream::pair().unwrap();
        let (inner, _) = DefaultStream::from_unix_stream(socket).unwrap();
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            admitted: AtomicUsize::new(0),
        });
        let stream = DeadlineStream {
            inner,
            shared: shared.clone(),
            event_poll: AtomicBool::new(false),
            active: Mutex::new(Some(Arc::new(Control {
                deadline: Instant::now() + Duration::from_millis(40),
                cancelled: Mutex::new(Some(Box::new(|| false))),
            }))),
        };
        let start = Instant::now();
        loop {
            if stream.poll(PollMode::Readable).is_err() {
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(shared.closed.load(Ordering::Acquire));
    }
}
