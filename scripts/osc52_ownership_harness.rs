#![allow(dead_code, unused_variables)]
use std::collections::VecDeque;
type RawFd = i32;
// CAP
#[derive(Default)]
struct TerminalState {
    pending_osc52_clipboard_queries: VecDeque<&'static [u8]>,
    osc_reply_terminator: &'static [u8],
    responses: Vec<(String, Vec<u8>)>,
}
impl TerminalState {
    // Fake encoder sink captures only synthetic payloads admitted by production.
    fn respond_osc52_clipboard(&mut self, content: &str, terminator: &[u8]) {
        self.responses
            .push((content.to_owned(), terminator.to_vec()));
    }
    fn query(&mut self) {
        let data = "?";
        // QUERY
    }
}
// HELPERS
struct Session {
    id: usize,
    master_fd: RawFd,
    clipboard_read_in_flight: bool,
    osc52_pending_refusals: Option<VecDeque<&'static [u8]>>,
    terminal: TerminalState,
    pty: Vec<(String, Vec<u8>)>,
}
impl Session {
    fn flush_responses(&mut self) {
        self.pty.append(&mut self.terminal.responses);
    }
    fn refresh(&mut self) {}
}
struct Config {
    allow_clipboard_read: bool,
}
enum Message {
    Osc52Query(usize, RawFd, &'static [u8], Option<String>),
}
struct App {
    config: Config,
    sessions: Vec<Session>,
    host_clipboard_read_owner: Option<(usize, RawFd)>,
}
impl App {
    // METHODS
    fn update(&mut self, message: Message) {
        match message {
            // CALLBACK
        }
    }
}
fn session(id: usize, fd: RawFd) -> Session {
    Session {
        id,
        master_fd: fd,
        clipboard_read_in_flight: true,
        osc52_pending_refusals: Some(VecDeque::new()),
        terminal: TerminalState::default(),
        pty: Vec::new(),
    }
}
fn app() -> App {
    App {
        config: Config {
            allow_clipboard_read: true,
        },
        sessions: vec![session(10, 5), session(11, 6)],
        host_clipboard_read_owner: Some((10, 5)),
    }
}
fn callback(id: usize, fd: RawFd, text: &str) -> Message {
    Message::Osc52Query(id, fd, b"\x07", Some(text.into()))
}
#[test]
fn late_callback_after_close_and_fd_reuse_cannot_reach_replacement() {
    let mut a = app();
    a.sessions.remove(0);
    a.release_host_clipboard_read(10, 5);
    assert_eq!(a.host_clipboard_read_owner, None);
    a.sessions.push(session(12, 5)); // reused OS descriptor, new stable identity
    assert!(a.try_begin_host_clipboard_read(12, 5));
    a.update(callback(10, 5, "synthetic-private"));
    assert_eq!(a.host_clipboard_read_owner, Some((12, 5)));
    assert!(a.sessions.iter().all(|s| s.pty.is_empty()));
    assert!(a.sessions[1].clipboard_read_in_flight);
}
#[test]
fn same_id_different_descriptor_is_not_the_original_session() {
    let mut a = app();
    a.sessions[0].master_fd = 99;
    a.update(callback(10, 5, "synthetic-private"));
    assert!(a.sessions[0].pty.is_empty());
}
#[test]
fn switching_and_reordering_sessions_preserves_original_destination() {
    let mut a = app();
    a.sessions.swap(0, 1);
    a.update(callback(10, 5, "synthetic"));
    assert!(a.sessions[0].pty.is_empty());
    assert_eq!(a.sessions[1].pty[0].0, "synthetic");
    assert!(!a.sessions[1].clipboard_read_in_flight);
}
#[test]
fn revocation_before_callback_returns_empty() {
    let mut a = app();
    a.config.allow_clipboard_read = false;
    a.update(callback(10, 5, "synthetic-private"));
    assert_eq!(a.sessions[0].pty[0].0, "");
}
#[test]
fn absent_clipboard_returns_empty() {
    let mut a = app();
    a.update(Message::Osc52Query(10, 5, b"\x07", None));
    assert_eq!(a.sessions[0].pty[0].0, "");
}
#[test]
fn oversized_fake_clipboard_is_refused_at_exact_byte_boundary() {
    for size in [
        MAX_CLIPBOARD_RESPONSE_BYTES,
        MAX_CLIPBOARD_RESPONSE_BYTES + 1,
    ] {
        let mut a = app();
        a.update(callback(10, 5, &"x".repeat(size)));
        assert_eq!(
            a.sessions[0].pty[0].0.len(),
            if size > MAX_CLIPBOARD_RESPONSE_BYTES {
                0
            } else {
                size
            }
        );
    }
}
#[test]
fn parser_and_cross_batch_refusals_remain_bounded_and_ordered() {
    let mut a = app();
    let s = &mut a.sessions[0];
    s.terminal.osc_reply_terminator = b"\x1b\\";
    for _ in 0..1000 {
        s.terminal.query();
        assert!(defer_osc52_refusal(
            &mut s.osc52_pending_refusals,
            b"\x1b\\"
        ));
    }
    assert_eq!(s.terminal.pending_osc52_clipboard_queries.len(), 8);
    assert_eq!(s.osc52_pending_refusals.as_ref().unwrap().len(), 8);
    a.update(callback(10, 5, "synthetic"));
    assert_eq!(a.sessions[0].pty.len(), 9); // one read plus eight deferred refusals
    assert_eq!(a.sessions[0].pty[0].0, "synthetic");
    assert!(a.sessions[0].pty[1..]
        .iter()
        .all(|(body, term)| body.is_empty() && term == b"\x1b\\"));
}
#[test]
fn host_reads_are_single_flight_and_foreign_completion_cannot_release() {
    let mut a = app();
    assert!(!a.try_begin_host_clipboard_read(11, 6));
    a.finish_host_clipboard_read(11, 6);
    assert_eq!(a.host_clipboard_read_owner, Some((10, 5)));
    a.finish_host_clipboard_read(10, 5);
    assert!(a.try_begin_host_clipboard_read(11, 6));
}
