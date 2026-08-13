//! 会话持久化：记录每个标签页的工作目录与活动索引，在重启后恢复。
//! 端口自 ember `session_persistence.rs`，精简为 frost 实际需要的字段。
use serde::{Deserialize, Serialize};
use std::path::Path;

#[cfg(test)]
use jterm_core::snapshot_file;

/// 读取快照时的上限。一份快照最多 32 个会话（`MAX_RESTORED_SESSIONS`）的 cwd
/// 加每个标签页一棵窗格树，实测是几 KB，所以 1 MiB 留了三个数量级的余量。
/// 有上限本身才是重点：这个文件是启动时按配置路径读进来交给 serde_json 的，
/// 无上限的 `read_to_string` 会先把一个被撑大的文件整份读进内存，才有机会拒绝它。
const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024;
pub const MAX_RESTORED_SESSIONS: usize = 32;
const MAX_RESTORED_TABS: usize = MAX_RESTORED_SESSIONS;
const MAX_RESTORED_PANES_PER_TAB: usize = 12;
const MAX_RESTORED_LAYOUT_DEPTH: usize = 64;
const MAX_RESTORED_LAYOUT_NODES: usize = 64;
const MAX_RESTORED_CWD_BYTES: usize = 4096;
/// 标签页自定义标题的上限。标题只是一行标签文字，不需要更多。
pub const MAX_RESTORED_TAB_TITLE_BYTES: usize = 256;
const MAX_RESTORED_AXIS_BYTES: usize = 10;
const MAX_LEGAL_RESTORED_TEXT_BYTES: usize = MAX_RESTORED_SESSIONS * MAX_RESTORED_CWD_BYTES
    + MAX_RESTORED_TABS * MAX_RESTORED_TAB_TITLE_BYTES
    + (MAX_RESTORED_TABS + 1) * (MAX_RESTORED_PANES_PER_TAB - 1) * MAX_RESTORED_AXIS_BYTES
    + MAX_RESTORED_AXIS_BYTES;
/// Every retained string is charged before ownership. This is deliberately
/// above the maximum legal v1/v2 snapshot, while still far below the file cap.
const MAX_RESTORED_TEXT_BYTES: usize = 160 * 1024;
const _: () = assert!(MAX_LEGAL_RESTORED_TEXT_BYTES <= MAX_RESTORED_TEXT_BYTES);

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

/// 单个会话快照（frost 仅需要 cwd 来重新 spawn）。
#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub cwd: Option<String>,
}

/// 分屏布局快照:重启后恢复分屏方向、各 pane 占比与对应的会话。
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct TabSnapshot {
    pub tree: PaneTreeSnapshot,
    /// 该标签页中拥有键盘焦点的窗格所显示的会话索引。标签页标题、激活时
    /// 恢复的焦点都以它为准。
    #[serde(default)]
    pub focus: Option<usize>,
    /// 用户在右键菜单里改过的标签页标题。`None` 表示跟随焦点会话自己的标题。
    /// 旧快照没有这个字段，恢复为 `None`。
    #[serde(default)]
    pub title: Option<String>,
    /// 固定的标签页重启后仍固定，并排在最前。
    #[serde(default)]
    pub pinned: bool,
    /// 「重要」标记（多选模型）。
    #[serde(default)]
    pub marked: bool,
}

/// 会话列表快照。
#[derive(Debug, Clone, Serialize)]
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

// ---------------------------------------------------------------------------
// Allocation-bounded snapshot decoding
// ---------------------------------------------------------------------------

/// State shared by the bounded seeds. The input file has its own byte cap, but
/// ordinary derived deserialization can still turn a compact JSON array into
/// thousands of Vec elements before `sanitize` gets a chance to truncate it.
#[derive(Clone, Copy)]
struct DecodeBudget {
    remaining_text_bytes: usize,
    extra_sessions: usize,
    invalid_cwds: usize,
    invalid_titles: usize,
    invalid_tab_layouts: bool,
    invalid_legacy_tree: bool,
    invalid_legacy_split: bool,
    active_tab_repaired: bool,
}

impl DecodeBudget {
    fn new(text_bytes: usize) -> Self {
        Self {
            remaining_text_bytes: text_bytes,
            extra_sessions: 0,
            invalid_cwds: 0,
            invalid_titles: 0,
            invalid_tab_layouts: false,
            invalid_legacy_tree: false,
            invalid_legacy_split: false,
            active_tab_repaired: false,
        }
    }

    fn charge_text<E: serde::de::Error>(
        &mut self,
        field: &'static str,
        bytes: usize,
    ) -> Result<(), E> {
        let Some(remaining) = self.remaining_text_bytes.checked_sub(bytes) else {
            return Err(E::custom(format_args!(
                "session snapshot exceeds its cumulative text budget while decoding '{field}'"
            )));
        };
        self.remaining_text_bytes = remaining;
        Ok(())
    }

    fn warnings(self, restored_sessions: usize) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.extra_sessions > 0 {
            warnings.push(format!(
                "restored only the first {MAX_RESTORED_SESSIONS} of {} sessions",
                restored_sessions + self.extra_sessions
            ));
        }
        if self.invalid_cwds > 0 {
            warnings.push(format!(
                "discarded {} oversized or invalid working directories",
                self.invalid_cwds
            ));
        }
        if self.invalid_titles > 0 {
            warnings.push(format!(
                "discarded {} oversized or invalid tab titles",
                self.invalid_titles
            ));
        }
        if self.invalid_tab_layouts {
            warnings.push("discarded oversized or invalid tab layouts".to_string());
        }
        if self.invalid_legacy_tree {
            warnings.push("discarded invalid legacy pane layout".to_string());
        }
        if self.invalid_legacy_split {
            warnings.push("discarded invalid legacy split layout".to_string());
        }
        if self.active_tab_repaired {
            warnings.push("active tab index was outside the restored list".to_string());
        }
        warnings
    }
}

#[derive(Clone, Copy)]
enum SnapshotField {
    Version,
    Sessions,
    ActiveIndex,
    Split,
    Tree,
    Tabs,
    ActiveTab,
    Cwd,
    Mode,
    Ratios,
    Panes,
    Focused,
    Kind,
    Session,
    Axis,
    Children,
    Focus,
    Title,
    Pinned,
    Marked,
    Unknown,
}

impl<'de> Deserialize<'de> for SnapshotField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_identifier(SnapshotFieldVisitor)
    }
}

struct SnapshotFieldVisitor;

impl serde::de::Visitor<'_> for SnapshotFieldVisitor {
    type Value = SnapshotField;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a session snapshot field name")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(match value {
            "version" => SnapshotField::Version,
            "sessions" => SnapshotField::Sessions,
            "active_index" => SnapshotField::ActiveIndex,
            "split" => SnapshotField::Split,
            "tree" => SnapshotField::Tree,
            "tabs" => SnapshotField::Tabs,
            "active_tab" => SnapshotField::ActiveTab,
            "cwd" => SnapshotField::Cwd,
            "mode" => SnapshotField::Mode,
            "ratios" => SnapshotField::Ratios,
            "panes" => SnapshotField::Panes,
            "focused" => SnapshotField::Focused,
            "kind" => SnapshotField::Kind,
            "session" => SnapshotField::Session,
            "axis" => SnapshotField::Axis,
            "children" => SnapshotField::Children,
            "focus" => SnapshotField::Focus,
            "title" => SnapshotField::Title,
            "pinned" => SnapshotField::Pinned,
            "marked" => SnapshotField::Marked,
            _ => SnapshotField::Unknown,
        })
    }
}

