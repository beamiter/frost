# frost

frost 是一个面向 Linux 的现代终端模拟器，使用 Rust、iced 和 wgpu 构建。它把多标签、分屏、完整回滚搜索、会话恢复和 GPU 渲染放进一个轻量桌面应用，同时默认收紧远程终端可触达的宿主能力。

## 主要能力

- 多标签、拖动排序、快速标签切换，以及 tmux 风格的树状分屏（任意 pane 可再沿任一方向嵌套拆分）
- 拖动普通单-pane tab 到目标 pane 的四边即可无损并入分屏；拖动分屏标题栏回 tab 栏即可恢复为普通 tab。拖动悬停标签约半秒会预览目标页，中心释放安全取消，现有 PTY 不会重启或复制
- 搜索当前屏幕与全部 scrollback，支持大小写匹配、正则和自动滚动定位
- 查找替换面板（`Ctrl+Alt+R`）：scrollback 是只读输出，替换作用于当前选中文本——结果复制到剪贴板，或不带回车回填到提示符；支持字面/正则、大小写、全词与全部替换
- UTF-8、中文宽字符、True Color、256 色、鼠标报告、括号粘贴和扩展键盘协议
- Kitty 图像直接传输（`f=100` PNG、`f=24` RGB、`f=32` RGBA），协议的结构层与 anvil/2/4 共用 `jterm_core::kitty_graphics`；带传输、像素、解压内存和放置数量上限，并对带 `i=`/`I=` 的命令回送 `OK` / `EINVAL` / `ENOTSUP` / `ENOENT` 应答
- 文件侧栏、路径插入、链接识别、命令面板、主题编辑和实时设置
- OSC 8 显式超链接会随网格、scrollback 与 resize/reflow 保留；`Ctrl+单击` 打开时与文本
  检测链接共用同一套安全策略，只允许无凭据、无视觉欺骗的绝对 HTTP(S) URL。URI、id 和
  会话内链接表均有硬上限，过期 viewport 点击会按投影版本拒绝而不会误开新位置的目标
- 文件侧栏按目录异步懒加载，支持返回上级与刷新；慢盘、NFS/FUSE 不再阻塞主界面。侧栏还可通过
  `[[remote_hosts]]` 原生浏览 SSH 主机与运行中的 Docker 容器（无需 sshfs）：远程一侧只运行一段
  标准 POSIX sh 探测脚本（经 `ssh` / `docker exec` 的 stdin 传入）。右键任意节点或空白区可打开
  文件操作菜单：新建文件/目录、重命名、删除（含确认框）、复制/剪切/粘贴、复制路径与刷新，
  本地与远程行为一致；跨位置粘贴即为上传/下载（远程⇄远程经本地临时中转），文件按流式传输、
  目录经 tar 转发（目录上传在解包前原子拒绝同名目标），实时显示传输进度（可随时取消，
  取消不会留下半截文件），全程有 512 MiB 上限与超时保护，远端失败会在面板内联显示。
  从系统文件管理器把文件/目录拖放到文件树即可导入：落在目录行上导入该目录、其余位置导入当前根目录，
  远程位置走同一条上传通道；一次拖放最多 256 项、总量不超过传输上限，同名目标逐项拒绝
- 自动保存标签工作目录并恢复会话；多实例之间不会互相覆盖恢复数据
- OSC 10/11/12 动态颜色、OSC 52/5522 剪贴板和桌面通知
- OSC 133 Block mode：完成命令、Background 输出和当前输入/运行区以主题相对卡片呈现（状态条、轻染色、圆角、状态/耗时徽标，支持普通与 Compact Block Spacing），空闲提示符处、用户编辑前的异步输出会形成 Background 块，运行中块实时显示已用时间；支持块选择、右键动作、书签、失败/慢命令/Background 筛选、复制/回填、多块 Markdown、整会话 Markdown/JSON 导出与跨块搜索，历史修剪后已捕获的块输出仍可搜索和复制
- 持久化命令历史与模糊选择器（`Ctrl+Shift+H`）：完成的命令连同目录、退出码写入与 anvil/forge 同格式的 JSONL 索引（从不保存输出），跨重启召回；Enter 只把选中命令回填到提示符，不自动执行
- 长命令完成桌面通知：OSC 133 计时超过阈值（默认 10 秒）且命令不在正被注视的 pane（窗口失焦或非活动 pane）时提醒
- 分屏 pane 标题栏显示所在目录的 git 分支与脏状态（后台探测并缓存，从不逐帧运行 git）
- 有界 PTY 输入/输出队列、稳定会话身份校验和繁忙进程关闭保护
- Shell Agent 提案始终逐条人工审批；只有绑定 pane 处于空闲且提示符输入为空时才可发送，
  命令完成结果按一次性执行代次精确关联，迟到或仅后缀相同的输出不会推进 Agent
