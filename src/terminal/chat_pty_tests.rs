//! Explicit acceptance of parser/projection against one real Bash PTY.
//! This does not substitute for desktop/pointer/render verification.
use super::*;
use crate::chat_mode::ChatSurface;
use crate::pty::Pty;
use std::time::{Duration, Instant};

struct Fixture {
    pty: Pty,
    terminal: TerminalState,
    view: ProjectionViewState,
    policy: ProjectionPolicy,
    phase: &'static str,
}

impl Fixture {
    fn write(&mut self, bytes: &[u8]) {
        self.terminal.note_user_input(bytes);
        let mut offset = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while offset < bytes.len() {
            offset += self.pty.write(&bytes[offset..]).expect("native PTY write");
            assert!(Instant::now() < deadline, "PTY write timed out");
        }
    }

    fn poll(&mut self, predicate: impl Fn(&TerminalState, &[u8]) -> bool) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut output = Vec::new();
        loop {
            let view =
                self.terminal
                    .get_projected_viewport_with_state(true, &self.policy, &mut self.view);
            let cursor = self
                .terminal
                .raw_cell_origin_at_absolute(
                    self.terminal.scrollback.len() + self.terminal.cursor_row,
                    self.terminal.cursor_col,
                )
                .unwrap();
            assert!(
                view.raw_to_view(cursor).is_some(),
                "native cursor stays visible"
            );
            if predicate(&self.terminal, &output) {
                return output;
            }
            assert!(
                Instant::now() < deadline,
                "PTY response timed out during {} ({} bytes; {:?})",
                self.phase,
                output.len(),
                self.terminal.current_zone_state
            );
            let mut bytes = [0u8; 8192];
            // The owned PTY master is valid and non-blocking; only this test
            // reads it. No renderer or second shell participates.
            let read =
                unsafe { libc::read(self.pty.master_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
            if read > 0 {
                let bytes = &bytes[..read as usize];
                self.terminal.process_batch(bytes);
                output.extend_from_slice(bytes);
            } else {
                assert!(read != 0, "unexpected PTY EOF");
                let error = std::io::Error::last_os_error();
                assert!(
                    error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EINTR),
                    "PTY read failed: {error}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn submit(&mut self, command: &[u8]) -> u64 {
        assert!(self.terminal.chat_surface().allows_submit_affordance());
        let generation = self.terminal.agent_prompt_generation;
        self.write(command);
        let enter = crate::encode_key(
            &iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
            iced::keyboard::Location::Standard,
            iced::keyboard::Modifiers::empty(),
            None,
            self.terminal.is_application_cursor_keys(),
            crate::KeyboardEnhancements::default(),
        )
        .unwrap();
        self.write(&enter);
        self.terminal.note_chat_submit();
        assert!(!self.terminal.chat_surface().allows_submit_affordance());
        generation
    }

    fn ready_after(&mut self, generation: u64) -> Vec<u8> {
        self.poll(|terminal, _| {
            terminal.chat_surface() == ChatSurface::Prompt
                && terminal.agent_prompt_generation > generation
        })
    }
}

#[test]
#[ignore = "requires a real Bash PTY; run explicitly for Chat acceptance"]
fn chat_real_bash_preserves_native_state_completion_multiline_hidden_input_and_interrupt() {
    let argv = ["/bin/bash", "--noprofile", "--norc", "-i"].map(str::to_owned);
    let pty = Pty::new_with_cwd_env(
        100,
        30,
        Some("/tmp"),
        None,
        Some("/bin/bash"),
        Some(&argv),
        &[
            ("HISTFILE", "/dev/null"),
            ("INPUTRC", "/dev/null"),
            ("BASH_ENV", "/dev/null"),
        ],
    )
    .expect("spawn real Bash PTY");
    let pid = pty.get_child_pid();
    let mut policy = ProjectionPolicy::new();
    policy.set_chat(true);
    policy.set_native_chrome(false, 3);
    let mut fixture = Fixture {
        pty,
        terminal: TerminalState::new(100, 30),
        view: ProjectionViewState::new(),
        policy,
        phase: "initialize integration",
    };
    // Standard shell integration only. Every subsequent byte is sent through
    // this same native master, including completion and foreground stdin.
    fixture.write(b"PS1=$'\\[\\e]133;A\\a\\]$ \\[\\e]133;B\\a\\]'; PS2='> '; PS0=$'\\e]133;C\\a'; PROMPT_COMMAND='printf \"\\033]133;D;%s\\007\" \"$?\"'\r");
    fixture.poll(|terminal, _| terminal.chat_surface() == ChatSurface::Prompt);

    fixture.phase = "size";
    let generation = fixture.submit(b"printf 'SIZE:'; stty size");
    let output = fixture.ready_after(generation);
    assert!(String::from_utf8_lossy(&output).contains("SIZE:30 100"));
    fixture.phase = "persist environment";
    let generation = fixture.submit(b"cd /; export FROST_CHAT_FIXTURE=samepty");
    fixture.ready_after(generation);
    let generation = fixture.submit(b"printf 'PERSIST:%s:%s\\n' \"$PWD\" \"$FROST_CHAT_FIXTURE\"");
    let output = fixture.ready_after(generation);
    assert!(String::from_utf8_lossy(&output).contains("PERSIST:/:samepty"));
    assert_eq!(fixture.pty.get_child_pid(), pid);

    fixture.phase = "completion setup";
    let generation = fixture.submit(b"frost_chat_fixture_complete() { :; }");
    fixture.ready_after(generation);
    fixture.phase = "Tab completion";
    fixture.write(b"frost_chat_fixture_comp\t");
    fixture.poll(|terminal, output| {
        terminal.chat_surface() == ChatSurface::Prompt
            && String::from_utf8_lossy(output).contains("lete")
    });
    fixture.phase = "submit completion";
    let generation = fixture.submit(b"");
    fixture.ready_after(generation);

    fixture.phase = "multiline";
    let generation = fixture.submit(b"for part in one two; do");
    fixture.poll(|_, output| output.windows(2).any(|pair| pair == b"> "));
    fixture.write(b"printf 'MULTI:%s\\n' \"$part\"; done\r");
    let output = fixture.ready_after(generation);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("MULTI:one") && text.contains("MULTI:two"));

    fixture.phase = "hidden read";
    let generation =
        fixture.submit(b"read -s -p 'Secret:' token; printf '\\nLENGTH:%s\\n' \"${#token}\"");
    fixture.poll(|terminal, output| {
        terminal.chat_surface() == ChatSurface::Running
            && String::from_utf8_lossy(output).contains("Secret:")
    });
    assert!(!fixture.terminal.chat_surface().allows_submit_affordance());
    fixture.write(b"synthetic\r");
    let output = fixture.ready_after(generation);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("LENGTH:9"));
    assert!(!text.contains("synthetic"));
    assert!(fixture.terminal.command_zones.iter().all(|zone| !zone
        .command
        .as_deref()
        .unwrap_or_default()
        .contains("synthetic")));

    fixture.phase = "interrupt child readiness";
    let generation = fixture.submit(b"sleep 30");
    let fd = fixture.pty.master_fd();
    // Bash emits PS0 before handing the foreground group to its child. Wait
    // for exec of the actual sleep as well: tcsetpgrp can precede the child
    // restoring inherited SIGINT disposition. Otherwise this unrealistically
    // immediate test can race Bash's fork/exec setup instead of application
    // stdin. This readiness check is test-only, not a button restriction.
    fixture.poll(|terminal, _| {
        terminal.chat_surface() == ChatSurface::Running
            && jterm_core::process::tty_foreground_pgid(fd).is_some_and(|group| {
                group != pid
                    && std::fs::read_to_string(format!("/proc/{group}/comm"))
                        .is_ok_and(|name| name.trim() == "sleep")
                    && jterm_core::process::tty_foreground_pgid(fd) == Some(group)
            })
    });
    fixture.phase = "interrupt completion";
    let interrupt = crate::encode_key(
        &iced::keyboard::Key::Character("c".into()),
        iced::keyboard::Location::Standard,
        iced::keyboard::Modifiers::CTRL,
        None,
        false,
        crate::KeyboardEnhancements::default(),
    )
    .unwrap();
    fixture.write(&interrupt);
    assert_eq!(fixture.terminal.chat_surface(), ChatSurface::Running);
    fixture.ready_after(generation);
    assert_eq!(
        fixture.terminal.command_zones.back().unwrap().exit_code,
        Some(130)
    );
    assert_eq!(fixture.pty.get_child_pid(), pid);
}