/// A nullable cwd that is validated while still borrowed. Invalid cwd values
/// retain the old sanitizer behaviour: discard only that field and keep the
/// session.
struct CwdSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for CwdSeed<'_> {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_option(self)
    }
}

impl<'de> serde::de::Visitor<'de> for CwdSeed<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("null or a bounded working directory")
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(CwdValueVisitor {
            budget: self.budget,
        })
    }
}

struct CwdValueVisitor<'a> {
    budget: &'a mut DecodeBudget,
}

impl serde::de::Visitor<'_> for CwdValueVisitor<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "a working directory of at most {MAX_RESTORED_CWD_BYTES} bytes"
        )
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        if value.len() > MAX_RESTORED_CWD_BYTES || value.as_bytes().contains(&0) {
            self.budget.invalid_cwds += 1;
            return Ok(None);
        }
        self.budget.charge_text::<E>("cwd", value.len())?;
        Ok(Some(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.visit_str(&value)
    }
}

struct SessionSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for SessionSeed<'_> {
    type Value = SessionSnapshot;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for SessionSeed<'_> {
    type Value = SessionSnapshot;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded terminal session")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut cwd: Option<Option<String>> = None;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Cwd => {
                    if cwd.is_some() {
                        return Err(A::Error::duplicate_field("cwd"));
                    }
                    cwd = Some(map.next_value_seed(CwdSeed {
                        budget: self.budget,
                    })?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(SessionSnapshot {
            cwd: cwd.unwrap_or(None),
        })
    }
}

struct SessionsSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for SessionsSeed<'_> {
    type Value = Vec<SessionSnapshot>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for SessionsSeed<'_> {
    type Value = Vec<SessionSnapshot>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "at most {MAX_RESTORED_SESSIONS} sessions")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let budget = self.budget;
        let mut sessions = Vec::with_capacity(
            seq.size_hint()
                .unwrap_or(MAX_RESTORED_SESSIONS)
                .min(MAX_RESTORED_SESSIONS),
        );
        while sessions.len() < MAX_RESTORED_SESSIONS {
            let Some(session) = seq.next_element_seed(SessionSeed {
                budget: &mut *budget,
            })?
            else {
                return Ok(sessions);
            };
            sessions.push(session);
        }
        while seq.next_element_seed(DiscardSessionSeed)?.is_some() {
            budget.extra_sessions = budget.extra_sessions.saturating_add(1);
        }
        Ok(sessions)
    }
}

/// Validate sessions beyond the retained prefix without allocating their cwd.
/// Derived Serde used to reject a scalar or a duplicate/wrong-typed known field
/// even in session 33, so truncation must not silently broaden the schema.
struct DiscardSessionSeed;

impl<'de> serde::de::DeserializeSeed<'de> for DiscardSessionSeed {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for DiscardSessionSeed {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a terminal session")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut saw_cwd = false;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Cwd => {
                    if saw_cwd {
                        return Err(A::Error::duplicate_field("cwd"));
                    }
                    saw_cwd = true;
                    map.next_value_seed(DiscardOptionalStringSeed)?;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

struct DiscardOptionalStringSeed;

impl<'de> serde::de::DeserializeSeed<'de> for DiscardOptionalStringSeed {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_option(self)
    }
}

impl<'de> serde::de::Visitor<'de> for DiscardOptionalStringSeed {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("null or a string")
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(DiscardStringVisitor)
    }
}

struct DiscardStringVisitor;

impl serde::de::Visitor<'_> for DiscardStringVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E: serde::de::Error>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }
}

/// Raw fields are borrowed from the input so a deeply nested optional layout
/// is not cloned once per ancestor. The duplicate bit lets `null` still count
/// as a present field.
#[derive(Default)]
struct DeferredRawField<'de> {
    value: Option<&'de serde_json::value::RawValue>,
    duplicate: bool,
}

impl<'de> DeferredRawField<'de> {
    fn read<A: serde::de::MapAccess<'de>>(&mut self, map: &mut A) -> Result<(), A::Error> {
        if self.value.is_some() {
            self.duplicate = true;
            map.next_value::<serde::de::IgnoredAny>()?;
        } else {
            self.value = Some(map.next_value::<&'de serde_json::value::RawValue>()?);
        }
        Ok(())
    }

    fn required<E: serde::de::Error>(
        self,
        field: &'static str,
    ) -> Result<&'de serde_json::value::RawValue, E> {
        if self.duplicate {
            return Err(E::duplicate_field(field));
        }
        self.value.ok_or_else(|| E::missing_field(field))
    }

    fn optional<E: serde::de::Error>(
        self,
        field: &'static str,
    ) -> Result<Option<&'de serde_json::value::RawValue>, E> {
        if self.duplicate {
            return Err(E::duplicate_field(field));
        }
        Ok(self.value)
    }
}

struct RawSessionsSnapshot<'de> {
    version: u32,
    sessions: &'de serde_json::value::RawValue,
    active_index: Option<usize>,
    split: Option<&'de serde_json::value::RawValue>,
    tree: Option<&'de serde_json::value::RawValue>,
    tabs: Option<&'de serde_json::value::RawValue>,
    active_tab: Option<usize>,
}

struct RawSessionsSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RawSessionsSeed {
    type Value = RawSessionsSnapshot<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawSessionsSeed {
    type Value = RawSessionsSnapshot<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a versioned sessions snapshot")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut version = None;
        let mut sessions = DeferredRawField::default();
        let mut active_index: Option<Option<usize>> = None;
        let mut split = DeferredRawField::default();
        let mut tree = DeferredRawField::default();
        let mut tabs = DeferredRawField::default();
        let mut active_tab: Option<Option<usize>> = None;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Version => {
                    if version.is_some() {
                        return Err(A::Error::duplicate_field("version"));
                    }
                    let decoded = map.next_value::<u32>()?;
                    if !(1..=2).contains(&decoded) {
                        return Err(A::Error::custom(format_args!(
                            "unsupported session snapshot version {decoded}"
                        )));
                    }
                    version = Some(decoded);
                }
                SnapshotField::Sessions => sessions.read(&mut map)?,
                SnapshotField::ActiveIndex => {
                    if active_index.is_some() {
                        return Err(A::Error::duplicate_field("active_index"));
                    }
                    active_index = Some(map.next_value::<Option<usize>>()?);
                }
                SnapshotField::Split => split.read(&mut map)?,
                SnapshotField::Tree => tree.read(&mut map)?,
                SnapshotField::Tabs => tabs.read(&mut map)?,
                SnapshotField::ActiveTab => {
                    if active_tab.is_some() {
                        return Err(A::Error::duplicate_field("active_tab"));
                    }
                    active_tab = Some(map.next_value::<Option<usize>>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(RawSessionsSnapshot {
            version: version.ok_or_else(|| A::Error::missing_field("version"))?,
            sessions: sessions.required::<A::Error>("sessions")?,
            active_index: active_index.unwrap_or(None),
            split: split.optional::<A::Error>("split")?,
            tree: tree.optional::<A::Error>("tree")?,
            tabs: tabs.optional::<A::Error>("tabs")?,
            active_tab: active_tab.unwrap_or(None),
        })
    }
}

struct AxisSeed<'a> {
    budget: &'a mut DecodeBudget,
    field: &'static str,
}

impl<'de> serde::de::DeserializeSeed<'de> for AxisSeed<'_> {
    type Value = String;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl serde::de::Visitor<'_> for AxisSeed<'_> {
    type Value = String;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("'vertical' or 'horizontal'")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        if !matches!(value, "vertical" | "horizontal") {
            return Err(E::unknown_variant(value, &["vertical", "horizontal"]));
        }
        self.budget.charge_text::<E>(self.field, value.len())?;
        Ok(value.to_owned())
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.visit_str(&value)
    }
}

