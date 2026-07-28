# jterm3

jterm3 是一个面向 Linux 的现代终端模拟器，使用 Rust、iced 和 wgpu 构建。它把多标签、分屏、完整回滚搜索、会话恢复和 GPU 渲染放进一个轻量桌面应用，同时默认收紧远程终端可触达的宿主能力。

## 主要能力

- 多标签、拖动排序、快速标签切换，以及 tmux 风格的树状分屏（任意 pane 可再沿任一方向嵌套拆分）
- 搜索当前屏幕与全部 scrollback，支持大小写匹配、正则和自动滚动定位
- UTF-8、中文宽字符、True Color、256 色、鼠标报告、括号粘贴和扩展键盘协议
- Kitty 图像直接传输（PNG、RGB、RGBA），带传输、像素、解压内存和放置数量上限
- 文件侧栏、路径插入、链接识别、命令面板、主题编辑和实时设置
- 文件侧栏按目录异步懒加载，支持返回上级与刷新；慢盘、NFS/FUSE 不再阻塞主界面
- 自动保存标签工作目录并恢复会话；多实例之间不会互相覆盖恢复数据
- OSC 10/11/12 动态颜色、OSC 52/5522 剪贴板和桌面通知
- OSC 133 shell 集成：沿命令提示符逐条跳转（`Ctrl+Shift+↑/↓`）、一键复制上一条命令输出（`Ctrl+Shift+G`），历史修剪时命令区保持对齐
- 长命令完成桌面通知：OSC 133 计时超过阈值（默认 10 秒）且命令不在正被注视的 pane（窗口失焦或非活动 pane）时提醒
- 分屏 pane 标题栏显示所在目录的 git 分支与脏状态（后台探测并缓存，从不逐帧运行 git）
- 有界 PTY 输入/输出队列、稳定会话身份校验和繁忙进程关闭保护
- PTY 启动采用 fork→exec 错误握手；无效目录、shell/exec 失败会显示可重试诊断而不是崩溃
- 配置与快捷键热重载采用 last-known-good；坏文件会显示路径/行列并暂停自动写回

## 构建与运行

目前支持 Linux。项目固定使用 Rust 1.97.0（`rust-toolchain.toml` 会由 rustup 自动选择），并需要 Fontconfig、Wayland/X11 和 OpenGL/EGL 的开发库。Ubuntu/Debian 可安装：

```bash
sudo apt-get install pkg-config libfontconfig1-dev libwayland-dev \
  libx11-dev libx11-xcb-dev libxcb1-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libxcursor-dev libxi-dev \
  libxrandr-dev libxkbcommon-dev libegl1-mesa-dev libgl1-mesa-dev
```

然后构建：

```bash
rustup toolchain install 1.97.0 --profile minimal --component rustfmt --component clippy
cargo build --release --locked
./target/release/jterm3
```

如需安装到当前用户：

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
| 上/下一个命令提示符 | `Ctrl+Shift+↑` / `Ctrl+Shift+↓`（需 shell 发送 OSC 133 集成序列） |
| 复制上一条命令输出 | `Ctrl+Shift+G`（同样依赖 OSC 133） |
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

快捷键从 `$XDG_CONFIG_HOME/jterm3/keybindings.toml`（通常是 `~/.config/jterm3/keybindings.toml`）加载，并与默认绑定合并。

## 配置

主配置位于 `$XDG_CONFIG_HOME/jterm3/config.toml`。设置面板中的修改会自动保存，外部编辑也会热重载。示例：

```toml
font_family = "JetBrains Mono Nerd Font"
font_size = 14.0
line_spacing = 1.0
padding = 2.0
ui_scale = 1.0
scrollback_lines = 20000
scroll_speed = 3
theme = "tokyo-night"
tab_position = "top"
restore_session = true
disable_alt_screen = false

# 可选：明确指定 shell
shell = "/bin/bash"

# 何时检查 rsh 是否有新版本：startup 每次启动联网 | daily（默认）复用缓存 | never 关闭
rsh_update_check = "daily"

# 安全默认值。开启后，SSH 中的程序也能读取宿主剪贴板。
allow_clipboard_read = false

# 长命令完成桌面通知（OSC 133 计时；正被注视的 pane 不提醒）
notify_long_blocks = true
notify_long_block_threshold_ms = 10000

# 分屏 pane 标题栏中的 git 分支/脏状态
show_repo_strip = true

# AI / Agent（默认关闭；不开启则没有任何数据离开本机）
ai_enabled = false
ai_provider = "anthropic"   # anthropic | openai-compatible | ollama
ai_model = "claude-sonnet-4-6"
# API key 文件：一行 key、权限 600。未设置时在设置面板粘贴 key 即可，
# 会自动写入 ~/.config/jterm3/ai.key。环境变量 JTERM3_AI_API_KEY_FILE
# 优先于此项（与 jterm4 一致），且不会被写回配置文件。
# ai_api_key_file = "~/.config/jterm3/ai.key"
```

`Ctrl+=`、`Ctrl+-` 和 `Ctrl+滚轮` 只调整当前运行时字号，不再改写配置；`Ctrl+0` 回到 `font_size`。`ui_scale` 会统一缩放标签栏、状态栏、设置面板和命中区域。设置面板中的持久修改仍会自动保存。

如果 `config.toml` 或 `keybindings.toml` 编辑出错，jterm3 会保留最后一次可用配置并在窗口内显示诊断。主配置有错误时自动保存会暂停，避免默认值覆盖原文件；修正文件后会自动恢复热重载。

内置主题包括 Dark、Light、Monokai、Dracula、Nord、Gruvbox Dark、Tokyo Night、One Dark、Catppuccin Mocha 和 Solarized Light。自定义主题保存在 `~/.config/jterm3/themes/`。

## 安装与更新 rsh

jterm3 优先使用配套 shell [`rsh`](https://github.com/beamiter/rsh)，找不到才退回 bash。
命令面板中的 **Install or update rsh** 会在独立会话里运行安装脚本：会话本身就是进度界面，
可以 Ctrl+C 中断，脚本结束后等待 Enter 再关闭，失败原因不会一闪而过。

安装脚本来自 rsh 仓库并内嵌在二进制里，因此一台从未装过 rsh 的机器也能引导；校验和验证、
`rename(2)` 原子替换（**运行中的 shell 不受影响，新会话才使用新版本**）、旧二进制回滚副本，
以及 `PATH` 被 `/usr/bin/rsh`（Debian 系的 BSD remote shell）遮蔽时的提示，都由脚本统一处理。

缺少 rsh 或有新版本时，标签栏下方出现一条可忽略的提示行。检查在后台线程进行、从不自动安装，
离线时保持静默。`rsh_update_check = "daily"`（默认）复用安装脚本自己的缓存
（`~/.cache/rsh/update-check.json`），同机同时开着多个 jterm 也只产生一次网络请求；
`"startup"` 每次启动都联网，`"never"` 关闭检查。

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