- 失败命令块（OSC 133 报告非零退出码的已完成块）在右键菜单与命令面板中提供
  **Fix with Agent** / **Explain with Agent** / **Retry** 动作。Fix/Explain 把精确命令、
  有界捕获输出和已验证的 cwd 作为框架化不可信上下文，开启一个全新的 Agent 任务：绝不接续
  无关的历史会话快照，任务始终按稳定会话 id 绑定来源 pane（焦点切换不影响），也不会替换仍在
  运行的已批命令、待审提案或未结束的会话。Retry 走守护式语义重放：仅精确、未截断、单行的命令，
  且仅当块记录的 cwd 与独立观测到的本地 shell 进程 cwd（`/proc`）一致时才执行——SSH/tmux 类
  包装进程的本地 cwd 不代表其报告的工作目录，因此失败关闭；此外还要求主屏幕、提示符空闲、
  输入为空且开启 bracketed paste。被 16 KiB 上限截断的命令与 Background 块一律不进入
  Retry/Agent 路径
- PTY 启动采用 fork→exec 错误握手；无效目录、shell/exec 失败会显示可重试诊断而不是崩溃
- 配置与快捷键热重载采用 last-known-good；坏文件会显示路径/行列并暂停自动写回
- 字体探测与桌面通知只调用固定可信系统程序，独立进程组内并发有界读取 stdout/stderr，
  统一超时后终止并回收整组；工作目录或可写 `PATH` 项不能替换这些后台 helper

## 构建与运行

目前支持 Linux。项目使用 Rust stable 工具链（`rust-toolchain.toml` 会由 rustup 自动选择），并需要 Fontconfig、Wayland/X11 和 OpenGL/EGL 的开发库。Ubuntu/Debian 可安装：

```bash
sudo apt-get install pkg-config libfontconfig1-dev libwayland-dev \
  libx11-dev libx11-xcb-dev libxcb1-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libxcursor-dev libxi-dev \
  libxrandr-dev libxkbcommon-dev libegl1-mesa-dev libgl1-mesa-dev
```

然后构建：

```bash
rustup toolchain install stable --profile minimal --component rustfmt --component clippy
cargo build --release --locked
./target/release/frost
```

## 安装（含桌面集成）

```bash
./scripts/install.sh              # 构建并安装二进制 + 启动器条目
./scripts/install.sh --binary /path/to/frost  # 安装已构建的二进制，跳过 Cargo
./scripts/install.sh --dry-run    # 只打印将要执行的命令，不改动文件
./scripts/install.sh --no-desktop # 只装二进制
./scripts/uninstall.sh            # 一并移除；配置与历史保留
```

`--binary` 适合发布压缩包、CI 产物和发行版打包：安装器不会调用 Rust 工具链，仍会用同一套
受测路径安装二进制、desktop 文件、AppStream 元数据和图标。输入必须是可读的普通文件；目标
二进制权限统一设为 `0755`。它可与 `--prefix`、`--bin-dir`、`--no-desktop` 和 `DESTDIR`
组合使用。

默认装到 `~/.local`（可用 `--prefix` / `--bin-dir` 覆盖，打包场景用 `DESTDIR`）：

| 内容 | 位置 |
| --- | --- |
| 二进制 | `~/.local/bin/frost` |
| 启动器条目 | `~/.local/share/applications/io.github.beamiter.frost.desktop` |
| 图标 | `~/.local/share/icons/hicolor/{scalable,128x128,256x256}/apps/io.github.beamiter.frost.*` |
| AppStream 元数据 | `~/.local/share/metainfo/io.github.beamiter.frost.metainfo.xml` |

安装与卸载脚本都从同一组运行时路径推导目标：二进制默认为 `PREFIX/bin/frost`，
桌面文件和图标位于 `PREFIX/share`。再次运行安装脚本会更新同一目标。`--bin-dir`
只覆盖二进制目录；之后卸载时也应传入同一个 `--bin-dir`。`DESTDIR` 只在这些
运行时绝对路径前追加打包根目录，desktop 文件中的 `Exec=` 仍指向不含
`DESTDIR` 的运行时路径。

旧版源码安装脚本曾错误地把无参数安装写到 `~/.cargo/bin/frost`。新脚本不会
自动删除那个可能由用户显式管理的文件；若升级后 `command -v frost` 仍指向旧
位置，可先核对它确实是旧副本，再执行
`rm -f -- "$HOME/.cargo/bin/frost"`。卸载脚本总会同时移除所选 prefix 下的桌面
集成，因此不应仅为清理旧二进制而运行它。