struct RatiosSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RatiosSeed {
    type Value = Vec<f32>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RatiosSeed {
    type Value = Vec<f32>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "at most {MAX_RESTORED_PANES_PER_TAB} retained pane ratios"
        )
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut ratios = Vec::with_capacity(
            seq.size_hint()
                .unwrap_or(MAX_RESTORED_PANES_PER_TAB)
                .min(MAX_RESTORED_PANES_PER_TAB),
        );
        while ratios.len() < MAX_RESTORED_PANES_PER_TAB {
            let Some(ratio) = seq.next_element::<f32>()? else {
                return Ok(ratios);
            };
            ratios.push(ratio);
        }
        // The old sanitizer silently truncated surplus ratios to the pane
        // count. Preserve that compatibility without constructing them.
        while seq.next_element::<f32>()?.is_some() {}
        Ok(ratios)
    }
}

struct PanesSeed;

impl<'de> serde::de::DeserializeSeed<'de> for PanesSeed {
    type Value = Vec<usize>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for PanesSeed {
    type Value = Vec<usize>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "at most {MAX_RESTORED_PANES_PER_TAB} pane indexes"
        )
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut panes = Vec::with_capacity(
            seq.size_hint()
                .unwrap_or(MAX_RESTORED_PANES_PER_TAB)
                .min(MAX_RESTORED_PANES_PER_TAB),
        );
        while panes.len() < MAX_RESTORED_PANES_PER_TAB {
            let Some(pane) = seq.next_element::<usize>()? else {
                return Ok(panes);
            };
            panes.push(pane);
        }
        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(A::Error::custom(format_args!(
                "legacy split exceeds its {MAX_RESTORED_PANES_PER_TAB}-pane limit"
            )));
        }
        Ok(panes)
    }
}

struct SplitSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for SplitSeed<'_> {
    type Value = SplitSnapshot;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for SplitSeed<'_> {
    type Value = SplitSnapshot;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded legacy split layout")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut mode = None;
        let mut ratios = None;
        let mut panes = None;
        let mut focused = None;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Mode => {
                    if mode.is_some() {
                        return Err(A::Error::duplicate_field("mode"));
                    }
                    mode = Some(map.next_value_seed(AxisSeed {
                        budget: self.budget,
                        field: "legacy split mode",
                    })?);
                }
                SnapshotField::Ratios => {
                    if ratios.is_some() {
                        return Err(A::Error::duplicate_field("ratios"));
                    }
                    ratios = Some(map.next_value_seed(RatiosSeed)?);
                }
                SnapshotField::Panes => {
                    if panes.is_some() {
                        return Err(A::Error::duplicate_field("panes"));
                    }
                    panes = Some(map.next_value_seed(PanesSeed)?);
                }
                SnapshotField::Focused => {
                    if focused.is_some() {
                        return Err(A::Error::duplicate_field("focused"));
                    }
                    focused = Some(map.next_value::<usize>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(SplitSnapshot {
            mode: mode.ok_or_else(|| A::Error::missing_field("mode"))?,
            ratios: ratios.unwrap_or_default(),
            panes: panes.ok_or_else(|| A::Error::missing_field("panes"))?,
            focused: focused.ok_or_else(|| A::Error::missing_field("focused"))?,
        })
    }
}

#[derive(Clone, Copy)]
enum TreeKind {
    Leaf,
    Split,
}

struct TreeKindSeed;

impl<'de> serde::de::DeserializeSeed<'de> for TreeKindSeed {
    type Value = TreeKind;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl serde::de::Visitor<'_> for TreeKindSeed {
    type Value = TreeKind;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("'leaf' or 'split'")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        match value {
            "leaf" => Ok(TreeKind::Leaf),
            "split" => Ok(TreeKind::Split),
            other => Err(E::unknown_variant(other, &["leaf", "split"])),
        }
    }
}

struct TreeBudget {
    nodes: usize,
    leaves: usize,
}

impl TreeBudget {
    fn new() -> Self {
        Self {
            nodes: 0,
            leaves: 0,
        }
    }
}

struct RawTreeNode<'de> {
    kind: TreeKind,
    session: DeferredRawField<'de>,
    axis: DeferredRawField<'de>,
    ratios: DeferredRawField<'de>,
    children: DeferredRawField<'de>,
}

struct RawTreeNodeSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RawTreeNodeSeed {
    type Value = RawTreeNode<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawTreeNodeSeed {
    type Value = RawTreeNode<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded pane-tree node")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut kind = None;
        let mut session = DeferredRawField::default();
        let mut axis = DeferredRawField::default();
        let mut ratios = DeferredRawField::default();
        let mut children = DeferredRawField::default();
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Kind => {
                    if kind.is_some() {
                        return Err(A::Error::duplicate_field("kind"));
                    }
                    kind = Some(map.next_value_seed(TreeKindSeed)?);
                }
                SnapshotField::Session => session.read(&mut map)?,
                SnapshotField::Axis => axis.read(&mut map)?,
                SnapshotField::Ratios => ratios.read(&mut map)?,
                SnapshotField::Children => children.read(&mut map)?,
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }

        Ok(RawTreeNode {
            kind: kind.ok_or_else(|| A::Error::missing_field("kind"))?,
            session,
            axis,
            ratios,
            children,
        })
    }
}

struct RawChildrenSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RawChildrenSeed {
    type Value = Vec<&'de serde_json::value::RawValue>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawChildrenSeed {
    type Value = Vec<&'de serde_json::value::RawValue>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "at most {MAX_RESTORED_PANES_PER_TAB} pane-tree children"
        )
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut children = Vec::with_capacity(
            seq.size_hint()
                .unwrap_or(MAX_RESTORED_PANES_PER_TAB)
                .min(MAX_RESTORED_PANES_PER_TAB),
        );
        while children.len() < MAX_RESTORED_PANES_PER_TAB {
            let Some(raw) = seq.next_element::<&'de serde_json::value::RawValue>()? else {
                return Ok(children);
            };
            children.push(raw);
        }
        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(A::Error::custom(format_args!(
                "pane split exceeds its {MAX_RESTORED_PANES_PER_TAB}-child limit"
            )));
        }
        Ok(children)
    }
}

fn decode_axis(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
    field: &'static str,
) -> Result<String, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let axis =
        serde::de::DeserializeSeed::deserialize(AxisSeed { budget, field }, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    Ok(axis)
}

fn decode_ratios(raw: &serde_json::value::RawValue) -> Result<Vec<f32>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let ratios = serde::de::DeserializeSeed::deserialize(RatiosSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    Ok(ratios)
}

fn decode_raw_children(
    raw: &serde_json::value::RawValue,
) -> Result<Vec<&serde_json::value::RawValue>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let children = serde::de::DeserializeSeed::deserialize(RawChildrenSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    Ok(children)
}

