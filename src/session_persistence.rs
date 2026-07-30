//! 会话持久化：记录每个标签页的工作目录与活动索引，在重启后恢复。
//! 端口自 jterm2 `session_persistence.rs`，精简为 jterm3 实际需要的字段。
use jterm_core::snapshot_file;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 读取快照时的上限。一份快照最多 32 个会话（`MAX_RESTORED_SESSIONS`）的 cwd
/// 加每个标签页一棵窗格树，实测是几 KB，所以 1 MiB 留了三个数量级的余量。
/// 有上限本身才是重点：这个文件是启动时按配置路径读进来交给 serde_json 的，
/// 无上限的 `read_to_string` 会先把一个被撑大的文件整份读进内存，才有机会拒绝它。
const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024;

/// `load` 的结果。必须把“没有快照”和“快照读不动”分开：后者的字节还在磁盘上，
/// 而恢复失败之后几秒内定时自动保存就会覆盖同一个路径，所以调用方要先隔离。
pub enum SnapshotLoad {
    /// 路径上没有快照（首次启动，或用户自己删了）。
    Missing,
    /// Boxed：快照本体比另外两个 variant 大一个数量级，而这个枚举在启动时
    /// 只会构造一次，多一次间接寻址换的是每个调用点都不用搬 240 字节。
    Loaded(Box<SessionsSnapshot>),
    /// 文件存在但读不出来或解析不了；字节仍在原处等待隔离。
    Unreadable(String),
}

/// 单个会话快照（jterm3 仅需要 cwd 来重新 spawn）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub cwd: Option<String>,
}

/// 分屏布局快照:重启后恢复分屏方向、各 pane 占比与对应的会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitSnapshot {
    /// "vertical"(左右)或 "horizontal"(上下)。
    pub mode: String,
    /// 每个 pane 的占比(与 `panes` 一一对应,总和约为 1)。
    /// 缺失或长度不符时恢复端回退为均分。
    #[serde(default)]
    pub ratios: Vec<f32>,
    /// 各 pane 对应的会话索引(指向 `sessions`)。
    pub panes: Vec<usize>,
    /// 拥有键盘焦点的 pane(索引进 `panes`)。
    pub focused: usize,
}

/// tmux 风格的递归分屏布局快照。`Leaf` 显示一个会话;`Split` 沿某轴划分若干
/// 子节点。旧的扁平 `SplitSnapshot` 仍可读取(见 `split` 字段),但新布局写入
/// 此字段以支持任意嵌套。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneTreeSnapshot {
    /// 叶子:指向 `sessions` 的会话索引。
    Leaf { session: usize },
    /// 分裂:`axis` 为 "vertical"(左右)或 "horizontal"(上下)。
    Split {
        axis: String,
        #[serde(default)]
        ratios: Vec<f32>,
        children: Vec<PaneTreeSnapshot>,
    },
}

/// 一个标签页的窗格树快照。窗格归标签页所有，所以布局必须按标签页存。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabSnapshot {
    pub tree: PaneTreeSnapshot,
    /// 该标签页中拥有键盘焦点的窗格所显示的会话索引。标签页标题、激活时
    /// 恢复的焦点都以它为准。
    #[serde(default)]
    pub focus: Option<usize>,
}

/// 会话列表快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSnapshot {
    pub version: u32,
    pub sessions: Vec<SessionSnapshot>,
    #[serde(default)]
    pub active_index: Option<usize>,
    /// 旧的扁平分屏布局(单轴)。仅用于读取旧快照,新快照不再写入。
    #[serde(default)]
    pub split: Option<SplitSnapshot>,
    /// v1 的全局单棵布局树。仅用于读取旧快照并迁移成第一个标签页；新快照
    /// 仍会写出当前标签页的树，好让旧版本至少能开出用户最后看到的那组窗格。
    #[serde(default)]
    pub tree: Option<PaneTreeSnapshot>,
    /// v2：每个标签页一棵树。空表示旧快照，由 `tree` 迁移。
    #[serde(default)]
    pub tabs: Vec<TabSnapshot>,
    #[serde(default)]
    pub active_tab: Option<usize>,
}

impl SessionsSnapshot {
    pub fn new(
        sessions: Vec<SessionSnapshot>,
        active_index: Option<usize>,
        tabs: Vec<TabSnapshot>,
        active_tab: Option<usize>,
    ) -> Self {
        // 兼容字段：给旧版本当前标签页的树，而不是一个空布局。
        let tree = active_tab
            .and_then(|idx| tabs.get(idx))
            .or_else(|| tabs.first())
            .map(|tab| tab.tree.clone());
        SessionsSnapshot {
            version: 2,
            sessions,
            active_index,
            split: None,
            tree,
            tabs,
            active_tab,
        }
    }