这套桌面集成才让 frost 出现在 GNOME/KDE 的应用列表里，可搜索、可点击启动、可固定到
dock。有三个细节决定它到底显不显示，安装脚本都已处理：

- `Exec=` / `TryExec=` 会被改写成二进制的绝对路径（`/usr` 这类系统 prefix 保留相对
  形式以便重定位）。桌面会话的 `PATH` 在登录时就固定了，若 `~/.local/bin` 不在其中，
  `TryExec=frost` 会失败并让条目**整个从应用列表消失**。自定义路径中的空格、`$` 和
  反斜杠会按 Desktop Entry 规范分别编码到 `Exec` / `TryExec`；规范禁止可执行路径含
  `=`。含 `%` 的绝对路径还会落入「引号内 field code 行为未定义」的兼容陷阱，因此这
  两种路径在启用桌面集成时都会被安装器明确拒绝。
- 安装与卸载后都会刷新 `update-desktop-database` 和 `gtk-update-icon-cache`；陈旧的
  图标缓存会盖住刚装进去的图标。`DESTDIR` 打包时跳过，交给包管理器处理。
- `StartupWMClass` 为 `io.github.beamiter.frost`，与窗口真实的 `WM_CLASS` 一致。
  iced 把 `window::Settings` 里的 `platform_specific.application_id` 同时用作 X11
  `WM_CLASS` 与 Wayland app_id；不设置时两者都是空字符串，桌面环境无法把窗口关联到
  条目，dock 里只会出现一个没有图标、无法固定的窗口。

窗口本身也带图标：`data/io.github.beamiter.frost-128.png` 内嵌进二进制并在启动时交给
winit，因此即便直接 `cargo run`、或条目还没安装，`_NET_WM_ICON` 也是设好的——启动器条目
只能覆盖桌面环境能关联上的窗口。

可用 `desktop-file-validate <条目>` 与 `gtk-launch io.github.beamiter.frost` 自检。

也可以只手动安装二进制：

```bash
install -Dm755 target/release/frost "$HOME/.local/bin/frost"
```

默认字体会优先使用 SauceCodePro Nerd Font；未安装时 iced/Fontconfig 会回退到系统字体。可以在设置面板中选择任意已安装的等宽字体。

## 常用快捷键

| 操作 | 快捷键 |
| --- | --- |
| 新建标签 | `Ctrl+Shift+T` |
| 复制 / 粘贴 | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| 仅复制所选命令块的输出 | `Alt+复制快捷键`（默认 `Ctrl+Alt+Shift+C`；显式绑定优先；可见终端文本选区优先） |
| 搜索全部回滚 | `Ctrl+Shift+F` |
| 查找替换（选中文本） | `Ctrl+Alt+R`（替换结果进剪贴板或回填提示符，从不改写 scrollback） |
| 上/下一个命令提示符 | `Ctrl+Shift+↑` / `Ctrl+Shift+↓`（需 shell 发送 OSC 133 集成序列） |
| 复制上一条命令输出 | `Ctrl+Shift+G`（同样依赖 OSC 133） |
| 搜索命令块 | `Ctrl+Alt+F`（命令与输出统一搜索；可筛选失败/慢命令/书签/Background；Enter 定位匹配输出行） |
| 添加/移除块书签 | `Ctrl+Shift+B`（仅作用于当前选择；无选择时按键继续交给 PTY；前后书签导航为 `Ctrl+,` / `Ctrl+.`） |
| Agent 修复 / 解释失败命令块 | `Ctrl+Alt+X` / `Ctrl+Alt+E`（作用于选中的或最新的失败块，需 OSC 133） |
| 重试失败命令块 | `Ctrl+Alt+T`（cwd 一致时原样重放该块命令） |
| 全选命令块 | `Ctrl+Shift+A` |
| 清空已完成命令块 | `Ctrl+Shift+K`（显示块数并要求确认；不可撤销；保留当前提示符或运行中命令） |
| 回填所选命令 | `Ctrl+Shift+I`（只回填，不执行） |
| 历史命令选择器 | `Ctrl+Shift+H`（Enter 回填到提示符不执行；`Ctrl+R` 留给 shell 自身） |
| 命令面板 | `Ctrl+Shift+P` |
| 快速切换标签 | `Ctrl+Shift+L` |
| 标签 1–8 / 最后一个 | `Ctrl+1`…`Ctrl+8` / `Ctrl+9` |
| 左右 / 上下分屏 | `Ctrl+Shift+E` / `Ctrl+Shift+D`（拆分聚焦 pane；同向并入同级，异向嵌套子分屏，最多 12 个 pane） |
| 方向聚焦 Pane | `Ctrl+Alt+方向键`（按几何位置跨嵌套跳转，边缘不回绕） |
| 调整 Pane 大小 | `Ctrl+Alt+Shift+方向键`（双击分割线均分该节点） |
| Pane 缩放（临时全屏） | `Ctrl+Shift+Z` |
| 交换相邻 Pane | `Ctrl+Shift+X` |
| 关闭聚焦 Pane / 当前标签 | `Ctrl+Shift+W`（分屏时其余 pane 保持） |
| 文件/标签侧栏 | `Ctrl+\` |
| Shell Agent | `Ctrl+Alt+G` |
| 设置 | `Ctrl+Shift+O` |
| 临时放大 / 缩小 / 恢复配置字号 | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` |
| 窗口透明度增 / 减 | `Ctrl+Alt+=` / `Ctrl+Alt+-`（写回配置 `opacity`，设置面板也有滑块） |

