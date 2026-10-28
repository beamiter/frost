use super::*;
use crate::core::{Event as UiEvent, Point, event, input_method, keyboard, mouse, theme, window};
use futures::{StreamExt, executor::LocalPool, task::LocalSpawnExt};
use std::sync::Arc;

struct Passthrough {
    generation: u64,
    lifetime: Arc<()>,
}

impl Recipe for Passthrough {
    type Output = Event;
    fn hash(&self, state: &mut Hasher) {
        state.write_u64(self.generation);
    }
    fn stream(self: Box<Self>, input: crate::subscription::EventStream) -> crate::BoxStream<Event> {
        let lifetime = self.lifetime;
        input
            .inspect(move |_| {
                let _ = Arc::strong_count(&lifetime);
            })
            .boxed()
    }
}

fn interaction(event: UiEvent) -> Event {
    Event::Interaction {
        window: window::Id::unique(),
        event,
        status: event::Status::Ignored,
    }
}

fn key_press(value: &str) -> Event {
    interaction(UiEvent::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Character(value.into()),
        modified_key: keyboard::Key::Character(value.into()),
        physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyA),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::NONE,
        text: Some(value.into()),
        repeat: false,
    }))
}

fn subscribe(
    tracker: &mut Tracker,
    pool: &mut LocalPool,
    output: mpsc::Sender<Event>,
    generation: u64,
) -> std::sync::Weak<()> {
    let lifetime = Arc::new(());
    let weak = Arc::downgrade(&lifetime);
    let recipes: Vec<Box<dyn Recipe<Output = Event>>> = vec![Box::new(Passthrough {
        generation,
        lifetime,
    })];
    for future in tracker.update(recipes.into_iter(), output) {
        pool.spawner()
            .spawn_local(future)
            .expect("spawn subscription");
    }
    weak
}

fn drain(pool: &mut LocalPool, output: &mut mpsc::Receiver<Event>) -> Vec<Event> {
    let mut received = Vec::new();
    loop {
        pool.run_until_stalled();
        let before = received.len();
        while let Ok(event) = output.try_recv() {
            received.push(event);
        }
        if received.len() == before {
            break;
        }
    }
    received
}

#[test]
fn stalled_consumer_keeps_every_keyboard_byte_above_old_hundred_event_limit() {
    let mut tracker = Tracker::new();
    let mut pool = LocalPool::new();
    let (output, mut received) = mpsc::channel(1);
    let _ = subscribe(&mut tracker, &mut pool, output, 1);
    let expected: Vec<_> = (0..170)
        .map(|i| key_press(&((b'a' + (i % 26)) as char).to_string()))
        .collect();
    // The consumer is deliberately unpolled during the entire burst. Increasing
    // typing delays or relying on executor luck cannot make this test pass.
    for event in &expected {
        tracker.broadcast(event.clone());
    }
    let actual = drain(&mut pool, &mut received);
    assert_eq!(
        actual.len(),
        expected.len(),
        "input event lost before listener"
    );
    assert!(actual == expected, "input order or bytes changed");
}

#[test]
fn keyboard_modifier_focus_and_non_input_events_keep_one_order_under_backpressure() {
    let mut tracker = Tracker::new();
    let mut pool = LocalPool::new();
    let (output, mut received) = mpsc::channel(1);
    let _ = subscribe(&mut tracker, &mut pool, output, 2);
    let expected: Vec<_> = (0..4096)
        .map(|i| match i % 8 {
            0 => key_press("α"),
            1 => interaction(UiEvent::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::SHIFT,
            ))),
            2 => interaction(UiEvent::Window(window::Event::Focused)),
            3 => interaction(UiEvent::Window(window::Event::Unfocused)),
            4 => interaction(UiEvent::Mouse(mouse::Event::CursorMoved {
                position: Point::new(i as f32, 1.0),
            })),
            5 => interaction(UiEvent::InputMethod(input_method::Event::Preedit(
                "拼".into(),
                Some(0..3),
            ))),
            6 => interaction(UiEvent::InputMethod(input_method::Event::Commit(
                "输入".into(),
            ))),
            _ => Event::SystemThemeChanged(theme::Mode::Dark),
        })
        .collect();
    for event in &expected {
        tracker.broadcast(event.clone());
    }
    let actual = drain(&mut pool, &mut received);
    assert_eq!(actual.len(), expected.len(), "interaction event lost");
    assert!(actual == expected, "focus/modifier/input order changed");
}

