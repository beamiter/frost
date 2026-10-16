#[cfg(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "emscripten",
        target_os = "redox"
    ))
))]
#[path = "platform/linux.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "platform/windows.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "platform/macos.rs"]
mod platform;

#[cfg(target_os = "ios")]
#[path = "platform/ios.rs"]
mod platform;

#[cfg(target_os = "android")]
#[path = "platform/android.rs"]
mod platform;

#[cfg(not(any(
    all(
        unix,
        not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "android",
            target_os = "emscripten",
            target_os = "redox"
        ))
    ),
    target_os = "windows",
    target_os = "macos",
    target_os = "ios",
    target_os = "android"
)))]
#[path = "platform/dummy.rs"]
mod platform;

use raw_window_handle::HasDisplayHandle;
use std::error::Error;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

pub struct Clipboard {
    raw: Box<dyn ClipboardProvider>,
    admission: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
}

impl Clipboard {
    /// Safety: the display handle must be valid for the lifetime of `Clipboard`
    pub unsafe fn connect<W: HasDisplayHandle>(
        window: &W,
    ) -> Result<Self, Box<dyn Error>> {
        let raw = platform::connect(window)?;

        Ok(Clipboard {
            raw,
            admission: Arc::new(AtomicUsize::new(0)),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Bounded native read integration for Linux winit `run_app`.
    ///
    /// Safety: the display must outlive this object. On Wayland the host must
    /// read its display while this object exists; synchronous reads are only
    /// valid on that host reader thread, outside a prepared-read guard.
    #[cfg(target_os = "linux")]
    pub unsafe fn connect_bounded<W: HasDisplayHandle>(
        window: &W,
    ) -> Result<Self, Box<dyn Error>> {
        let raw = unsafe { platform::connect_bounded(window)? };
        Ok(Clipboard {
            raw,
            admission: Arc::new(AtomicUsize::new(0)),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Submit without waiting. Errors mean no completion callback was invoked.
    pub fn read_async(
        &self,
        primary: bool,
        max_bytes: usize,
        deadline: std::time::Instant,
        cancelled: Box<dyn Fn() -> bool + Send + 'static>,
        complete: Box<dyn FnOnce(std::io::Result<String>) + Send + 'static>,
    ) -> std::io::Result<()> {
        loop {
            let count = self.admission.load(Ordering::Acquire);
            if count >= 8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "clipboard busy",
                ));
            }
            if self
                .admission
                .compare_exchange_weak(
                    count,
                    count + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                break;
            }
        }
        let permit = ReadPermit(self.admission.clone());
        let shutdown = self.shutdown.clone();
        self.raw.read_async(
            primary,
            max_bytes.min(1024 * 1024),
            deadline.min(
                std::time::Instant::now() + std::time::Duration::from_secs(3),
            ),
            Box::new(move || shutdown.load(Ordering::Acquire) || cancelled()),
            Box::new(move |result| {
                let _permit = permit;
                complete(result);
            }),
        )
    }

    pub fn read(&self) -> Result<String, Box<dyn Error>> {
        self.raw.read()
    }

    pub fn write(&mut self, contents: String) -> Result<(), Box<dyn Error>> {
        self.raw.write(contents)
    }
}

impl Clipboard {
    pub fn read_primary(&self) -> Option<Result<String, Box<dyn Error>>> {
        self.raw.read_primary()
    }

    pub fn write_primary(
        &mut self,
        contents: String,
    ) -> Option<Result<(), Box<dyn Error>>> {
        self.raw.write_primary(contents)
    }
}

pub trait ClipboardProvider {
    fn read_async(
        &self,
        _primary: bool,
        _max_bytes: usize,
        _deadline: std::time::Instant,
        _cancelled: Box<dyn Fn() -> bool + Send + 'static>,
        _complete: Box<dyn FnOnce(std::io::Result<String>) + Send + 'static>,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "bounded clipboard unavailable",
        ))
    }

    fn read(&self) -> Result<String, Box<dyn Error>>;

    fn write(&mut self, contents: String) -> Result<(), Box<dyn Error>>;

    fn read_primary(&self) -> Option<Result<String, Box<dyn Error>>> {
        None
    }

    fn write_primary(
        &mut self,
        _contents: String,
    ) -> Option<Result<(), Box<dyn Error>>> {
        None
    }
}

struct ReadPermit(Arc<AtomicUsize>);
impl Drop for ReadPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };
    struct Pending {
        cap: usize,
        deadline: Instant,
        cancelled: Box<dyn Fn() -> bool + Send>,
        complete: Box<dyn FnOnce(std::io::Result<String>) + Send>,
    }
    struct Provider(Rc<RefCell<Vec<Pending>>>);
    impl ClipboardProvider for Provider {
        fn read_async(
            &self,
            _: bool,
            cap: usize,
            deadline: Instant,
            cancelled: Box<dyn Fn() -> bool + Send>,
            complete: Box<dyn FnOnce(std::io::Result<String>) + Send>,
        ) -> std::io::Result<()> {
            self.0.borrow_mut().push(Pending {
                cap,
                deadline,
                cancelled,
                complete,
            });
            Ok(())
        }
        fn read(&self) -> Result<String, Box<dyn Error>> {
            unreachable!()
        }
        fn write(&mut self, _: String) -> Result<(), Box<dyn Error>> {
            unreachable!()
        }
    }
    #[test]
    fn common_admission_bounds_cancellation_and_teardown() {
        let jobs = Rc::new(RefCell::new(Vec::new()));
        let admission = Arc::new(AtomicUsize::new(0));
        let clipboard = Clipboard {
            raw: Box::new(Provider(jobs.clone())),
            admission: admission.clone(),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let start = Instant::now();
        for _ in 0..8 {
            clipboard
                .read_async(
                    false,
                    usize::MAX,
                    start + Duration::from_secs(60),
                    Box::new(|| false),
                    Box::new(|_| {}),
                )
                .unwrap();
        }
        assert_eq!(
            clipboard
                .read_async(
                    true,
                    1,
                    start + Duration::from_secs(60),
                    Box::new(|| false),
                    Box::new(|_| panic!("not admitted"))
                )
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        let now = Instant::now();
        for job in jobs.borrow().iter() {
            assert_eq!(job.cap, 1024 * 1024);
            assert!(job.deadline <= now + Duration::from_secs(3));
            assert!(!(job.cancelled)());
        }
        let job = jobs.borrow_mut().pop().unwrap();
        (job.complete)(Ok(String::new()));
        assert_eq!(admission.load(Ordering::Acquire), 7);
        drop(clipboard);
        assert!(jobs.borrow().iter().all(|job| (job.cancelled)()));
        jobs.borrow_mut().clear();
        assert_eq!(admission.load(Ordering::Acquire), 0);
    }
}