Block mode 的命令首行支持普通单击；`Shift+单击` 可从卡片任意行选择连续范围，
`Ctrl+Shift+单击` 可从任意行切换单块。正文普通/双击/三击仍归终端原生文本选择，
`Ctrl+单击` 链接仍打开链接。右键卡片任意位置会在指针旁打开固定 pane/块目标的动作面板，
可复制、向 Agent 附加该块、回填、书签、跳转、搜索和导出；失败块额外提供
**Fix with Agent** / **Explain with Agent** / **Retry**；右键已选范围中的块不会折叠范围。
有精确保留输出的已完成块还提供 `Collapse output / Expand output`：折叠会从滚动文档中真正
移除输出行并放入一行可点击摘要，不修改 PTY 原始缓冲；块级复制、导出与 Agent 仍使用完整输出。
Block Mode 关闭或进入 alternate screen 时会暂时绕过折叠视图，返回后恢复原状态。
书签以琥珀标记同时显示在 gutter 与滚动条上，块被历史保留策略淘汰或执行
“清空已完成块”时自动移除。清空会统一经过确认框，明确当前 pane 将删除的块数，
并永久移除这些块记录、书签和已捕获输出；此操作不可撤销，但不会影响当前提示符或
正在运行的命令。无选区时 `Ctrl+↑` 从最新块开始选择；已有选区后普通 `↑/↓`
折叠到相邻块，`Shift+↑/↓` 扩缩连续范围。选区存在且提示符空闲时，`Enter`
与 `Ctrl+Shift+I` 都会按终端顺序回填所选命令但不会执行；命令运行中 `Enter`
仍原样交给前台程序。多命令只在 shell 开启 bracketed paste 时作为可编辑多行
文本保留，否则安全地只回填第一个逻辑行，后续换行绝不会触发执行。
`Ctrl+,` / `Ctrl+.` 会循环跳到上一个/下一个书签；已有块选区时
`Ctrl+Shift+↑/↓` 定位到活动块顶部/底部，无选区时仍保留原提示符导航快捷键。

块搜索支持 `All / Failed / Slow / Bookmarked / Background` 五种视图；空查询时可直接
浏览筛选结果。文本查询保存完整逻辑行中的 Unicode 字符跨度，因此长行预览会围绕
关键词截取，并能定位到 soft-wrap 后实际包含命中的物理行。若 scrollback 已淘汰但有
捕获快照，搜索与复制仍可用，定位会安全降级到逻辑行首或块首。Block Mode 关闭、命令运行中
或全屏程序占用 alternate screen 时，物理 Block 快捷键会透传给前台程序，不会只弹
提示后吞键；命令面板和右键菜单仍是明确的鼠标操作入口。

搜索索引优先保留最新块：UI 线程最多提取 8 MiB 源文本，lowercase 在后台构建且常驻
索引最多 16 MiB。索引期间可继续输入筛选条件；新命令完成、缺失 `D` 后由下一提示符
收束或产生 Background 块时会自动刷新。若预算省略了更老块，结果区会明确显示
`older blocks not indexed`，不会把部分索引伪装成完整历史。

命令文本捕获有 16 KiB 上限；超过上限时保留 UTF-8 安全前缀并明确标为截断，复制与
导出仍可使用，但 Recall/Reinput、Agent 和持久化历史不会把不完整命令当成可执行文本。
若 OSC 133 已进入命令生命周期却无法恢复命令内容，会显示不可用占位而不会误归类为
Background。

