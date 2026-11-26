//! Real PTY regression, isolated because the production overload latch cannot reset.
#![cfg(target_os = "linux")]
use super::*;
use iced::futures::{channel::mpsc, executor::LocalPool, task::LocalSpawnExt};
use iced_winit::futures::subscription::{Event, Tracker};
use std::time::{Duration, Instant};

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "native overload fixture timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn real_pty_overload_quarantines_downstream_return_and_blocked_write() {
    const CHILD: &str = "FROST_NATIVE_OVERLOAD_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let sandbox =
            std::env::temp_dir().join(format!("frost-overload-home-{}", std::process::id()));
        std::fs::create_dir(&sandbox).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .current_dir(&sandbox)
            .env("PATH", "/usr/bin:/bin")
            .env("SHELL", "/bin/sh")
            .env("HOME", &sandbox)
            .env("XDG_CONFIG_HOME", sandbox.join("config"))
            .env("XDG_DATA_HOME", sandbox.join("data"))
            .env("XDG_CACHE_HOME", sandbox.join("cache"))
            .env("XDG_STATE_HOME", sandbox.join("state"));
        // The test binary may link toolchain dynamic libraries, but must not
        // inherit configuration, desktop sockets, credentials, or user homes.
        if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
            command.env("LD_LIBRARY_PATH", path);
        }
        let output = command
            .args(["--exact", "native_overload_tests::real_pty_overload_quarantines_downstream_return_and_blocked_write", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        std::fs::remove_dir_all(&sandbox).unwrap();
        assert!(
            output.status.success(),
            "isolated PTY test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(!Event::input_overloaded());
    let dir = std::env::temp_dir().join(format!("frost-native-overload-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let ready = dir.join("ready");
    let release = dir.join("release");
    let kernel_ready = dir.join("kernel-ready");
    let captured = dir.join("captured");
    // This fixture never reads until released, so the kernel input buffer really
    // reaches WouldBlock; raw/no-echo prevents line discipline transformations.
    let script = r#"
import fcntl, os, struct, termios, time, tty
tty.setraw(0)
open("ready", "w").close()
while not os.path.exists("release"):
    if struct.unpack("i", fcntl.ioctl(0, termios.FIONREAD, bytes(4)))[0]:
        open("kernel-ready", "w").close()
    time.sleep(0.01)
with open("captured", "wb", buffering=0) as output:
    while chunk := os.read(0, 4096):
        output.write(chunk)
"#;
    let argv = vec!["/usr/bin/python3".into(), "-c".into(), script.into()];
    let mut config = Config::default();
    config.vim9.auto_gui = false;
    config.shell = Some("/bin/sh".into());
    config.eva_companion_enabled = false;
    let (mut host, startup) = Frost::new(config.clone(), None, None, Default::default());
    drop(startup); // Do not execute startup tasks (including update checks).
    for session in &mut host.sessions {
        session.pty.stop();
    }
    host.sessions.clear();
    let mut session = Session::spawn_argv(&config, 0, 80, 24, dir.to_str(), Some(&argv)).unwrap();
    wait_until(|| ready.exists());
    const BULK_BYTES: usize = 1024 * 1024;
    assert!(session.write_pty(&vec![b'x'; BULK_BYTES]));
    wait_until(|| kernel_ready.exists());
    // The child does not read. Bulk input cannot fit in the kernel and the
    // actor must retain the in-flight message with Return queued behind it.
    wait_until(|| {
        session
            .pty
            .input_backlog()
            .is_ok_and(|queue| queue.user_bytes == BULK_BYTES)
    });
    assert!(session.write_pty(b"\r"));
    assert_eq!(
        session.pty.input_backlog().unwrap().user_bytes,
        BULK_BYTES + 1
    );

    let mut tracker = Tracker::new();
    let mut pool = LocalPool::new();
    let (output, mut received) = mpsc::channel(1);
    let subscription = iced_winit::futures::subscription::filter_map("overload-pty-test", Some);
    for future in tracker.update(
        iced_winit::futures::subscription::into_recipes(subscription).into_iter(),
        output,
    ) {
        pool.spawner().spawn_local(future).unwrap();
    }
    let enter = Event::Interaction {
        window: iced::window::Id::unique(),
        event: iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
            modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Enter),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::NONE,
            text: Some("\r".into()),
            repeat: false,
        }),
        status: iced::event::Status::Ignored,
    };
    tracker.broadcast(enter);
    pool.run_until_stalled();
    // Oversized admission triggers the production latch, not a test setter.
    tracker.broadcast(Event::Interaction {
        window: iced::window::Id::unique(),
        event: iced::Event::InputMethod(iced_winit::core::input_method::Event::Commit(
            "x".repeat(32 * 1024 * 1024),
        )),
        status: iced::event::Status::Ignored,
    });
    assert!(Event::input_overloaded());
    let downstream = received
        .try_recv()
        .expect("Return was not already downstream");
    assert!(
        !session.write_pty(b"\r"),
        "downstream Return crossed the writer gate"
    );
    assert!(
        session.pty.write(b"\r").is_err(),
        "direct PTY bypass crossed final gate"
    );
    std::fs::write(&release, b"go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let event = session
            .pty
            .events()
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("kernel write must hit the latched input fence after the reader resumes");
        if let SessionEvent::Error(error) = event {
            assert!(error.contains("native input overloaded"), "{error}");
            break;
        }
    }
    wait_until(|| std::fs::metadata(&captured).is_ok_and(|m| m.len() > 0));
    // Exercise the actual host reducer with the previously delivered event,
    // not a reconstructed key or a textual source assertion.
    host.sessions.push(session);
    let Event::Interaction {
        window,
        event: iced::Event::Keyboard(key),
        ..
    } = downstream
    else {
        panic!("expected the queued keyboard event");
    };
    drop(host.update(Message::WindowKey(window, key)));
    assert!(host.sessions[0].pty.input_backlog().is_err());
    let session_count = host.sessions.len();
    for message in [
        Message::Pasted(host.sessions[0].view_attachment.ticket(), Some("printf SHOULD_NOT_RUN\n".into())),
        Message::AgentSubmit,
        Message::AiChatsSubmit,
        Message::SidebarDialogSubmit,
        Message::NewSession,
    ] {
        drop(host.update(message));
        assert_eq!(host.sessions.len(), session_count);
        assert!(host.sessions[0].pty.input_backlog().is_err());
    }
    std::thread::sleep(Duration::from_millis(100));
    let bytes = std::fs::read(&captured).unwrap();
    assert!(!bytes.is_empty() && bytes.len() < BULK_BYTES);
    assert!(
        bytes.iter().all(|&byte| byte == b'x'),
        "Return reached the actual child"
    );
    host.sessions[0].pty.stop();
}