fn decode_tree_node(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
    tree_budget: &mut TreeBudget,
    depth: usize,
) -> Result<PaneTreeSnapshot, serde_json::Error> {
    if depth > MAX_RESTORED_LAYOUT_DEPTH {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            format_args!("pane tree exceeds its {MAX_RESTORED_LAYOUT_DEPTH}-level depth limit"),
        ));
    }
    if tree_budget.nodes >= MAX_RESTORED_LAYOUT_NODES {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            format_args!("pane tree exceeds its {MAX_RESTORED_LAYOUT_NODES}-node limit"),
        ));
    }
    tree_budget.nodes += 1;

    // Finish and drop this parser before following any child. serde_json keeps
    // a scratch buffer while skipping RawValue contents; recursing from inside
    // the visitor would retain one near-file-sized buffer per ancestor.
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let staged = serde::de::DeserializeSeed::deserialize(RawTreeNodeSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);

    match staged.kind {
        TreeKind::Leaf => {
            let session = serde_json::from_str::<usize>(
                staged
                    .session
                    .required::<serde_json::Error>("session")?
                    .get(),
            )?;
            if tree_budget.leaves >= MAX_RESTORED_PANES_PER_TAB {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    format_args!("pane tree exceeds its {MAX_RESTORED_PANES_PER_TAB}-pane limit"),
                ));
            }
            tree_budget.leaves += 1;
            Ok(PaneTreeSnapshot::Leaf { session })
        }
        TreeKind::Split => {
            let axis = decode_axis(
                staged.axis.required::<serde_json::Error>("axis")?,
                budget,
                "pane tree axis",
            )?;
            let ratios = staged
                .ratios
                .optional::<serde_json::Error>("ratios")?
                .map(decode_ratios)
                .transpose()?
                .unwrap_or_default();
            let raw_children =
                decode_raw_children(staged.children.required::<serde_json::Error>("children")?)?;
            if !(2..=MAX_RESTORED_PANES_PER_TAB).contains(&raw_children.len()) {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    format_args!(
                        "pane split must contain 2..={MAX_RESTORED_PANES_PER_TAB} children"
                    ),
                ));
            }
            let mut children = Vec::with_capacity(raw_children.len());
            for child in raw_children {
                children.push(decode_tree_node(child, budget, tree_budget, depth + 1)?);
            }
            Ok(PaneTreeSnapshot::Split {
                axis,
                ratios,
                children,
            })
        }
    }
}

fn decode_tree(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<PaneTreeSnapshot, serde_json::Error> {
    decode_tree_node(raw, budget, &mut TreeBudget::new(), 0)
}

/// Optional title validation mirrors `sanitize`: invalid text becomes `None`
/// and does not invalidate an otherwise usable tab.
struct TitleSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for TitleSeed<'_> {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_option(self)
    }
}

impl<'de> serde::de::Visitor<'de> for TitleSeed<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("null or a bounded tab title")
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(TitleValueVisitor {
            budget: self.budget,
        })
    }
}

struct TitleValueVisitor<'a> {
    budget: &'a mut DecodeBudget,
}

impl serde::de::Visitor<'_> for TitleValueVisitor<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "a tab title of at most {MAX_RESTORED_TAB_TITLE_BYTES} bytes"
        )
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        if value.trim().is_empty()
            || value.len() > MAX_RESTORED_TAB_TITLE_BYTES
            || value.chars().any(char::is_control)
        {
            self.budget.invalid_titles += 1;
            return Ok(None);
        }
        self.budget.charge_text::<E>("tab title", value.len())?;
        Ok(Some(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.visit_str(&value)
    }
}

struct RawTab<'de> {
    tree: &'de serde_json::value::RawValue,
    focus: Option<usize>,
    title: Option<String>,
    pinned: bool,
    marked: bool,
}

struct RawTabSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for RawTabSeed<'_> {
    type Value = RawTab<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawTabSeed<'_> {
    type Value = RawTab<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded tab snapshot")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut tree = DeferredRawField::default();
        let mut focus: Option<Option<usize>> = None;
        let mut title: Option<Option<String>> = None;
        let mut pinned = None;
        let mut marked = None;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Tree => tree.read(&mut map)?,
                SnapshotField::Focus => {
                    if focus.is_some() {
                        return Err(A::Error::duplicate_field("focus"));
                    }
                    focus = Some(map.next_value::<Option<usize>>()?);
                }
                SnapshotField::Title => {
                    if title.is_some() {
                        return Err(A::Error::duplicate_field("title"));
                    }
                    title = Some(map.next_value_seed(TitleSeed {
                        budget: self.budget,
                    })?);
                }
                SnapshotField::Pinned => {
                    if pinned.is_some() {
                        return Err(A::Error::duplicate_field("pinned"));
                    }
                    pinned = Some(map.next_value::<bool>()?);
                }
                SnapshotField::Marked => {
                    if marked.is_some() {
                        return Err(A::Error::duplicate_field("marked"));
                    }
                    marked = Some(map.next_value::<bool>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(RawTab {
            tree: tree.required::<A::Error>("tree")?,
            focus: focus.unwrap_or(None),
            title: title.unwrap_or(None),
            pinned: pinned.unwrap_or(false),
            marked: marked.unwrap_or(false),
        })
    }
}

struct RawTabs<'de> {
    tabs: Vec<&'de serde_json::value::RawValue>,
    truncated: bool,
}

struct RawTabsSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RawTabsSeed {
    type Value = RawTabs<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawTabsSeed {
    type Value = RawTabs<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "at most {MAX_RESTORED_TABS} tab snapshots")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut tabs = Vec::with_capacity(
            seq.size_hint()
                .unwrap_or(MAX_RESTORED_TABS)
                .min(MAX_RESTORED_TABS),
        );
        let mut input_count = 0;
        while input_count < MAX_RESTORED_TABS {
            let Some(raw) = seq.next_element::<&'de serde_json::value::RawValue>()? else {
                return Ok(RawTabs {
                    tabs,
                    truncated: false,
                });
            };
            input_count += 1;
            tabs.push(raw);
        }
        let mut truncated = false;
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            truncated = true;
        }
        Ok(RawTabs { tabs, truncated })
    }
}