命令面板中的 **Export Session Blocks as Markdown/JSON** 会把当前 pane 仍保留的
已定型块（最多 256 条，也包括缺失结束标记后由下一提示符收束的记录）写入
`$XDG_DATA_HOME/frost/exports/`（通常是 `~/.local/share/frost/exports/`）。JSON 和
Markdown 都会明确标记命令截断、输出已淘汰或未观察到完成；文件按本地时间命名，
同秒多次导出不覆盖，先私密暂存并原子发布，目录与文件权限分别为 `0700` / `0600`。
JSON 使用版本化的 `frost.block-session` v1 envelope，记录 pane session、捕获时间、
块顺序和截断/淘汰汇总，后续字段演进不再依赖无版本裸数组。

快捷键从 `$XDG_CONFIG_HOME/frost/keybindings.toml`（通常是 `~/.config/frost/keybindings.toml`）加载，并与默认绑定合并。chord 语法与 jterm 家族共享（来自 `jterm_core`）：修饰键顺序任意，接受 `control`、`option`、`cmd`/`command`/`win`/`meta` 等修饰键别名，以及 `enter`/`return`、`esc`/`escape`、`arrowleft`/`left`、`page_up`/`pageup` 等按键别名；`ctrl++` 表示加号本身（也可写 `ctrl+plus`），`\` 可写作 `backslash`，非 ASCII 按键按 Unicode 大小写折叠匹配。

## 配置

主配置位于 `$XDG_CONFIG_HOME/frost/config.toml`。设置面板中的修改会自动保存，外部编辑也会热重载。示例：

```toml
font_family = "JetBrains Mono Nerd Font"
font_size = 14.0
line_spacing = 1.0
padding = 2.0
ui_scale = 1.0
opacity = 1.0        # 窗口背景透明度 0.05–1.0，Ctrl+Alt+=/- 实时调节
scrollback_lines = 20000
scroll_speed = 3
theme = "tokyo-night"
tab_position = "top"
restore_session = true
disable_alt_screen = false
block_mode = true     # OSC 133 命令块、gutter、搜索与右键动作
block_compact = false # Compact Block Spacing；仅收紧卡片内缩/圆角，不改变终端行列

# 可选：明确指定 shell
shell = "/bin/bash"

# 何时检查 jsh 是否有新版本：startup 每次启动联网 | daily（默认）复用缓存 | never 关闭
jsh_update_check = "daily"

# 安全默认值。开启后，SSH 中的程序也能读取宿主剪贴板。
allow_clipboard_read = false

# 长命令完成桌面通知（OSC 133 计时；正被注视的 pane 不提醒）
notify_long_blocks = true
notify_long_block_threshold_ms = 10000

# 分屏 pane 标题栏中的 git 分支/脏状态
show_repo_strip = true

# 持久化命令历史（与 anvil/forge 同名键、同 JSONL 格式，可指向同一文件共享）
# 默认写入 ~/.local/state/frost/history.jsonl，只记录命令、目录、退出码与时间
command_history_enabled = true
# command_history_path = "~/.local/state/frost/history.jsonl"
command_history_max_entries = 10000

# AI / Agent（默认关闭；不开启则没有任何数据离开本机）
ai_enabled = false
ai_provider = "anthropic"   # anthropic | openai-compatible | ollama
ai_model = "claude-sonnet-4-6"
# 流式回复：Agent 面板边生成边显示模型回复（三家 provider 均支持）。
# 关闭则退回整段阻塞请求；两种方式记录到会话里的内容完全一致。
ai_stream = true
# API key 文件：一行 key、权限 600。未设置时在设置面板粘贴 key 即可，
# 会自动写入 ~/.config/frost/ai.key。环境变量 FROST_AI_API_KEY_FILE
# 优先于此项（与 forge 一致），且不会被写回配置文件。
# ai_api_key_file = "~/.config/frost/ai.key"

