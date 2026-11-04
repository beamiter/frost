//! Eva agent handoffs through Frost's own task reducers. No coordinator,
//! agent or worktree: the handoff is injected where the Eva host delivers it,
//! and the composer is closed through the real `TaskComposerClose` reducer.
use super::*;
use std::path::Path;
use std::time::{Duration, Instant};

const CHILD: &str = "FROST_EVA_HANDOFF_TEST_CHILD";
const TEST: &str =
    "eva_handoff_tests::eva_handoff_opens_an_ordinary_visible_draft_and_closing_abandons_it_once";

struct App(Frost);
impl Drop for App {
    fn drop(&mut self) {
        for session in &mut self.0.sessions {
            if session.pty.is_alive() {
                let pid = session.pty.get_child_pid();
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut alive = false;
            for session in &mut self.0.sessions {
                alive |= session.pty.is_alive();
            }
            if !alive || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn handoff(op: &str) -> companion_host::AgentHandoff {
    companion_host::AgentHandoff {
        operation_id: op.into(),
        context: companion_host::ContextRef {
            host_id: "frost".into(),
            pane_id: "pane-1".into(),
            task_id: Some("build".into()),
            project_id: Some("alpha".into()),
            revision: 1,
        },
        title: "找出 build 失败原因".into(),
        goal: "找出 build 失败原因并给出最小修复\n\nEva 的说明：没有附带终端输出。".into(),
    }
}

fn run_child() {
    let config = Config {
        shell: Some("/bin/sh".into()),
        restore_session: false,
        ascii_organism_enabled: false,
        eva_companion_enabled: false,
        experimental_task_sidebar: false,
        ..Config::default()
    };
    let (app, _startup) = Frost::new(
        config,
        None,
        None,
        Arc::new(parking_lot::Mutex::new(
            native_enter::NativeEnterOwnership::default(),
        )),
    );
    let mut app = App(app);
    let app = &mut app.0;
    app.is_first_instance = false;
    app.focused = true;

    assert_eq!(
        app.open_eva_handoff(handoff("op-off")),
        Err("agent_tasks_disabled")
    );
    assert!(!app.task_composer_visible());
    app.config.experimental_task_sidebar = true;
    app.open_eva_handoff(handoff("op-1")).unwrap();
    assert!(
        app.task_composer_visible(),
        "the composer is shown, not just filled"
    );
    let composer = app.task_panel.composer.as_ref().unwrap();
    assert_eq!(composer.title, "找出 build 失败原因");
    assert!(composer.goal.text().contains("没有附带终端输出"));
    assert_eq!(composer.eva_operation.as_deref(), Some("op-1"));
    assert!(
        composer.validation.is_empty(),
        "validation is the user's choice"
    );
    assert!(
        app.task_manager.tasks().is_empty() && app.task_panel.pending_creation.is_none(),
        "opening a handoff creates nothing"
    );
    assert_eq!(
        app.open_eva_handoff(handoff("op-2")),
        Err("task_draft_in_progress"),
        "a second handoff never replaces the draft"
    );
    assert_eq!(app.take_abandoned_eva_handoff(), None, "still open");

    // A close from a stale widget is ignored; the current one closes the draft.
    let _ = app.update(Message::TaskComposerClose(Arc::new(())));
    assert!(app.task_composer_visible());
    let reference = app.task_panel.composer.as_ref().unwrap().reference();
    let _ = app.update(Message::TaskComposerClose(reference));
    assert!(!app.task_composer_visible());
    assert_eq!(app.take_abandoned_eva_handoff().as_deref(), Some("op-1"));
    assert_eq!(app.take_abandoned_eva_handoff(), None, "reported once");

    // Reopened, it is the user's ordinary kept draft, no longer linked to Eva.
    app.task_composer_open();
    let composer = app.task_panel.composer.as_ref().unwrap();
    assert_eq!(composer.title, "找出 build 失败原因");
    assert!(composer.eva_operation.is_none());
}

// Run in a subprocess rather than mutating HOME/XDG in the parallel test runner.
#[test]
fn eva_handoff_opens_an_ordinary_visible_draft_and_closing_abandons_it_once() {
    if std::env::var_os(CHILD).is_some() {
        run_child();
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "frost-eva-handoff-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, "1")
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", &root)
        .env_remove("EVA_COMPANION_MODE")
        .current_dir(Path::new(&root))
        .status()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        status.success(),
        "isolated Eva handoff test failed: {status}"
    );
}
