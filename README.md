# jterm3

jterm3 是一个面向 Linux 的现代终端模拟器，使用 Rust、iced 和 wgpu 构建。它把多标签、分屏、完整回滚搜索、会话恢复和 GPU 渲染放进一个轻量桌面应用，同时默认收紧远程终端可触达的宿主能力。

## 主要能力

- 多标签、拖动排序、快速标签切换，以及 tmux 风格的树状分屏（任意 pane 可再沿任一方向嵌套拆分）
- 搜索当前屏幕与全部 scrollback，支持大小写匹配、正则和自动滚动定位
- 查找替换面板（`Ctrl+Alt+R`）：scrollback 是只读输出，替换作用于当前选中文本——结果复制到剪贴板，或不带回车回填到提示符；支持字面/正则、大小写、全词与全部替换
- UTF-8、中文宽字符、True Color、256 色、鼠标报告、括号粘贴和扩展键盘协议
- Kitty 图像直接传输（`f=100` PNG、`f=24` RGB、`f=32` RGBA），协议的结构层与 jterm1/2/4 共用 `jterm_core::kitty_graphics`；带传输、像素、解压内存和放置数量上限，并对带 `i=`/`I=` 的命令回送 `OK` / `EINVAL` / `ENOTSUP` / `ENOENT` 应答
- 文件侧栏、路径插入、链接识别、命令面板、主题编辑和实时设置
- 文件侧栏按目录异步懒加载，支持返回上级与刷新；慢盘、NFS/FUSE 不再阻塞主界面
- 自动保存标签工作目录并恢复会话；多实例之间不会互相覆盖恢复数据
- OSC 10/11/12 动态颜色、OSC 52/5522 剪贴板和桌面通知
- OSC 133 shell 集成：沿命令提示符逐条跳转（`Ctrl+Shift+↑/↓`）、一键复制上一条命令输出（`Ctrl+Shift+G`），历史修剪时命令区保持对齐
- 持久化命令历史与模糊选择器（`Ctrl+Shift+H`）：完成的命令连同目录、退出码写入与 jterm1/jterm4 同格式的 JSONL 索引（从不保存输出），跨重启召回；Enter 只把选中命令回填到提示符，不自动执行
- 长命令完成桌面通知：OSC 133 计时超过阈值（默认 10 秒）且命令不在正被注视的 pane（窗口失焦或非活动 pane）时提醒
- 分屏 pane 标题栏显示所在目录的 git 分支与脏状态（后台探测并缓存，从不逐帧运行 git）
- 有界 PTY 输入/输出队列、稳定会话身份校验和繁忙进程关闭保护
- Shell Agent 提案始终逐条人工审批；只有绑定 pane 处于空闲且提示符输入为空时才可发送，
  命令完成结果按一次性执行代次精确关联，迟到或仅后缀相同的输出不会推进 Agent