# 向非本地 AI provider 发送命令上下文（命令、cwd、捕获输出）的显式授权。
# 直连本机回环 Ollama 不需要此项；继承的 HTTP 代理会取消该豁免。
ai_share_command_context = false
# 实验性 Tasks 面板（侧栏 "Tasks" 页）：为失败命令块创建独立 Git worktree
# 任务，并可选地运行原生 Codex 会话。与云端 AI 授权相互独立。
experimental_task_sidebar = false
```

### 实验性 Tasks 面板（原生 Codex 运行时）

开启 `experimental_task_sidebar` 后，失败命令块的右键菜单会出现 **Create task**：
frost 为该命令创建独立的 Git worktree（位于 `~/.local/share/frost/agent-tasks/`）并登记一个任务卡片。
任务卡片上的 **Start Codex** 在 `ai_enabled` 与 `ai_share_command_context` 都已开启时可用，
进入可取消的后台准备阶段（校验已注册 worktree、固定目录描述符、解析可信 codex/node 启动链、
构造受限 prompt、创建私有 0700 CODEX_HOME），全程不阻塞 UI；准备完成时任务代际与当前的
共享/脱敏授权会再次校验，取消、过期或授权被撤销的结果会被直接销毁而不会启动 Codex。

原生会话通过 codex app-server 的换行分隔 JSONL 协议运行，目前只接受已审计的
**codex-cli 0.147.0** 协议版本；登录仅通过 login RPC 传递当前内存中的访问令牌，
并在启动线程前校验 effective config，拒绝任何继承的 MCP、hooks、插件、应用、项目信任或
托管配置来源。审批策略固定为 `never`：托管审批请求只完整展示并只能 **Deny**。
每个 turn 结束后会话停在 **Ready for review**：可以发送有界评审反馈在同一线程上开始顺序后续
turn（最多 32 个），或选择 **Finish Codex** 结束会话。一个回合完成后会话不可恢复、不可重启。

隔离与回收：provider 运行在描述符固定的独立 worktree 和瞬态 user-systemd cgroup
（需要 **cgroup v2** 与可用的 systemd 用户会话）中；`/tmp` 不在可写根内，工具子进程获得
无登录、无代理、仅含受审绝对 PATH 的独立环境；frost 异常退出时由外部 guardian 触发
`cgroup.kill`。只有在 cgroup 为空且 leader 被回收后，任务才进入可验证状态。

会话完全停止后，**Run validation** 会在 worktree 内的独立只读验证终端中重放创建该任务的
原始命令（要求精确、未截断、单行；拒绝控制字符与双向伪装字符、符号链接逃逸，并重新校验
Git 注册与分支；通过打开的目录描述符 + fchdir 传递 cwd，shell 以非登录、no-rc 方式启动）。
验证结果为 running/passed/failed/needs-review/cancelled；即使验证通过，也必须显式点击
**Mark complete** 才算接受任务。**Review diff** 显示相对任务基准提交的有界
`git status --short` 与已跟踪文件的 `git diff HEAD`（未跟踪文件只列出路径）。
原生会话失败或退出不成功时，可以用 **Open terminal Agent** 在终端中显式继续（PTY 兼容路径）。
任务元数据仅存在于运行时；**Hide task** 只隐藏元数据，不会删除 worktree。
任务终端（Agent 或验证）的子进程退出后标签页会保留为只读副本供回看：标题带 "(exited)" 后缀、
pane 头部显示 `■ exited`，键盘与粘贴输入不再写入已死的 PTY，只会弹出一条节流提示；任务终端
也不会进入会话快照，重启后不会恢复成恰好落在任务 worktree 里的普通 shell。


`Ctrl+=`、`Ctrl+-` 和 `Ctrl+滚轮` 只调整当前运行时字号，不再改写配置；`Ctrl+0` 回到 `font_size`。`ui_scale` 会统一缩放标签栏、状态栏、设置面板和命中区域。设置面板中的持久修改仍会自动保存。

设置中的 **Compact Block Spacing** 与 `block_compact` 是同一开关，并会立即更新所有当前 pane。它只收紧卡片内缩与圆角，不改变连续终端 grid 的行列、OSC 133 区域坐标或 PTY 大小。

如果 `config.toml` 或 `keybindings.toml` 编辑出错，frost 会保留最后一次可用配置并在窗口内显示诊断。主配置有错误时自动保存会暂停，避免默认值覆盖原文件；修正文件后会自动恢复热重载。

内置主题包括 Dark、Light、Monokai、Dracula、Nord、Gruvbox Dark、Tokyo Night、One Dark、Catppuccin Mocha 和 Solarized Light。自定义主题保存在 `~/.config/frost/themes/`。

## 安装与更新 jsh

frost 优先使用配套 shell [`jsh`](https://github.com/beamiter/jsh)，找不到才退回 bash。
命令面板中的 **Install or update jsh** 会在独立会话里运行安装脚本：会话本身就是进度界面，
可以 Ctrl+C 中断，脚本结束后等待 Enter 再关闭，失败原因不会一闪而过。

安装脚本来自 jsh 仓库并内嵌在二进制里，因此一台从未装过 jsh 的机器也能引导；校验和验证、
`rename(2)` 原子替换（**运行中的 shell 不受影响，新会话才使用新版本**）、旧二进制回滚副本，
以及 `PATH` 上的 `jsh` 其实是同名的其他程序时的提示，都由脚本统一处理。

缺少 jsh 或有新版本时，标签栏下方出现一条可忽略的提示行。检查在后台线程进行、从不自动安装，
离线时保持静默。`jsh_update_check = "daily"`（默认）复用安装脚本自己的缓存
（`~/.cache/jsh/update-check.json`），同机同时开着多个 jterm 也只产生一次网络请求；
`"startup"` 每次启动都联网，`"never"` 关闭检查。

## Kitty 图像协议

协议的结构层——控制数据解析、`m=1` 分块重组、base64 解码、原始格式长度校验、PNG 的 IHDR 嗅探
和全部容量上限——现在来自 anvil/2/3/4 共用的 `jterm_core::kitty_graphics`（预算取 `Caps::SCREEN`：
64 MiB 编码 / 64 MiB 解码 / 16384 像素边长 / 16 KiB 控制段 / 64 MiB 全部在途分块）。图像仓库、
放置、删除和协议应答仍留在 frost，因为它们需要解码器给出的尺寸和错误文本。

**升级后可见的行为变化：**

- **`x=` / `y=` 改回协议语义，图像位置会移动。** 以前 frost 把 `x=`/`y=` 当成屏幕列/行，
  于是不带这两个键的图像一律画在左上角 `(0,0)`。它们在协议里是**源图裁剪偏移**（像素），
  屏幕位置由命令抵达时的**光标格**决定。现在 `a=T` / `a=p` 都锚定在光标处，`x=`/`y=`/`w=`/`h=`
  只裁剪源图。**先前渲染在左上角的图像会改到光标位置显示**，用 `x=`/`y=` 手工摆位的脚本需要改用光标定位。
- **`t=` 会被校验。** 以前 `t=` 被完全忽略，`t=f` 会把 base64 编码的**文件路径**喂给图像解码器，
  然后静默失败。现在只接受 `t=d`（内联），`t=f` / `t=t` / `t=s` 一律回 `ENOTSUP`。
- **新增协议应答。** 以前没有任何应答，用 `i=` 寻址的客户端（如 `kitten icat`）只能等到自己超时。
  现在带 `i=` 或 `I=` 的命令会收到 `ESC _ G i=<id>[,p=<id>] ; OK ESC \` 或 `<code>:<message>`；
  `q=1` 吃掉 `OK`，`q=2` 连错误一起吃掉；`p=0` 不回显。分块传输只在最后一块之后应答一次。
- **只有 APC 会进入图像协议。** 以前 DCS（`ESC P`）、SOS（`ESC X`）、PM（`ESC ^`）与 APC 走同一条路，
  且用 `payload.contains("a=")` 嗅探，一段无关的 DCS 只要含 `a=` 就会被当作图像命令。
  现在只有 `ESC _ G …` 才路由过去。
- **`f=` 缺省值从 PNG 变成 RGBA。** 这是协议默认值。不写 `f=` 的命令现在表示原始 RGBA，
  必须同时给出 `s=` 和 `v=`。
- **`f=` 只接受 `100` / `32` / `24`。** frost 私有的 `png` / `jpeg` / `jpg` / `webp` / `rgb` / `rgba`
  别名已移除，随之**不再支持 JPEG 与 WebP 传输**（`f=100` 只解码 PNG）。
- **分号分隔的旧语法已移除。** `a=t;i=1;s=100;v=100;f=png` 这种 frost 私有写法不再被识别，
  只接受标准的 `G<控制对逗号分隔>;<base64>`。
- **原始像素长度必须精确匹配** `s*v*通道数`，以前允许多余尾字节。
- **`i=` 与 `I=` 互斥**，同时给出会被拒绝。
- **base64 更严格**：长度 `% 4 == 1` 或中间出现 `=` 会被拒绝（仍容忍空白与缺失/多余的尾部填充）。
- **续块只能带 `m=` 和可选的 `q=`**；带其它控制的「续块」会被当成新命令并中断原传输。
- **尺寸上限从 8192 提高到 16384**（像素总量仍受 64 MiB 解码预算约束）。
- **10 秒未完成传输超时已移除。** 这里不再有时钟：半截上传由终端复位（RIS，`ESC c`）和
  共享的在途字节总量上限兜底。
- `a=p` 引用不存在的图像会返回 `ENOENT`，而不是记下一个永远画不出来的放置。

## 安全说明

终端控制序列来自本地或远程程序，不能天然视为可信输入。frost 默认拒绝 OSC 52/5522 读取宿主剪贴板；如果显式开启 `allow_clipboard_read`，通过 SSH 运行的程序也可能获得剪贴板内容。剪贴板写入仍按主流终端兼容行为允许。Kitty 图像、OSC 8 链接和通知均有资源、协议或频率限制。

## 开发验证

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --all-features --locked
bash -n scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
shellcheck scripts/install.sh scripts/uninstall.sh scripts/test-install-paths.sh
bash scripts/test-install-paths.sh
desktop-file-validate data/io.github.beamiter.frost.desktop
appstreamcli validate --pedantic --no-net data/io.github.beamiter.frost.metainfo.xml
```

