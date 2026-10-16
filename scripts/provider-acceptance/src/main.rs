//! Hosted-only real winit host. No independent reader of the host Wayland socket.
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    num::NonZeroU32,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};
#[derive(Debug)]
enum Event {
    Command(String),
    Done(u64, bool, io::Result<String>),
    Eof,
}
struct App {
    // Explicit drop in exiting: provider, surface, window, context.
    clipboard: Option<window_clipboard::Clipboard>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    context: Option<softbuffer::Context<Rc<Window>>>,
    window: Option<Rc<Window>>,
    proxy: EventLoopProxy<Event>,
    cancelled: HashMap<u64, Arc<AtomicBool>>,
    start: Instant,
}
fn digest(s: &str) -> u64 {
    s.bytes().fold(14695981039346656037, |a, b| {
        (a ^ b as u64).wrapping_mul(1099511628211)
    })
}
impl App {
    fn report(&self, phase: &str, id: u64, primary: bool, result: io::Result<String>) {
        let (ok, len, hash, kind) = match result {
            Ok(s) => (true, s.len(), digest(&s), String::new()),
            Err(e) => (false, 0, 0, format!("{:?}", e.kind())),
        };
        println!("{{\"phase\":\"{phase}\",\"id\":{id},\"primary\":{primary},\"ms\":{},\"ok\":{ok},\"len\":{len},\"digest\":\"{hash:016x}\",\"kind\":\"{kind}\"}}", self.start.elapsed().as_millis());
        io::stdout().flush().unwrap();
    }
    fn submit(&mut self, id: u64, primary: bool) {
        assert!(!self.cancelled.contains_key(&id));
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let proxy = self.proxy.clone();
        let result = self.clipboard.as_ref().unwrap().read_async(
            primary,
            1024 * 1024,
            Instant::now() + Duration::from_secs(3),
            Box::new(move || c.load(Ordering::Acquire)),
            Box::new(move |r| {
                let _ = proxy.send_event(Event::Done(id, primary, r));
            }),
        );
        if result.is_ok() {
            self.cancelled.insert(id, cancel);
        }
        self.report("admit", id, primary, result.map(|()| String::new()));
    }
}
impl ApplicationHandler<Event> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        assert!(self.window.is_none());
        let window = Rc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Frost provider acceptance")
                        .with_inner_size(winit::dpi::PhysicalSize::new(320, 200)),
                )
                .unwrap(),
        );
        let context = softbuffer::Context::new(window.clone()).unwrap();
        self.surface = Some(softbuffer::Surface::new(&context, window.clone()).unwrap());
        self.clipboard =
            Some(unsafe { window_clipboard::Clipboard::connect_bounded(window.as_ref()).unwrap() });
        self.context = Some(context);
        self.window = Some(window.clone());
        window.request_redraw();
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                let w = self.window.as_ref().unwrap();
                let size = w.inner_size();
                if size.width > 0 && size.height > 0 {
                    let s = self.surface.as_mut().unwrap();
                    s.resize(
                        NonZeroU32::new(size.width).unwrap(),
                        NonZeroU32::new(size.height).unwrap(),
                    )
                    .unwrap();
                    let mut buffer = s.buffer_mut().unwrap();
                    buffer.fill(0x224466);
                    w.pre_present_notify();
                    buffer.present().unwrap();
                }
            }
            WindowEvent::Focused(true) => self.report("focused", 0, false, Ok(String::new())),
            WindowEvent::CloseRequested => event_loop.exit(),
            _ => {}
        }
    }
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::Done(id, p, r) => {
                assert!(
                    self.cancelled.remove(&id).is_some(),
                    "duplicate/unadmitted completion"
                );
                self.report("done", id, p, r);
            }
            Event::Eof => event_loop.exit(),
            Event::Command(line) => {
                let args: Vec<_> = line.split_whitespace().collect();
                assert!(!args.is_empty());
                match args[0] {
                    "batch" => {
                        assert_eq!(args.len(), 2);
                        let base: u64 = args[1].parse().unwrap();
                        for i in 0..9 {
                            self.submit(base + i, i % 2 == 1);
                        }
                    }
                    "read" => {
                        assert_eq!(args.len(), 3);
                        self.submit(args[1].parse().unwrap(), args[2] == "primary");
                    }
                    "cancel" => {
                        assert_eq!(args.len(), 1);
                        for c in self.cancelled.values() {
                            c.store(true, Ordering::Release);
                        }
                        self.report("cancel", 0, false, Ok(String::new()));
                    }
                    "sync" => {
                        assert_eq!(args.len(), 3);
                        let p = args[2] == "primary";
                        let c = self.clipboard.as_ref().unwrap();
                        let r = if p {
                            c.read_primary().expect("PRIMARY support")
                        } else {
                            c.read()
                        };
                        self.report(
                            "sync",
                            args[1].parse().unwrap(),
                            p,
                            r.map_err(|_| io::Error::other("native synchronous error")),
                        );
                    }
                    "write" => {
                        assert_eq!(args.len(), 1);
                        let c = self.clipboard.as_mut().unwrap();
                        c.write("owned-clipboard".into()).unwrap();
                        c.write_primary("owned-primary".into())
                            .expect("PRIMARY support")
                            .unwrap();
                        self.report("write", 0, false, Ok(String::new()));
                    }
                    "quit" => {
                        assert_eq!(args.len(), 1);
                        event_loop.exit();
                    }
                    _ => panic!("unknown fixture command"),
                }
            }
        }
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        let start = Instant::now();
        drop(self.clipboard.take());
        drop(self.surface.take());
        drop(self.window.take());
        drop(self.context.take());
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "provider/window teardown exceeded budget"
        );
        self.report("exit", 0, false, Ok(String::new()));
    }
}
fn main() {
    let event_loop = EventLoop::<Event>::with_user_event().build().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let input = proxy.clone();
    // Bounded line reader; stdin belongs only to the fixture harness. Process exit ends reader.
    std::thread::spawn(move || {
        let mut reader = io::stdin().lock();
        for _ in 0..256 {
            let mut line = Vec::new();
            loop {
                let buf = reader.fill_buf().unwrap();
                if buf.is_empty() {
                    let _ = input.send_event(Event::Eof);
                    return;
                }
                let n = buf
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(buf.len(), |i| i + 1);
                assert!(line.len() + n <= 128, "command too long");
                line.extend_from_slice(&buf[..n]);
                reader.consume(n);
                if line.ends_with(b"\n") {
                    break;
                }
            }
            if input
                .send_event(Event::Command(String::from_utf8(line).unwrap()))
                .is_err()
            {
                return;
            }
        }
        panic!("command budget exceeded");
    });
    event_loop
        .run_app(&mut App {
            clipboard: None,
            surface: None,
            context: None,
            window: None,
            proxy,
            cancelled: HashMap::new(),
            start: Instant::now(),
        })
        .unwrap();
}
