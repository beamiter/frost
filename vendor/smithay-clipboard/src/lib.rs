//! Smithay Clipboard
//!
//! Provides access to the Wayland clipboard for gui applications. The user
//! should have surface around.

#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
use std::ffi::c_void;
use std::io::Result;
use std::os::fd::AsRawFd;
use std::sync::mpsc::{self, Receiver};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::time::Instant;

use sctk::reexports::calloop::channel::{self, Sender};
use sctk::reexports::client::Connection;
use sctk::reexports::client::backend::Backend;

mod bounded;
mod mime;
mod state;
mod worker;

/// Access to a Wayland clipboard.
pub struct Clipboard {
    request_sender: Sender<worker::Command>,
    request_receiver: Receiver<Result<String>>,
    clipboard_thread: Option<std::thread::JoinHandle<()>>,
    external_connection: Option<Connection>,
    shutdown: Arc<AtomicBool>,
    admission: Arc<AtomicUsize>,
    next_request: AtomicU64,
}

impl Clipboard {
    /// Creates new clipboard which will be running on its own thread with its
    /// own event queue to handle clipboard requests.
    ///
    /// # Safety
    ///
    /// `display` must be a valid `*mut wl_display` pointer, and it must remain
    /// valid for as long as `Clipboard` object is alive.
    pub unsafe fn new(display: *mut c_void) -> Self {
        unsafe { Self::new_mode(display, false) }
    }

    /// Connect with an external GUI socket reader and bounded async reads.
    ///
    /// # Safety
    /// The display lifetime requirements of [`Self::new`] apply. The host must
    /// keep reading the display while this object exists. Synchronous loads
    /// may only be called on that GUI reader thread, outside a prepared read.
    /// This is the Frost/Iced winit `run_app` integration, not a general-purpose
    /// replacement for hosts with additional concurrent display readers.
    pub unsafe fn new_external_reader(display: *mut c_void) -> Self {
        unsafe { Self::new_mode(display, true) }
    }

    unsafe fn new_mode(display: *mut c_void, external: bool) -> Self {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);

        // Create channel to send data to clipboard thread.
        let (request_sender, rx_chan) = channel::channel();
        // Create channel to get data from the clipboard thread.
        let (clipboard_reply_sender, request_receiver) = mpsc::channel();

        let name = String::from("smithay-clipboard");
        let shutdown = Arc::new(AtomicBool::new(false));
        let external_connection = external.then(|| connection.clone());
        let clipboard_thread = worker::spawn(
            name,
            connection,
            rx_chan,
            clipboard_reply_sender,
            external,
            shutdown.clone(),
        );