CI 对格式、零警告 Clippy、全量测试和 release 构建分别设有独立质量门槛；安装测试还会用
预编译 fixture 做一次真实 `DESTDIR` 安装/卸载往返，核对权限、桌面启动路径和全部资源文件。

调试构建可设置 `FROST_DEBUG=1` 输出有界的协议字节预览。

## 许可证

frost 以 **MIT OR Apache-2.0** 双许可证发布，使用者可任选其一；完整文本见
[`LICENSE-MIT`](LICENSE-MIT) 与 [`LICENSE-APACHE`](LICENSE-APACHE)。向本仓库提交
贡献即表示贡献者同意按相同的双许可证条款授权该贡献。

## Remote hosts and containers

Ctrl+Shift+S opens the remote host picker. An entry in the config file names an
ssh destination or a running container, and choosing one opens it in a new
session:

```toml
# An ssh destination…
[[remote_hosts]]
name = "dev"
host = "dev.example.com"
user = "yj"
deploy = "persist"
ssh_args = ["-p", "22"]

# …and a running container, reached with docker exec.
[[remote_hosts]]
name = "myubuntu"
host = "myubuntu"
docker = true
deploy = "persist"
```

These two are also what a config file with no `remote_hosts` key starts with:
the two mistakes the grammar cannot forgive are invisible in an empty list —
the port belongs in `ssh_args`, never as `host = "box:22"`, and the login
belongs in `user`, never as `host = "root@box"`. An explicit list wins,
`remote_hosts = []` included, so hosts deleted in the panel stay deleted.