    /// 序列化为 JSON 字符串（也用于变更去重）。
    pub fn to_json(&self) -> Option<String> {
        serde_json::to_string_pretty(self).ok()
    }

    /// 原子写入到文件。
    ///
    /// 之前这里是 `fs::write` + `rename`，注释却写着“原子写入”：临时文件没有
    /// `sync_all`，父目录也没有 fsync，所以掉电后 rename 可能已经生效而内容还没
    /// 落盘，重启后读到的是一份被截断的快照。`write_atomic_private` 连带把目录
    /// 建成 0700、文件 0600 —— 快照里每个 pane 的 cwd 就是用户文件系统的地图。
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string_pretty(self)?;
        snapshot_file::write_atomic_private(path, json.as_bytes())?;
        Ok(())
    }

    /// 从文件加载。文件不存在与文件读不动是两种结果，见 [`SnapshotLoad`]。
    pub fn load(path: &Path) -> SnapshotLoad {
        if !path.exists() {
            return SnapshotLoad::Missing;
        }
        let content = match snapshot_file::read_bounded(path, MAX_SNAPSHOT_BYTES) {
            Ok(content) => content,
            // 和 exists() 之间存在竞争：文件刚被删掉就当成没有快照，否则调用方
            // 会去隔离一个已经不在那里的文件。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return SnapshotLoad::Missing
            }
            Err(error) => return SnapshotLoad::Unreadable(error.to_string()),
        };
        match serde_json::from_str(&content) {
            Ok(snapshot) => SnapshotLoad::Loaded(Box::new(snapshot)),
            Err(error) => SnapshotLoad::Unreadable(error.to_string()),
        }
    }
}