- PTY 启动采用 fork→exec 错误握手；无效目录、shell/exec 失败会显示可重试诊断而不是崩溃
- 配置与快捷键热重载采用 last-known-good；坏文件会显示路径/行列并暂停自动写回

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
./target/release/jterm3
```

## 安装（含桌面集成）

```bash
./scripts/install.sh              # 构建并安装二进制 + 启动器条目
./scripts/install.sh --dry-run    # 只打印将要执行的命令，不改动文件
./scripts/install.sh --no-desktop # 只装二进制
./scripts/uninstall.sh            # 一并移除；配置与历史保留
```

默认装到 `~/.local`（可用 `--prefix` / `--bin-dir` 覆盖，打包场景用 `DESTDIR`）：

| 内容 | 位置 |
| --- | --- |
| 二进制 | `~/.local/bin/jterm3` |
| 启动器条目 | `~/.local/share/applications/io.github.beamiter.jterm3.desktop` |
| 图标 | `~/.local/share/icons/hicolor/{scalable,128x128,256x256}/apps/io.github.beamiter.jterm3.*` |
| AppStream 元数据 | `~/.local/share/metainfo/io.github.beamiter.jterm3.metainfo.xml` |

这套桌面集成才让 jterm3 出现在 GNOME/KDE 的应用列表里，可搜索、可点击启动、可固定到
dock。有三个细节决定它到底显不显示，安装脚本都已处理：

- `Exec=` / `TryExec=` 会被改写成二进制的绝对路径（`/usr` 这类系统 prefix 保留相对
  形式以便重定位）。桌面会话的 `PATH` 在登录时就固定了，若 `~/.local/bin` 不在其中，
  `TryExec=jterm3` 会失败并让条目**整个从应用列表消失**。
- 安装与卸载后都会刷新 `update-desktop-database` 和 `gtk-update-icon-cache`；陈旧的
  图标缓存会盖住刚装进去的图标。`DESTDIR` 打包时跳过，交给包管理器处理。
- `StartupWMClass` 为 `io.github.beamiter.jterm3`，与窗口真实的 `WM_CLASS` 一致。
  iced 把 `window::Settings` 里的 `platform_specific.application_id` 同时用作 X11
  `WM_CLASS` 与 Wayland app_id；不设置时两者都是空字符串，桌面环境无法把窗口关联到
  条目，dock 里只会出现一个没有图标、无法固定的窗口。

窗口本身也带图标：`data/io.github.beamiter.jterm3-128.png` 内嵌进二进制并在启动时交给
winit，因此即便直接 `cargo run`、或条目还没安装，`_NET_WM_ICON` 也是设好的——启动器条目
只能覆盖桌面环境能关联上的窗口。

可用 `desktop-file-validate <条目>` 与 `gtk-launch io.github.beamiter.jterm3` 自检。

也可以只手动安装二进制：

```bash
install -Dm755 target/release/jterm3 "$HOME/.local/bin/jterm3"
```

默认字体会优先使用 SauceCodePro Nerd Font；未安装时 iced/Fontconfig 会回退到系统字体。可以在设置面板中选择任意已安装的等宽字体。

## 常用快捷键

| 操作 | 快捷键 |
| --- | --- |
| 新建标签 | `Ctrl+Shift+T` |
| 复制 / 粘贴 | `Ctrl+Shift+C` / `Ctrl+Shift+V` |
| 搜索全部回滚 | `Ctrl+Shift+F` |
| 查找替换（选中文本） | `Ctrl+Alt+R`（替换结果进剪贴板或回填提示符，从不改写 scrollback） |
| 上/下一个命令提示符 | `Ctrl+Shift+↑` / `Ctrl+Shift+↓`（需 shell 发送 OSC 133 集成序列） |
| 复制上一条命令输出 | `Ctrl+Shift+G`（同样依赖 OSC 133） |
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
| 设置 | `Ctrl+Shift+O` |
| 临时放大 / 缩小 / 恢复配置字号 | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` |
| 窗口透明度增 / 减 | `Ctrl+Alt+=` / `Ctrl+Alt+-`（写回配置 `opacity`，设置面板也有滑块） |