The settings panel (Ctrl+Shift+O) has a Remote hosts section that adds,
edits and removes these entries in place; changes auto-save into the same
`[[remote_hosts]]` tables.

`deploy = "off"` (the default) connects plainly and runs `remote_shell`
(default `jsh`) as found on the destination. `"persist"` and `"incognito"`
bring jsh along through the family's `jsh-remote.sh`: when the local jsh is a
static build — which a Linux install now is — it lends itself, so nothing is
fetched from anywhere and the far side runs exactly the version that sent it.
Persist keeps jsh's dot-files and a cached binary in the destination's `$HOME`
so the next connection skips the transfer; incognito sandboxes `$HOME` and
deletes it on exit — inside a container the sandbox lives in its tmpfs, so
`docker diff` stays empty. An entry the config grammar rejects is shown in the
picker with its reason rather than hidden.

The grammar, validation and argv are shared with the whole jterm family
(`jterm_core::jsh_remote::RemoteHostConfig`); typing `ssh host` or
`docker exec -it name bash` into a jsh prompt reaches the same machinery with
no configuration at all.

The file sidebar (Ctrl+\, Files panel) browses these hosts natively — no
sshfs, nothing to install on the far side. A location picker above the tree
switches between Local and every configured entry (`ssh: …` / `docker: …`);
a remote tree is read one directory level at a time by running a small POSIX
sh probe script through `ssh` / `docker exec` stdin, with bounded output and
hard timeouts. Right-clicking a node (or the empty area below the tree, which
targets the root directory) opens a file-operations menu — New File, New
Folder, Rename, Delete (with a full-path confirmation), Copy, Cut, Copy Path,
Paste and Refresh — that works identically locally and remotely. Paste also
crosses locations: a remote entry pasted locally downloads, a local entry
pasted to a remote host uploads, and remote→remote relays through a unique
local temp path. Files stream (never buffered whole, 512 MiB cap, group-kill
on timeout) and directories travel as tar; the probe refuses an existing
directory target before extracting, and a cut across locations deletes
the source only after the copy completes. Transfers report live progress in
the panel (uploads show bytes against the file's size) and can be cancelled
from the same notice — a cancelled transfer never leaves a partial file in
place. Dropping files or folders from the OS file manager onto the tree
imports them — onto a directory row into that directory, anywhere else into
the current root; remote locations go through the same upload channel with
progress and cancel. A drop is capped at 256 items and the transfer size
limit, and existing names are refused per item, never overwritten. Remote
failures surface inline in the panel rather than taking the tree down.