/// 尝试获取单实例锁。成功返回持锁的 `File`（需在进程生命周期内持有），
/// 失败（已有实例运行）返回 `None`。端口自 jterm2 `try_acquire_instance_lock`。
pub fn try_acquire_instance_lock() -> Option<std::fs::File> {
    let lock_path = dirs::config_dir()?.join("jterm3").join("instance.lock");
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // PTY children are fork/exec'd from this process. Without CLOEXEC they
        // inherit the flock and can make jterm3 look permanently running after
        // the UI exits.
        .custom_flags(libc::O_CLOEXEC)
        .open(&lock_path)
        .ok()?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // LOCK_EX | LOCK_NB: 非阻塞排他锁。fd 来自有效的 File，生命周期覆盖本次调用。
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        use std::io::{Seek, Write};
        // Truncate only after owning the lock. Opening with truncate(true)
        // allowed a second instance to erase the first instance's PID even
        // though its flock attempt subsequently failed.
        let _ = file.set_len(0);
        let mut f = &file;
        let _ = f.seek(std::io::SeekFrom::Start(0));
        let _ = write!(f, "{}", std::process::id());
        Some(file)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("jterm3-snapshot-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_missing_snapshot_is_not_reported_as_unreadable() {
        let root = scratch("missing");
        assert!(matches!(
            SessionsSnapshot::load(&root.join("session_history.json")),
            SnapshotLoad::Missing
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The distinction the restore path depends on: a corrupt snapshot must be
    /// `Unreadable`, not silently equivalent to "no snapshot", or the caller
    /// cannot know it has something to quarantine before the next autosave.
    #[test]
    fn a_corrupt_snapshot_is_unreadable_and_left_on_disk_for_quarantine() {
        let root = scratch("corrupt");
        let path = root.join("session_history.json");
        std::fs::write(&path, b"{\"version\":2,\"sessions\":[{\"cwd\"").unwrap();

        assert!(matches!(
            SessionsSnapshot::load(&path),
            SnapshotLoad::Unreadable(_)
        ));
        assert!(path.exists(), "load must not consume the evidence");

        let backup = snapshot_file::quarantine_corrupt(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"{\"version\":2,\"sessions\":[{\"cwd\""
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_oversized_snapshot_is_rejected_rather_than_read() {
        let root = scratch("oversize");
        let path = root.join("session_history.json");
        // Valid JSON, so only the size bound can reject it.
        let padding = "x".repeat(MAX_SNAPSHOT_BYTES as usize);
        std::fs::write(
            &path,
            format!("{{\"version\":2,\"sessions\":[],\"pad\":\"{padding}\"}}"),
        )
        .unwrap();

        assert!(matches!(
            SessionsSnapshot::load(&path),
            SnapshotLoad::Unreadable(_)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn save_round_trips_and_leaves_the_snapshot_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch("save");
        // A directory level that does not exist yet, so save has to create it.
        let path = root.join("state").join("session_history.json");
        let snapshot = SessionsSnapshot::new(
            vec![SessionSnapshot {
                cwd: Some("/tmp".to_string()),
            }],
            Some(0),
            Vec::new(),
            None,
        );
        snapshot.save(&path).unwrap();

        let SnapshotLoad::Loaded(back) = SessionsSnapshot::load(&path) else {
            panic!("a snapshot this app just wrote must load");
        };
        assert_eq!(back.sessions.len(), 1);
        assert_eq!(back.sessions[0].cwd.as_deref(), Some("/tmp"));

        // The cwd of every pane is a map of the user's filesystem.
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        // The old fs::write + rename left a `session_history.json.tmp` behind on
        // any failure; nothing but the snapshot may remain.
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshots_without_split_field_still_deserialize() {
        let legacy = r#"{"version":1,"sessions":[{"cwd":"/tmp"}],"active_index":0}"#;
        let snap: SessionsSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(snap.split.is_none());
        assert_eq!(snap.sessions.len(), 1);
    }

    #[test]
    fn tree_layout_round_trips_through_json() {
        // V[ 0, H[2, 1] ] — a genuinely nested tmux-style layout.
        let snap = SessionsSnapshot::new(
            vec![
                SessionSnapshot { cwd: None },
                SessionSnapshot { cwd: None },
                SessionSnapshot { cwd: None },
            ],
            Some(1),
            vec![TabSnapshot {
                tree: PaneTreeSnapshot::Split {
                    axis: "vertical".to_string(),
                    ratios: vec![0.6, 0.4],
                    children: vec![
                        PaneTreeSnapshot::Leaf { session: 0 },
                        PaneTreeSnapshot::Split {
                            axis: "horizontal".to_string(),
                            ratios: vec![0.5, 0.5],
                            children: vec![
                                PaneTreeSnapshot::Leaf { session: 2 },
                                PaneTreeSnapshot::Leaf { session: 1 },
                            ],
                        },
                    ],
                },
                focus: Some(1),
            }],
            Some(0),
        );
        let json = snap.to_json().unwrap();
        let back: SessionsSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tabs.len(), 1);
        assert_eq!(back.tabs[0].focus, Some(1));
        assert_eq!(back.active_tab, Some(0));
        let PaneTreeSnapshot::Split { axis, children, .. } = back.tabs[0].tree.clone() else {
            panic!("expected a split at the root");
        };
        assert_eq!(axis, "vertical");
        assert_eq!(children.len(), 2);
        assert!(matches!(children[0], PaneTreeSnapshot::Leaf { session: 0 }));
        assert!(matches!(children[1], PaneTreeSnapshot::Split { .. }));
        // The compat field still carries the active tab's tree for old builds.
        assert!(back.tree.is_some());
    }

    #[test]
    fn a_v1_snapshot_has_no_tabs_and_migrates_from_tree() {
        let legacy = r#"{"version":1,"sessions":[{"cwd":null},{"cwd":null}],
            "active_index":1,
            "tree":{"kind":"split","axis":"vertical","ratios":[0.5,0.5],
                "children":[{"kind":"leaf","session":0},{"kind":"leaf","session":1}]}}"#;
        let snap: SessionsSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(snap.tabs.is_empty());
        assert!(snap.active_tab.is_none());
        // The restore path turns this single tree into the first tab.
        assert!(snap.tree.is_some());
    }

    #[test]
    fn legacy_flat_split_field_still_deserializes() {
        // Old jterm3 snapshots stored a single-axis `split` and no `tree`. Both
        // fields must round-trip so the restore path can fall back to `split`.
        let legacy = r#"{"version":1,"sessions":[{"cwd":null},{"cwd":null}],
            "active_index":0,
            "split":{"mode":"vertical","ratios":[0.35,0.65],"panes":[0,1],"focused":0}}"#;
        let snap: SessionsSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(snap.tree.is_none());
        let split = snap.split.unwrap();
        assert_eq!(split.panes, vec![0, 1]);
        assert_eq!(split.mode, "vertical");
    }
}