快捷键从 `$XDG_CONFIG_HOME/jterm3/keybindings.toml`（通常是 `~/.config/jterm3/keybindings.toml`）加载，并与默认绑定合并。chord 语法与 jterm 家族共享（来自 `jterm_core`）：修饰键顺序任意，接受 `control`、`option`、`cmd`/`command`/`win`/`meta` 等修饰键别名，以及 `enter`/`return`、`esc`/`escape`、`arrowleft`/`left`、`page_up`/`pageup` 等按键别名；`ctrl++` 表示加号本身（也可写 `ctrl+plus`），`\` 可写作 `backslash`，非 ASCII 按键按 Unicode 大小写折叠匹配。

## 配置

主配置位于 `$XDG_CONFIG_HOME/jterm3/config.toml`。设置面板中的修改会自动保存，外部编辑也会热重载。示例：

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

# 持久化命令历史（与 jterm1/jterm4 同名键、同 JSONL 格式，可指向同一文件共享）
# 默认写入 ~/.local/state/jterm3/history.jsonl，只记录命令、目录、退出码与时间
command_history_enabled = true
# command_history_path = "~/.local/state/jterm3/history.jsonl"
command_history_max_entries = 10000

# AI / Agent（默认关闭；不开启则没有任何数据离开本机）
ai_enabled = false
ai_provider = "anthropic"   # anthropic | openai-compatible | ollama
ai_model = "claude-sonnet-4-6"
# 流式回复：Agent 面板边生成边显示模型回复（三家 provider 均支持）。
# 关闭则退回整段阻塞请求；两种方式记录到会话里的内容完全一致。
ai_stream = true
# API key 文件：一行 key、权限 600。未设置时在设置面板粘贴 key 即可，
# 会自动写入 ~/.config/jterm3/ai.key。环境变量 JTERM3_AI_API_KEY_FILE
# 优先于此项（与 jterm4 一致），且不会被写回配置文件。
# ai_api_key_file = "~/.config/jterm3/ai.key"
```

`Ctrl+=`、`Ctrl+-` 和 `Ctrl+滚轮` 只调整当前运行时字号，不再改写配置；`Ctrl+0` 回到 `font_size`。`ui_scale` 会统一缩放标签栏、状态栏、设置面板和命中区域。设置面板中的持久修改仍会自动保存。

如果 `config.toml` 或 `keybindings.toml` 编辑出错，jterm3 会保留最后一次可用配置并在窗口内显示诊断。主配置有错误时自动保存会暂停，避免默认值覆盖原文件；修正文件后会自动恢复热重载。

内置主题包括 Dark、Light、Monokai、Dracula、Nord、Gruvbox Dark、Tokyo Night、One Dark、Catppuccin Mocha 和 Solarized Light。自定义主题保存在 `~/.config/jterm3/themes/`。

## 安装与更新 jsh

jterm3 优先使用配套 shell [`jsh`](https://github.com/beamiter/jsh)，找不到才退回 bash。
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
和全部容量上限——现在来自 jterm1/2/3/4 共用的 `jterm_core::kitty_graphics`（预算取 `Caps::SCREEN`：
64 MiB 编码 / 64 MiB 解码 / 16384 像素边长 / 16 KiB 控制段 / 64 MiB 全部在途分块）。图像仓库、
放置、删除和协议应答仍留在 jterm3，因为它们需要解码器给出的尺寸和错误文本。

**升级后可见的行为变化：**

- **`x=` / `y=` 改回协议语义，图像位置会移动。** 以前 jterm3 把 `x=`/`y=` 当成屏幕列/行，
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
- **`f=` 只接受 `100` / `32` / `24`。** jterm3 私有的 `png` / `jpeg` / `jpg` / `webp` / `rgb` / `rgba`
  别名已移除，随之**不再支持 JPEG 与 WebP 传输**（`f=100` 只解码 PNG）。
- **分号分隔的旧语法已移除。** `a=t;i=1;s=100;v=100;f=png` 这种 jterm3 私有写法不再被识别，
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

终端控制序列来自本地或远程程序，不能天然视为可信输入。jterm3 默认拒绝 OSC 52/5522 读取宿主剪贴板；如果显式开启 `allow_clipboard_read`，通过 SSH 运行的程序也可能获得剪贴板内容。剪贴板写入仍按主流终端兼容行为允许。Kitty 图像和通知均有资源或频率限制。

## 开发验证

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --all-features --locked
```

CI 对格式、零警告 Clippy、全量测试和 release 构建分别设有独立质量门槛。

调试构建可设置 `JTERM3_DEBUG=1` 输出有界的协议字节预览。

## 许可证

jterm3 以 **MIT OR Apache-2.0** 双许可证发布，使用者可任选其一；完整文本见
[`LICENSE-MIT`](LICENSE-MIT) 与 [`LICENSE-APACHE`](LICENSE-APACHE)。向本仓库提交
贡献即表示贡献者同意按相同的双许可证条款授权该贡献。