fn decode_tab(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<TabSnapshot, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let staged = serde::de::DeserializeSeed::deserialize(RawTabSeed { budget }, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    let tree = decode_tree(staged.tree, budget)?;
    Ok(TabSnapshot {
        tree,
        focus: staged.focus,
        title: staged.title,
        pinned: staged.pinned,
        marked: staged.marked,
    })
}

struct DecodedTabs {
    tabs: Vec<TabSnapshot>,
    retained_input_indices: Vec<usize>,
}

fn decode_tabs(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<DecodedTabs, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let staged = serde::de::DeserializeSeed::deserialize(RawTabsSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    if staged.truncated {
        budget.invalid_tab_layouts = true;
    }
    let mut tabs = Vec::with_capacity(staged.tabs.len());
    let mut retained_input_indices = Vec::with_capacity(staged.tabs.len());
    for (input_index, raw_tab) in staged.tabs.into_iter().enumerate() {
        let before = *budget;
        match decode_tab(raw_tab, budget) {
            Ok(tab) => {
                tabs.push(tab);
                retained_input_indices.push(input_index);
            }
            Err(_) => {
                *budget = before;
                budget.invalid_tab_layouts = true;
            }
        }
    }
    Ok(DecodedTabs {
        tabs,
        retained_input_indices,
    })
}

fn remap_active_tab(active: Option<usize>, tabs: &DecodedTabs) -> (Option<usize>, bool) {
    let Some(active) = active else {
        return (None, false);
    };
    if tabs.tabs.is_empty() {
        return (None, true);
    }
    let retained = tabs
        .retained_input_indices
        .iter()
        .position(|index| *index == active);
    if let Some(retained) = retained {
        return (Some(retained), false);
    }
    let fallback = tabs
        .retained_input_indices
        .iter()
        .position(|index| *index > active)
        .or_else(|| tabs.tabs.len().checked_sub(1));
    (fallback, true)
}

fn decode_sessions(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<Vec<SessionSnapshot>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let sessions =
        serde::de::DeserializeSeed::deserialize(SessionsSeed { budget }, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    Ok(sessions)
}

fn decode_split(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<SplitSnapshot, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let split = serde::de::DeserializeSeed::deserialize(SplitSeed { budget }, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    Ok(split)
}

fn raw_is_null(raw: &serde_json::value::RawValue) -> bool {
    raw.get().trim() == "null"
}

fn decode_bounded_snapshot_with_text_budget(
    content: &str,
    text_budget: usize,
) -> Result<(SessionsSnapshot, Vec<String>), serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(content);
    let raw = serde::de::DeserializeSeed::deserialize(RawSessionsSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);

    // Validate the envelope before decoding any owned session or layout data,
    // regardless of where `version` appeared in the JSON object.
    if !(1..=2).contains(&raw.version) {
        return Err(<serde_json::Error as serde::de::Error>::custom(
            format_args!("unsupported session snapshot version {}", raw.version),
        ));
    }

    let mut budget = DecodeBudget::new(text_budget);
    let sessions = decode_sessions(raw.sessions, &mut budget)?;
    let decoded_tabs = match raw.tabs {
        Some(raw_tabs) => {
            let before = budget;
            match decode_tabs(raw_tabs, &mut budget) {
                Ok(tabs) => tabs,
                Err(_) => {
                    budget = before;
                    budget.invalid_tab_layouts = true;
                    DecodedTabs {
                        tabs: Vec::new(),
                        retained_input_indices: Vec::new(),
                    }
                }
            }
        }
        None => DecodedTabs {
            tabs: Vec::new(),
            retained_input_indices: Vec::new(),
        },
    };
    let (active_tab, active_tab_repaired) = remap_active_tab(raw.active_tab, &decoded_tabs);
    budget.active_tab_repaired = active_tab_repaired;

    let tree = match raw.tree.filter(|raw_tree| !raw_is_null(raw_tree)) {
        Some(raw_tree) => {
            let before = budget;
            match decode_tree(raw_tree, &mut budget) {
                Ok(tree) => Some(tree),
                Err(_) => {
                    budget = before;
                    budget.invalid_legacy_tree = true;
                    None
                }
            }
        }
        None => None,
    };

    let split = match raw.split.filter(|raw_split| !raw_is_null(raw_split)) {
        Some(raw_split) => {
            let before = budget;
            match decode_split(raw_split, &mut budget) {
                Ok(split) => Some(split),
                Err(_) => {
                    budget = before;
                    budget.invalid_legacy_split = true;
                    None
                }
            }
        }
        None => None,
    };

    let warnings = budget.warnings(sessions.len());
    Ok((
        SessionsSnapshot {
            version: raw.version,
            sessions,
            active_index: raw.active_index,
            split,
            tree,
            tabs: decoded_tabs.tabs,
            active_tab,
        },
        warnings,
    ))
}

fn decode_bounded_snapshot(
    content: &str,
) -> Result<(SessionsSnapshot, Vec<String>), serde_json::Error> {
    decode_bounded_snapshot_with_text_budget(content, MAX_RESTORED_TEXT_BYTES)
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
        self.bounded_json().ok().map(|(json, _warnings)| json)
    }

    /// 原子写入到文件。
    ///
    /// 之前这里是 `fs::write` + `rename`，注释却写着“原子写入”：临时文件没有
    /// `sync_all`，父目录也没有 fsync，所以掉电后 rename 可能已经生效而内容还没
    /// 落盘，重启后读到的是一份被截断的快照。`write_atomic_private` 连带把目录
    /// 建成 0700、文件 0600 —— 快照里每个 pane 的 cwd 就是用户文件系统的地图。
    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let (json, warnings) = self.bounded_json()?;
        for warning in warnings {
            log::warn!("[SessionPersistence] {warning}");
        }
        crate::persistence::write_snapshot_atomic(path, json.as_bytes(), MAX_SNAPSHOT_BYTES)?;
        Ok(())
    }

    /// 从文件加载。文件不存在与文件读不动是两种结果，见 [`SnapshotLoad`]。
    pub fn load(path: &Path) -> SnapshotLoad {
        if !path.exists() {
            return SnapshotLoad::Missing;
        }
        let content = match crate::persistence::read_text_bounded(path, MAX_SNAPSHOT_BYTES) {
            Ok(content) => content,
            // 和 exists() 之间存在竞争：文件刚被删掉就当成没有快照，否则调用方
            // 会去隔离一个已经不在那里的文件。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return SnapshotLoad::Missing
            }
            Err(error) => return SnapshotLoad::Unreadable(error.to_string()),
        };
        match decode_bounded_snapshot(&content) {
            Ok((mut snapshot, mut warnings)) => {
                warnings.extend(snapshot.sanitize());
                for warning in warnings {
                    log::warn!("[SessionPersistence] {warning}");
                }
                SnapshotLoad::Loaded(Box::new(snapshot))
            }
            Err(error) => SnapshotLoad::Unreadable(error.to_string()),
        }
    }

    fn bounded_json(&self) -> Result<(String, Vec<String>), Box<dyn std::error::Error>> {
        let mut bounded = self.clone();
        let warnings = bounded.sanitize();
        let json = serde_json::to_string_pretty(&bounded)?;
        if json.len() as u64 > MAX_SNAPSHOT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!(
                    "bounded session snapshot is {} bytes; limit is {MAX_SNAPSHOT_BYTES}",
                    json.len()
                ),
            )
            .into());
        }
        Ok((json, warnings))
    }

    fn sanitize(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.sessions.len() > MAX_RESTORED_SESSIONS {
            warnings.push(format!(
                "restored only the first {MAX_RESTORED_SESSIONS} of {} sessions",
                self.sessions.len()
            ));
            self.sessions.truncate(MAX_RESTORED_SESSIONS);
        }
        let mut invalid_cwds = 0usize;
        for session in &mut self.sessions {
            if session.cwd.as_ref().is_some_and(|cwd| {
                cwd.len() > MAX_RESTORED_CWD_BYTES || cwd.as_bytes().contains(&0)
            }) {
                session.cwd = None;
                invalid_cwds += 1;
            }
        }
        if invalid_cwds > 0 {
            warnings.push(format!(
                "discarded {invalid_cwds} oversized or invalid working directories"
            ));
        }
        if self
            .active_index
            .is_some_and(|index| index >= self.sessions.len())
        {
            self.active_index = self.sessions.len().checked_sub(1);
            warnings.push("active session index was outside the restored list".to_string());
        }

        let original_tabs = self.tabs.len();
        self.tabs.truncate(MAX_RESTORED_TABS);
        self.tabs
            .retain_mut(|tab| sanitize_tree_shape(&mut tab.tree));
        // 标题是自由文本，会原样出现在标签栏上：控制字符和超长串在这里就
        // 丢弃，而不是等到渲染时才发现。
        let mut invalid_titles = 0usize;
        for tab in &mut self.tabs {
            if tab.title.as_ref().is_some_and(|title| {
                title.trim().is_empty()
                    || title.len() > MAX_RESTORED_TAB_TITLE_BYTES
                    || title.chars().any(|c| c.is_control())
            }) {
                tab.title = None;
                invalid_titles += 1;
            }
        }
        if invalid_titles > 0 {
            warnings.push(format!(
                "discarded {invalid_titles} oversized or invalid tab titles"
            ));
        }
        if self.tabs.len() != original_tabs {
            warnings.push("discarded oversized or invalid tab layouts".to_string());
        }
        if self
            .tree
            .as_mut()
            .is_some_and(|tree| !sanitize_tree_shape(tree))
        {
            self.tree = None;
            warnings.push("discarded invalid legacy pane layout".to_string());
        }
        if self.split.as_mut().is_some_and(|split| {
            if !matches!(split.mode.as_str(), "vertical" | "horizontal")
                || !(2..=MAX_RESTORED_PANES_PER_TAB).contains(&split.panes.len())
            {
                return true;
            }
            split.ratios.truncate(split.panes.len());
            split.focused = split.focused.min(split.panes.len().saturating_sub(1));
            false
        }) {
            self.split = None;
            warnings.push("discarded invalid legacy split layout".to_string());
        }
        if self
            .active_tab
            .is_some_and(|index| index >= self.tabs.len())
        {
            self.active_tab = self.tabs.len().checked_sub(1);
            warnings.push("active tab index was outside the restored list".to_string());
        }
        warnings
    }
}

