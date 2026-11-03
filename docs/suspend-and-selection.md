# 休眠恢复与终端框选

合盖后仅剩壁纸的问题对应 WGPU 画面提交连续返回 `SurfaceError::Other`。
旧路径只请求下一帧，继续使用失效的图形设备。现在会重建 compositor、各窗口的
renderer 和 surface，保留现有窗口、终端会话和 PTY。失败重试从 100 ms 退避至
最多 2 s；重新获得焦点时也请求刷新。

默认 Block 模式使用共用的输入区投影，但不启用 Chat 命令气泡。选区校验此前
重建显示参数时遗漏 `command_bubbles`，导致 Block 选区一直被判为失效。现在
按原投影参数校验，保留原有来源、版本及跨输入区的限制。输出和当前命令可拖选，
双击选词、三击选行及 Ctrl+Shift+C 继续使用原生选区。已完成命令的真实提示符行
保留单击选择命令块的行为；拖动则选择文字。按钮和合成标题仍各自执行其操作。

验证入口：

```sh
cargo build --release --locked --offline
python3 scripts/test-selection-admission.py
cargo test --locked --offline --manifest-path tools/native-feedback-harness/Cargo.toml
python3 scripts/native-selection-recovery.py --binary target/release/frost --output /tmp/frost-selection-recovery
```

原生测试使用独立 X11 桌面和合成剪贴板内容，并向测试进程注入 Vulkan 设备丢失。
它检查三个显示模式的拖选复制、恢复后的新画面，以及连续三次恢复时同一 shell/PTY
仍可执行命令。真实合盖、硬件休眠和锁屏解锁仍需在笔记本上做最终复测。

选区准入回归直接编译生产解析器、投影与选区代码，避开当前主测试目标中尚未迁移的
Iced `()` renderer 夹具；临时撤掉修复后，同一断言必须再次失败。