#[test]
fn cancellation_releases_stalled_non_input_backlog_before_next_generation() {
    let mut tracker = Tracker::new();
    let mut pool = LocalPool::new();
    let (output, mut received) = mpsc::channel(1);
    let lifetime = subscribe(&mut tracker, &mut pool, output.clone(), 3);
    let rss = || {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .unwrap_or("unavailable")
            .to_owned()
    };
    let before = rss();
    for _ in 0..65_536 {
        tracker.broadcast(Event::SystemThemeChanged(theme::Mode::Light));
    }
    let queued = rss();
    assert!(
        tracker
            .update(std::iter::empty(), output.clone())
            .is_empty()
    );
    pool.run_until_stalled();
    assert!(
        lifetime.upgrade().is_none(),
        "cancelled stream retained its inbox"
    );
    assert!(
        received.try_recv().is_err(),
        "cancelled backlog reached next opening"
    );
    eprintln!(
        "non-input backlog65536 EventSize{} before[{before}] queued[{queued}] after[{}]; no hard memory cap is claimed",
        std::mem::size_of::<Event>(),
        rss()
    );
    let _ = subscribe(&mut tracker, &mut pool, output, 4);
    let expected = key_press("new-generation");
    tracker.broadcast(expected.clone());
    assert!(
        drain(&mut pool, &mut received) == vec![expected],
        "cancelled generation leaked events"
    );
}

#[test]
fn dropped_output_receiver_releases_queued_subscription_events() {
    let mut tracker = Tracker::new();
    let mut pool = LocalPool::new();
    let (output, received) = mpsc::channel(1);
    let lifetime = subscribe(&mut tracker, &mut pool, output, 5);
    for _ in 0..4096 {
        tracker.broadcast(key_press("synthetic"));
    }
    drop(received);
    pool.run_until_stalled();
    assert!(
        lifetime.upgrade().is_none(),
        "closed consumer kept inbox alive"
    );
    assert!(
        tracker
            .subscriptions
            .values()
            .all(|e| e.listener.as_ref().is_none_or(|s| s.is_closed()))
    );
}

#[test]
fn cancellation_interrupts_a_send_blocked_by_a_live_full_output_sink() {
    let mut tracker = Tracker::new();
    let mut pool = LocalPool::new();
    let (output, mut received) = mpsc::channel(1);
    let lifetime = subscribe(&mut tracker, &mut pool, output.clone(), 6);
    for _ in 0..4096 {
        tracker.broadcast(key_press("cancelled-generation"));
    }
    // Poll with a live receiver that deliberately does not drain. The finite
    // output capacity stalls send while thousands of input events remain.
    pool.run_until_stalled();
    assert!(lifetime.upgrade().is_some());
    assert!(
        tracker
            .update(std::iter::empty(), output.clone())
            .is_empty()
    );
    pool.run_until_stalled();
    assert!(
        lifetime.upgrade().is_none(),
        "cancellation could not release an inbox while output send was pending"
    );
    // Messages already admitted to the downstream sink cannot be recalled.
    // They are distinct from the old subscription's still-queued backlog.
    let already_sent = drain(&mut pool, &mut received);
    assert!(!already_sent.is_empty());
    assert!(
        already_sent.len() <= 2,
        "internal backlog leaked after cancellation"
    );
    let _ = subscribe(&mut tracker, &mut pool, output, 7);
    let expected = key_press("new-generation");
    tracker.broadcast(expected.clone());
    assert!(
        drain(&mut pool, &mut received) == vec![expected],
        "cancelled internal backlog entered the next generation"
    );
}