        Self {
            request_receiver,
            request_sender,
            clipboard_thread,
            external_connection,
            shutdown,
            admission: Arc::new(AtomicUsize::new(0)),
            next_request: AtomicU64::new(1),
        }
    }

    /// Load clipboard data.
    ///
    /// Loads content from a clipboard on a last observed seat.
    pub fn load(&self) -> Result<String> {
        if self.external_connection.is_some() {
            return self.load_bounded(false);
        }
        let _ = self.request_sender.send(worker::Command::Load);

        if let Ok(reply) = self.request_receiver.recv() {
            reply
        } else {
            // The clipboard thread is dead, however we shouldn't crash downstream, so
            // propogating an error.
            Err(std::io::Error::other("clipboard is dead."))
        }
    }

    /// Store to a clipboard.
    ///
    /// Stores to a clipboard on a last observed seat.
    pub fn store<T: Into<String>>(&self, text: T) {
        let request = worker::Command::Store(text.into());
        let _ = self.request_sender.send(request);
    }

    /// Load primary clipboard data.
    ///
    /// Loads content from a  primary clipboard on a last observed seat.
    pub fn load_primary(&self) -> Result<String> {
        if self.external_connection.is_some() {
            return self.load_bounded(true);
        }
        let _ = self.request_sender.send(worker::Command::LoadPrimary);

        if let Ok(reply) = self.request_receiver.recv() {
            reply
        } else {
            // The clipboard thread is dead, however we shouldn't crash downstream, so
            // propogating an error.
            Err(std::io::Error::other("clipboard is dead."))
        }
    }

    /// Submit a bounded read without waiting for a clipboard source.
    /// Admission failure returns an error without invoking `complete`.
    pub fn read_async(
        &self,
        primary: bool,
        max_bytes: usize,
        deadline: Instant,
        cancelled: Box<dyn Fn() -> bool + Send + 'static>,
        complete: Box<dyn FnOnce(Result<String>) + Send + 'static>,
    ) -> Result<()> {
        if self.external_connection.is_none() || self.shutdown.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "bounded clipboard unavailable",
            ));
        }
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let request =
            bounded::Request::admit(id, max_bytes, deadline, cancelled, complete, &self.admission)?;
        let target = if primary {
            state::SelectionTarget::Primary
        } else {
            state::SelectionTarget::Clipboard
        };
        match self.request_sender.send(worker::Command::LoadBounded(target, request)) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let worker::Command::LoadBounded(_, request) = error.0 {
                    request.reject();
                }
                Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "clipboard worker stopped"))
            },
        }
    }

    fn load_bounded(&self, primary: bool) -> Result<String> {
        use std::io::{Error, ErrorKind};
        let connection =
            self.external_connection.as_ref().ok_or_else(|| Error::other("no external reader"))?;
        let deadline = Instant::now() + bounded::TIMEOUT;
        let cancelled = Arc::new(AtomicBool::new(false));
        struct CancelOnDrop(Arc<AtomicBool>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _cancel = CancelOnDrop(cancelled.clone());
        let (send, receive) = mpsc::sync_channel(1);
        self.read_async(
            primary,
            bounded::MAX_BYTES,
            deadline,
            Box::new(move || cancelled.load(Ordering::Acquire)),
            Box::new(move |result| {
                let _ = send.send(result);
            }),
        )?;
        loop {
            match receive.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Error::new(ErrorKind::BrokenPipe, "clipboard worker stopped"));
                },
                Err(mpsc::TryRecvError::Empty) => {},
            }
            if Instant::now() >= deadline {
                return Err(Error::new(ErrorKind::TimedOut, "clipboard timed out"));
            }
            match connection.flush() {
                Ok(()) => {},
                Err(sctk::reexports::client::backend::WaylandError::Io(error))
                    if error.kind() == ErrorKind::WouldBlock => {},
                Err(_) => {
                    return Err(Error::new(ErrorKind::BrokenPipe, "clipboard display unavailable"));
                },
            }
            if let Some(guard) = connection.prepare_read() {
                let mut fd = libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one initialized pollfd lives for this bounded call.
                let ready = unsafe { libc::poll(&mut fd, 1, 10) };
                if ready > 0 {
                    match guard.read() {
                        Ok(_) => {},
                        Err(sctk::reexports::client::backend::WaylandError::Io(error))
                            if error.kind() == ErrorKind::WouldBlock => {},
                        Err(_) => {
                            return Err(Error::new(
                                ErrorKind::BrokenPipe,
                                "clipboard display unavailable",
                            ));
                        },
                    }
                } else {
                    drop(guard);
                    if ready < 0 && Error::last_os_error().kind() != ErrorKind::Interrupted {
                        return Err(Error::new(
                            ErrorKind::BrokenPipe,
                            "clipboard display poll failed",
                        ));
                    }
                }
            } else {
                // The worker owns dispatch of the guest queue; do not dispatch
                // its protocol callbacks from the GUI or retain a read guard.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    /// Store to a primary clipboard.
    ///
    /// Stores to a primary clipboard on a last observed seat.
    pub fn store_primary<T: Into<String>>(&self, text: T) {
        let request = worker::Command::StorePrimary(text.into());
        let _ = self.request_sender.send(request);
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Shutdown smithay-clipboard.
        let _ = self.request_sender.send(worker::Command::Exit);
        if let Some(clipboard_thread) = self.clipboard_thread.take() {
            let _ = clipboard_thread.join();
        }
    }
}

#[cfg(test)]
mod bounded_native_tests {
    use super::*;
    use std::time::Duration;

    // These tests create only a private socket pair and a deliberately silent
    // synthetic compositor endpoint. Never connect to the user's clipboard.
    #[test]
    #[ignore = "requires native AF_UNIX socket pairs; run on hosted fixture machine"]
    fn native_external_registry_deadline_without_a_host_reader() {
        let (client, _silent_server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let start = Instant::now();
        let result = sctk::reexports::client::globals::registry_queue_init_external_reader::<
            state::State,
        >(&connection, start + Duration::from_millis(30), || false);
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    #[ignore = "requires native AF_UNIX socket pairs; run on hosted fixture machine"]
    fn native_eight_queued_reads_and_teardown_during_silent_initialization() {
        let (client, _silent_server) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        // SAFETY: this owned connection outlives Clipboard, including its join.
        let clipboard =
            unsafe { Clipboard::new_external_reader(connection.backend().display_ptr().cast()) };
        let (send, receive) = mpsc::channel();
        for id in 0..8 {
            let send = send.clone();
            clipboard
                .read_async(
                    false,
                    bounded::MAX_BYTES,
                    Instant::now() + bounded::TIMEOUT,
                    Box::new(|| false),
                    Box::new(move |result| {
                        send.send((id, result.is_err())).unwrap();
                    }),
                )
                .unwrap();
        }
        let rejected_send = send.clone();
        assert_eq!(
            clipboard
                .read_async(
                    true,
                    1,
                    Instant::now() + bounded::TIMEOUT,
                    Box::new(|| false),
                    Box::new(move |_| {
                        rejected_send.send((9, false)).unwrap();
                    })
                )
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        let start = Instant::now();
        drop(clipboard);
        assert!(start.elapsed() < Duration::from_millis(250));
        drop(send);
        let mut replies: Vec<_> = receive.into_iter().collect();
        replies.sort();
        assert_eq!(replies, (0..8).map(|id| (id, true)).collect::<Vec<_>>());
    }
}