fn sanitize_tree_shape(tree: &mut PaneTreeSnapshot) -> bool {
    fn visit(
        tree: &mut PaneTreeSnapshot,
        depth: usize,
        nodes: &mut usize,
        leaves: &mut usize,
    ) -> bool {
        if depth > MAX_RESTORED_LAYOUT_DEPTH || *nodes >= MAX_RESTORED_LAYOUT_NODES {
            return false;
        }
        *nodes += 1;
        match tree {
            PaneTreeSnapshot::Leaf { .. } => {
                *leaves += 1;
                *leaves <= MAX_RESTORED_PANES_PER_TAB
            }
            PaneTreeSnapshot::Split {
                axis,
                ratios,
                children,
            } => {
                if !matches!(axis.as_str(), "vertical" | "horizontal")
                    || !(2..=MAX_RESTORED_PANES_PER_TAB).contains(&children.len())
                {
                    return false;
                }
                ratios.truncate(children.len());
                children
                    .iter_mut()
                    .all(|child| visit(child, depth + 1, nodes, leaves))
            }
        }
    }

    visit(tree, 0, &mut 0, &mut 0)
}

/// 尝试获取单实例锁。成功返回持锁的 `File`（需在进程生命周期内持有），
/// 失败（已有实例运行）返回 `None`。端口自 ember `try_acquire_instance_lock`。
pub fn try_acquire_instance_lock() -> Option<std::fs::File> {
    let lock_path = dirs::config_dir()?.join("frost").join("instance.lock");
    try_acquire_instance_lock_at(&lock_path)
}

