use std::io::{Error, ErrorKind, Result};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::time::{Duration, Instant};

use sctk::reexports::calloop::channel::Channel;
use sctk::reexports::calloop::{EventLoop, channel};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::Connection;
use sctk::reexports::client::backend::WaylandError;
use sctk::reexports::client::globals::{registry_queue_init, registry_queue_init_external_reader};

use crate::state::{SelectionTarget, State};

/// Spawn the existing clipboard event-queue worker. External-reader mode never
/// prepares or reads the display socket, including during initialization.
pub fn spawn(
    name: String,
    display: Connection,
    rx_chan: Channel<Command>,
    worker_replier: Sender<Result<String>>,
    external: bool,
    shutdown: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            worker_impl(display, rx_chan, worker_replier, external, shutdown);
        })
        .ok()
}

pub enum Command {
    Store(String),
    StorePrimary(String),
    Load,
    LoadPrimary,
    LoadBounded(SelectionTarget, crate::bounded::Request),
    Exit,
}

fn command(command: Command, state: &mut State) {
    match command {
        Command::StorePrimary(contents) => {
            state.store_selection(SelectionTarget::Primary, contents);
        },
        Command::Store(contents) => {
            state.store_selection(SelectionTarget::Clipboard, contents);
        },
        Command::Load if state.data_device_manager_state.is_some() => {
            if let Err(error) = state.load_selection(SelectionTarget::Clipboard) {
                let _ = state.reply_tx.send(Err(error));
            }
        },
        Command::LoadPrimary if state.primary_selection_manager_state.is_some() => {
            if let Err(error) = state.load_selection(SelectionTarget::Primary) {
                let _ = state.reply_tx.send(Err(error));
            }
        },
        Command::Load | Command::LoadPrimary => {
            let _ = state.reply_tx.send(Err(Error::other("requested selection is not supported")));
        },
        Command::LoadBounded(target, request) => state.load_bounded(target, request),
        Command::Exit => state.exit = true,
    }
}

fn worker_impl(
    connection: Connection,
    rx_chan: Channel<Command>,
    reply_tx: Sender<Result<String>>,
    external: bool,
    shutdown: Arc<AtomicBool>,
) {
    let initialized = if external {
        registry_queue_init_external_reader(
            &connection,
            Instant::now() + crate::bounded::TIMEOUT,
            || shutdown.load(Ordering::Acquire),
        )
    } else {
        registry_queue_init(&connection)
    };
    let (globals, mut event_queue) = match initialized {
        Ok(value) => value,
        Err(_) => return,
    };
    let mut event_loop = match EventLoop::<State>::try_new() {
        Ok(value) => value,
        Err(_) => return,
    };
    let loop_handle = event_loop.handle();
    let mut state = match State::new(&globals, &event_queue.handle(), loop_handle.clone(), reply_tx)
    {
        Some(state) => state,
        None => return,
    };
    if loop_handle
        .insert_source(rx_chan, |event, _, state| match event {
            channel::Event::Msg(command) => state.commands.push_back(command),
            channel::Event::Closed => state.exit = true,
        })
        .is_err()
    {
        return;
    }

    if !external {
        if WaylandSource::new(connection, event_queue).insert(loop_handle).is_err() {
            return;
        }
        loop {
            if event_loop.dispatch(None, &mut state).is_err() {
                break;
            }
            while let Some(next) = state.commands.pop_front() {
                command(next, &mut state);
            }
            if state.exit {
                break;
            }
        }
        return;
    }

    let initial_seat_deadline = Instant::now() + crate::bounded::TIMEOUT;
    loop {
        if shutdown.load(Ordering::Acquire) || state.exit {
            break;
        }
        // Explicitly supported guest dispatch: host winit reads the socket.
        // Drain pending focus/offer changes BEFORE admitting new read commands.
        if event_queue.dispatch_pending(&mut state).is_err() {
            break;
        }
        while let Some(next) = state.commands.pop_front() {
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            command(next, &mut state);
            if state.exit {
                break;
            }
        }
        if shutdown.load(Ordering::Acquire) || state.exit {
            break;
        }
        state.retire_bounded();
        let needs_flush = match connection.flush() {
            Ok(()) => false,
            Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => true,
            Err(_) => break,
        };
        // Even without an owned selection, periodic pending dispatch is needed
        // to retire keyboard/pointer/seat events rather than accumulating them.
        let wait = if state.has_active_transfer()
            || needs_flush
            || (!state.has_observed_seat() && Instant::now() < initial_seat_deadline)
        {
            Duration::from_millis(10)
        } else {
            Duration::from_millis(250)
        };
        if event_loop.dispatch(Some(wait), &mut state).is_err() {
            break;
        }
    }
    state.shutdown_bounded();
}