fn try_acquire_instance_lock_at(lock_path: &Path) -> Option<std::fs::File> {
    use std::io::{Seek, Write};

    // The old create(true) open followed a pre-positioned instance.lock
    // symlink and later truncated its target. The shared hardened opener uses
    // O_NOFOLLOW, 0600, owner/nlink validation and CLOEXEC before flocking.
    let mut file = match crate::persistence::try_acquire_process_lock(lock_path) {
        Ok(Some(file)) => file,
        Ok(None) => return None,
        Err(error) => {
            log::warn!("Cannot acquire instance lock: {error}");
            return None;
        }
    };
    // Truncate only after owning the lock. A losing second instance must not
    // erase the first instance's diagnostic PID.
    if let Err(error) = file
        .set_len(0)
        .and_then(|_| file.seek(std::io::SeekFrom::Start(0)).map(|_| ()))
        .and_then(|_| write!(file, "{}", std::process::id()))
        .and_then(|_| file.sync_data())
    {
        log::warn!(
            "Cannot initialize instance lock {}: {error}",
            lock_path.display()
        );
        return None;
    }
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("frost-snapshot-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        root
    }

    fn decode_and_sanitize(contents: &str) -> (SessionsSnapshot, Vec<String>) {
        let (mut snapshot, mut warnings) = decode_bounded_snapshot(contents).unwrap();
        warnings.extend(snapshot.sanitize());
        (snapshot, warnings)
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
        write_private(&path, b"{\"version\":2,\"sessions\":[{\"cwd\"");

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
        write_private(
            &path,
            format!("{{\"version\":2,\"sessions\":[],\"pad\":\"{padding}\"}}"),
        );

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

    #[cfg(unix)]
    #[test]
    fn instance_lock_is_private_exclusive_and_never_truncated_by_a_loser() {
        use std::os::unix::fs::PermissionsExt;

        let root = scratch("instance-lock");
        let path = root.join("instance.lock");
        let first = try_acquire_instance_lock_at(&path).expect("first owner acquires lock");
        let pid = std::process::id().to_string();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), pid);
        assert!(
            try_acquire_instance_lock_at(&path).is_none(),
            "second owner must not acquire or rewrite the held lock"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), pid);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(first);
        // Other tests exercise the real fork→exec PTY boundary in parallel.
        // A child forked while `first` was live inherits the flock until its
        // immediate exec applies CLOEXEC, so an instantaneous retry can observe
        // that harmless transition window. Require bounded eventual release;
        // a genuine leaked descriptor still fails after the deadline.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let reacquired = loop {
            if let Some(lock) = try_acquire_instance_lock_at(&path) {
                break Some(lock);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(reacquired.is_some(), "released lock stayed held after exec");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_rejects_a_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let root = scratch("instance-symlink");
        let path = root.join("instance.lock");
        let victim = root.join("victim.txt");
        write_private(&victim, b"keep me");
        symlink(&victim, &path).unwrap();

        assert!(try_acquire_instance_lock_at(&path).is_none());
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshots_without_split_field_still_deserialize() {
        let legacy = r#"{"version":1,"sessions":[{"cwd":"/tmp"}],"active_index":0}"#;
        let (snap, warnings) = decode_and_sanitize(legacy);
        assert!(warnings.is_empty(), "{warnings:?}");
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
                title: None,
                pinned: false,
                marked: false,
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
        let (back, warnings) = decode_and_sanitize(&json);
        assert!(warnings.is_empty(), "{warnings:?}");
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
        let (snap, warnings) = decode_and_sanitize(legacy);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(snap.tabs.is_empty());
        assert!(snap.active_tab.is_none());
        // The restore path turns this single tree into the first tab.
        assert!(snap.tree.is_some());
    }

    #[test]
    fn legacy_flat_split_field_still_deserializes() {
        // Old frost snapshots stored a single-axis `split` and no `tree`. Both
        // fields must round-trip so the restore path can fall back to `split`.
        let legacy = r#"{"version":1,"sessions":[{"cwd":null},{"cwd":null}],
            "active_index":0,
            "split":{"mode":"vertical","ratios":[0.35,0.65],"panes":[0,1],"focused":0}}"#;
        let (snap, warnings) = decode_and_sanitize(legacy);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(snap.tree.is_none());
        let split = snap.split.unwrap();
        assert_eq!(split.panes, vec![0, 1]);
        assert_eq!(split.mode, "vertical");
    }

    #[test]
    fn restored_structure_and_cwds_are_bounded_before_spawn_or_layout_conversion() {
        let root = scratch("sanitize");
        let path = root.join("session_history.json");
        let invalid_tree = PaneTreeSnapshot::Split {
            axis: "diagonal".to_string(),
            ratios: vec![1.0; 100],
            children: vec![PaneTreeSnapshot::Leaf { session: 0 }; 100],
        };
        let snapshot = SessionsSnapshot {
            version: 2,
            sessions: vec![
                SessionSnapshot {
                    cwd: Some("x".repeat(MAX_RESTORED_CWD_BYTES + 1)),
                };
                MAX_RESTORED_SESSIONS + 10
            ],
            active_index: Some(usize::MAX),
            split: None,
            tree: Some(invalid_tree.clone()),
            tabs: vec![
                TabSnapshot {
                    tree: invalid_tree,
                    focus: Some(0),
                    title: None,
                    pinned: false,
                    marked: false,
                };
                MAX_RESTORED_TABS + 10
            ],
            active_tab: Some(usize::MAX),
        };
        write_private(&path, serde_json::to_vec(&snapshot).unwrap());

        let SnapshotLoad::Loaded(restored) = SessionsSnapshot::load(&path) else {
            panic!("bounded valid JSON should load");
        };
        assert_eq!(restored.sessions.len(), MAX_RESTORED_SESSIONS);
        assert!(restored
            .sessions
            .iter()
            .all(|session| session.cwd.is_none()));
        assert_eq!(restored.active_index, Some(MAX_RESTORED_SESSIONS - 1));
        assert!(restored.tabs.is_empty());
        assert!(restored.tree.is_none());
        assert!(restored.active_tab.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// Tab titles come from a text field and are drawn verbatim in the strip.
    /// A restored snapshot must not be able to smuggle control characters or an
    /// unbounded string into that label; pin/mark ride along unchanged.
    #[test]
    fn restored_tab_titles_are_bounded_and_control_free_while_flags_survive() {
        let root = scratch("tab-titles");
        let path = root.join("session_history.json");
        let tab = |title: Option<&str>, pinned: bool, marked: bool| TabSnapshot {
            tree: PaneTreeSnapshot::Leaf { session: 0 },
            focus: Some(0),
            title: title.map(str::to_string),
            pinned,
            marked,
        };
        let snapshot = SessionsSnapshot {
            version: 2,
            sessions: vec![SessionSnapshot { cwd: None }],
            active_index: Some(0),
            split: None,
            tree: None,
            tabs: vec![
                tab(Some("build"), true, false),
                tab(
                    Some(&"x".repeat(MAX_RESTORED_TAB_TITLE_BYTES + 1)),
                    false,
                    true,
                ),
                tab(Some("two\nlines"), false, false),
                tab(Some("   "), false, false),
            ],
            active_tab: Some(0),
        };
        write_private(&path, serde_json::to_vec(&snapshot).unwrap());

        let SnapshotLoad::Loaded(restored) = SessionsSnapshot::load(&path) else {
            panic!("bounded valid JSON should load");
        };
        assert_eq!(restored.tabs[0].title.as_deref(), Some("build"));
        assert!(restored.tabs[0].pinned);
        // Oversized, control-bearing, and blank titles all fall back to
        // "follow the session's own label".
        assert_eq!(restored.tabs[1].title, None);
        assert!(restored.tabs[1].marked);
        assert_eq!(restored.tabs[2].title, None);
        assert_eq!(restored.tabs[3].title, None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Snapshots written before the context menu existed have none of these
    /// fields; they must load as plain tabs rather than failing the parse.
    #[test]
    fn legacy_tab_snapshots_without_menu_state_still_load() {
        let root = scratch("legacy-tabs");
        let path = root.join("session_history.json");
        write_private(
            &path,
            br#"{"version":2,"sessions":[{"cwd":"/tmp"}],"active_index":0,
                 "tabs":[{"tree":{"kind":"leaf","session":0},"focus":0}],
                 "active_tab":0}"#,
        );

        let SnapshotLoad::Loaded(restored) = SessionsSnapshot::load(&path) else {
            panic!("legacy snapshot should load");
        };
        assert_eq!(restored.tabs.len(), 1);
        assert_eq!(restored.tabs[0].title, None);
        assert!(!restored.tabs[0].pinned);
        assert!(!restored.tabs[0].marked);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_decoder_caps_wide_sessions_tabs_ratios_and_trees_before_ownership() {
        let sessions = std::iter::repeat_n(r#"{"cwd":"/tmp"}"#, 400)
            .collect::<Vec<_>>()
            .join(",");
        let ratios = std::iter::repeat_n("0.5", 200)
            .collect::<Vec<_>>()
            .join(",");
        let tree = format!(
            r#"{{"kind":"split","axis":"vertical","ratios":[{ratios}],
                 "children":[{{"kind":"leaf","session":0}},{{"kind":"leaf","session":1}}]}}"#
        );
        let tab = format!(r#"{{"tree":{tree},"focus":0}}"#);
        let tabs = std::iter::repeat_n(tab, 80).collect::<Vec<_>>().join(",");
        let json = format!(
            r#"{{"version":2,"sessions":[{sessions}],
                 "split":{{"mode":"horizontal","ratios":[{ratios}],"panes":[0,1],"focused":0}},
                 "tree":{tree},"tabs":[{tabs}],"active_tab":0}}"#
        );
        assert!(json.len() < MAX_SNAPSHOT_BYTES as usize);

        let (snapshot, warnings) = decode_and_sanitize(&json);

        assert_eq!(snapshot.sessions.len(), MAX_RESTORED_SESSIONS);
        assert!(snapshot.sessions.capacity() <= MAX_RESTORED_SESSIONS);
        assert_eq!(snapshot.tabs.len(), MAX_RESTORED_TABS);
        assert!(snapshot.tabs.capacity() <= MAX_RESTORED_TABS);
        let assert_tree_capacity = |tree: &PaneTreeSnapshot| {
            let PaneTreeSnapshot::Split {
                ratios, children, ..
            } = tree
            else {
                panic!("expected a split");
            };
            assert!(ratios.capacity() <= MAX_RESTORED_PANES_PER_TAB);
            assert!(children.capacity() <= MAX_RESTORED_PANES_PER_TAB);
        };
        assert_tree_capacity(snapshot.tree.as_ref().unwrap());
        for tab in &snapshot.tabs {
            assert_tree_capacity(&tab.tree);
        }
        let split = snapshot.split.as_ref().unwrap();
        assert!(split.ratios.capacity() <= MAX_RESTORED_PANES_PER_TAB);
        assert!(split.panes.capacity() <= MAX_RESTORED_PANES_PER_TAB);
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("restored only the first")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("tab layouts")));
    }

    #[test]
    fn surplus_sessions_and_ratios_are_schema_validated_without_being_retained() {
        let sessions = std::iter::repeat_n(r#"{"cwd":null}"#, MAX_RESTORED_SESSIONS)
            .chain(std::iter::once(r#"{"cwd":7}"#))
            .collect::<Vec<_>>()
            .join(",");
        let invalid_session = format!(r#"{{"version":2,"sessions":[{sessions}]}}"#);
        assert!(decode_bounded_snapshot(&invalid_session).is_err());

        let ratios = std::iter::repeat_n("0.5", MAX_RESTORED_PANES_PER_TAB)
            .chain(std::iter::once(r#""wrong""#))
            .collect::<Vec<_>>()
            .join(",");
        let invalid_ratio = format!(
            r#"{{"version":2,"sessions":[{{"cwd":null}}],"tabs":[
                {{"tree":{{"kind":"split","axis":"vertical","ratios":[{ratios}],
                    "children":[{{"kind":"leaf","session":0}},{{"kind":"leaf","session":0}}]}}}}
            ]}}"#
        );
        let (snapshot, warnings) = decode_and_sanitize(&invalid_ratio);
        assert!(snapshot.tabs.is_empty());
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("tab layouts")));
    }

    #[test]
    fn unsupported_version_short_circuits_before_scanning_a_later_payload() {
        // The tail is deliberately malformed. Once a leading future version is
        // known, neither raw arrays nor optional layouts should be inspected.
        let error = decode_bounded_snapshot(r#"{"version":99,"sessions":["#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported session snapshot version 99"));

        // A postfixed version cannot avoid the envelope scan, but still wins
        // over layout decoding once the valid raw envelope has been collected.
        let error = decode_bounded_snapshot(
            r#"{"sessions":[],"tree":{"kind":"split","axis":7},"version":99}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unsupported session snapshot version 99"));
    }

    #[test]
    fn required_known_fields_remain_strict_while_long_unknown_keys_are_ignored() {
        for invalid in [
            r#"{"version":2,"version":2,"sessions":[]}"#,
            r#"{"version":2}"#,
            r#"{"version":2,"sessions":{}}"#,
            r#"{"version":2,"sessions":[{"cwd":1}]}"#,
            r#"{"version":2,"sessions":[{"cwd":null,"cwd":null}]}"#,
            r#"{"version":2,"sessions":[],"active_index":"zero"}"#,
        ] {
            assert!(
                decode_bounded_snapshot(invalid).is_err(),
                "accepted invalid known field: {invalid}"
            );
        }

        let unknown = "x".repeat(8 * 1024);
        let json = format!(
            r#"{{"version":2,"sessions":[{{"cwd":"/tmp","{unknown}":true}}],
                "tabs":[{{"tree":{{"kind":"leaf","session":0,"{unknown}":[1,2,3]}},
                           "{unknown}":{{"nested":true}}}}],
                "{unknown}":"ignored"}}"#
        );
        let (snapshot, warnings) = decode_and_sanitize(&json);
        assert_eq!(snapshot.sessions[0].cwd.as_deref(), Some("/tmp"));
        assert_eq!(snapshot.tabs.len(), 1);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn tab_decode_is_transactional_and_keeps_valid_neighbors() {
        let json = r#"{
            "version": 2,
            "sessions": [{"cwd": "/a"}, {"cwd": "/b"}],
            "tabs": [
                {"tree": {"kind": "leaf", "session": 0}, "title": "first"},
                {"title": "missing tree"},
                {"tree": {"kind": "split", "axis": "vertical",
                          "children": [{"kind": "leaf", "session": 0}] }},
                {"tree": {"kind": "leaf", "session": 1}, "title": "last"}
            ],
            "active_tab": 3
        }"#;

        let (snapshot, warnings) = decode_and_sanitize(json);

        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.tabs[0].title.as_deref(), Some("first"));
        assert_eq!(snapshot.tabs[1].title.as_deref(), Some("last"));
        assert!(matches!(
            snapshot.tabs[1].tree,
            PaneTreeSnapshot::Leaf { session: 1 }
        ));
        assert_eq!(snapshot.active_tab, Some(1));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("tab layouts")));
    }

    #[test]
    fn active_tab_tracks_its_input_identity_across_transactional_discards() {
        let invalid = r#"{"title":"invalid: no tree"}"#;
        let first = r#"{"tree":{"kind":"leaf","session":0},"title":"first"}"#;
        let second = r#"{"tree":{"kind":"leaf","session":1},"title":"second"}"#;

        let before_active = format!(
            r#"{{"version":2,"sessions":[{{"cwd":null}},{{"cwd":null}}],
                "tabs":[{invalid},{first},{second}],"active_tab":1}}"#
        );
        let (snapshot, _) = decode_and_sanitize(&before_active);
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.active_tab, Some(0));
        assert_eq!(snapshot.tabs[0].title.as_deref(), Some("first"));

        let active_itself = format!(
            r#"{{"version":2,"sessions":[{{"cwd":null}},{{"cwd":null}}],
                "tabs":[{first},{invalid},{second}],"active_tab":1}}"#
        );
        let (snapshot, warnings) = decode_and_sanitize(&active_itself);
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(
            snapshot.active_tab,
            Some(1),
            "the first surviving tab after the discarded active tab wins"
        );
        assert_eq!(snapshot.tabs[1].title.as_deref(), Some("second"));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("active tab index")));
    }

    #[test]
    fn invalid_optional_tabs_and_tree_warn_then_fall_back_to_legacy_split() {
        let json = r#"{
            "version": 1,
            "sessions": [{"cwd": null}, {"cwd": null}],
            "tabs": {"not": "an array"},
            "tree": {"kind": "split", "axis": "diagonal",
                     "children": [{"kind": "leaf", "session": 0},
                                  {"kind": "leaf", "session": 1}]},
            "split": {"mode": "vertical", "ratios": [0.35, 0.65],
                      "panes": [0, 1], "focused": 1}
        }"#;

        let (snapshot, warnings) = decode_and_sanitize(json);

        assert!(snapshot.tabs.is_empty());
        assert!(snapshot.tree.is_none());
        assert_eq!(snapshot.split.as_ref().unwrap().panes, vec![0, 1]);
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("tab layouts")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("legacy pane layout")));
    }

    #[test]
    fn a_deep_late_tab_is_dropped_without_losing_valid_tabs_after_it() {
        let leaf = r#"{"kind":"leaf","session":0}"#;
        let mut deep = leaf.to_string();
        for _ in 0..=MAX_RESTORED_LAYOUT_DEPTH {
            deep = format!(r#"{{"kind":"split","axis":"vertical","children":[{deep},{leaf}]}}"#);
        }
        let json = format!(
            r#"{{"version":2,"sessions":[{{"cwd":null}}],"tabs":[
                {{"tree":{leaf},"title":"before"}},
                {{"tree":{deep},"title":"discarded"}},
                {{"tree":{leaf},"title":"after"}}
            ]}}"#
        );
        assert!(json.len() < MAX_SNAPSHOT_BYTES as usize);

        let (snapshot, warnings) = decode_and_sanitize(&json);

        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.tabs[0].title.as_deref(), Some("before"));
        assert_eq!(snapshot.tabs[1].title.as_deref(), Some("after"));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("tab layouts")));
    }

    #[test]
    fn bounded_decoder_preserves_v1_v2_defaults_and_charges_cumulative_text() {
        let v1 = r#"{"version":1,"sessions":[{"cwd":"/tmp"}],"active_index":0,
            "split":{"mode":"vertical","panes":[0,0],"focused":0}}"#;
        let (v1, warnings) = decode_and_sanitize(v1);
        assert!(v1.tabs.is_empty());
        assert!(v1.tree.is_none());
        assert!(v1.split.is_some());
        assert!(warnings.is_empty(), "{warnings:?}");

        let v2 = r#"{"version":2,"sessions":[{"cwd":null}],
            "tabs":[{"tree":{"kind":"leaf","session":0}}]}"#;
        let (v2, warnings) = decode_and_sanitize(v2);
        assert_eq!(v2.tabs.len(), 1);
        assert_eq!(v2.tabs[0].focus, None);
        assert_eq!(v2.tabs[0].title, None);
        assert!(!v2.tabs[0].pinned);
        assert!(!v2.tabs[0].marked);
        assert!(warnings.is_empty(), "{warnings:?}");

        let error = decode_bounded_snapshot_with_text_budget(
            r#"{"version":2,"sessions":[{"cwd":"12345678"},{"cwd":"abcdefgh"}]}"#,
            12,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("cumulative text budget"), "{error}");
    }

    #[test]
    fn future_snapshot_versions_are_preserved_as_unreadable() {
        let root = scratch("future-version");
        let path = root.join("session_history.json");
        write_private(&path, br#"{"version":99,"sessions":[{"cwd":"/tmp"}]}"#);

        assert!(matches!(
            SessionsSnapshot::load(&path),
            SnapshotLoad::Unreadable(reason) if reason.contains("unsupported")
        ));
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
