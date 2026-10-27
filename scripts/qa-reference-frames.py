#!/usr/bin/env python3
"""Capture the real native shell with public shell integration and actual PTY input.

All content is a deterministic QA fixture. No task state or successful test
result is injected into the application. This does not certify hardware IME.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import time
from PIL import ImageGrab


def wait_for(check, label, timeout=35):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError("Timed out: " + label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--mode", choices=["block", "chat"], required=True)
    parser.add_argument("--scale", type=float, choices=[1.0, 1.25, 1.5], default=1.0)
    parser.add_argument("--fixture", action="store_true", help="Show explicitly labelled rich component sample data")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    home, runtime = out / "home", out / "runtime"
    home.mkdir()
    runtime.mkdir(mode=0o700)
    config = home / ".config/frost"
    config.mkdir(parents=True)
    config_file = config / "config.toml"
    config_file.write_text(
        'shell = "/bin/bash"\nrestore_session = false\njsh_update_check = "never"\n'
        f'terminal_mode = "{args.mode}"\ninitial_width = 1356\ninitial_height = 704\n'
        'font_family = "DejaVu Sans Mono"\nfont_size = 12\nchrome_font_size = 10\n'
        'show_repo_strip = false\nbottom_bar = false\n' + f'ui_scale = {args.scale}\n'
    )
    for directory in [home, home / ".config", config]:
        directory.chmod(0o700)
    config_file.chmod(0o600)
    # Same public OSC 133 fixture used by Frost's native desktop smoke.
    home.joinpath(".bash_profile").write_text(r'''export HISTFILE=/dev/null
__frost_qa_prompt() {
  local status=$?
  if [[ ${__frost_qa_seen:-0} == 1 ]]; then printf '\e]133;D;%s\a' "$status"; fi
  __frost_qa_seen=1
}
PROMPT_COMMAND=__frost_qa_prompt
PS0=$'\e]133;C\a'
PS1='\[\e]133;A\a\]❯ \[\e]133;B\a\]'
''')
    env = os.environ.copy()
    for key in ["DISPLAY", "WAYLAND_DISPLAY", "SWAYSOCK"]:
        env.pop(key, None)
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"), XDG_DATA_HOME=str(home / ".local/share"),
               XDG_STATE_HOME=str(home / ".local/state"), XDG_CACHE_HOME=str(home / ".cache"),
               XDG_RUNTIME_DIR=str(runtime), LIBGL_ALWAYS_SOFTWARE="1", WGPU_BACKEND="vulkan",
               RUST_LOG="warn,frost::reference_tools=debug", SHELL="/bin/bash", WINIT_X11_SCALE_FACTOR="1")
    if args.fixture:
        env["JTERM_REFERENCE_FIXTURE"] = "1"
    else:
        env.pop("JTERM_REFERENCE_FIXTURE", None)
    processes, logs = [], []
    result = {"mode": args.mode, "status": "running", "fixture": True, "rich_component_fixture": args.fixture,
              "native_backend": "Iced X11 / software Vulkan", "captures": [],
              "behavior": {}, "logical_reference_size": [1356, 704], "app_scale": args.scale,
              "display_dpr": 1.0, "effective_pixel_scale": args.scale,
              "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
              "dirty_diff_sha256": hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD", "--binary"])).hexdigest(),
              "excluded": ["hardware GPU", "physical keyboard IME", "original JPEG pixel comparison"]}

    source_paths = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"]).decode().split("\0")
    source_digest = hashlib.sha256()
    for source_path in sorted(value for value in source_paths if value and not value.startswith("scripts/__pycache__/")):
        path = Path(source_path)
        if path.is_file():
            source_digest.update(source_path.encode() + b"\0" + path.read_bytes())
    result["source_tree_sha256"] = source_digest.hexdigest()
    sidecar = args.binary.with_suffix(".source-manifest.json")
    if sidecar.exists():
        manifest = json.loads(sidecar.read_text())
        result["binary_build_manifest"] = manifest
        assert manifest.get("binary_sha256") == result["binary_sha256"], "Binary does not match build manifest"

    def spawn(argv, name, **kwargs):
        log = open(out / (name + ".log"), "w")
        logs.append(log)
        process = subprocess.Popen(argv, env=env, stdout=log, stderr=log, **kwargs)
        processes.append(process)
        return process

    def run(argv):
        return subprocess.run(argv, env=env, text=True, capture_output=True, check=True, timeout=15)

    def window():
        response = subprocess.run(["xdotool", "search", "--onlyvisible", "--class", "io.github.beamiter.frost"],
                                  env=env, text=True, capture_output=True, timeout=3)
        return response.stdout.strip().splitlines()[-1] if response.returncode == 0 and response.stdout.strip() else None

    def capture(name, wid):
        run(["xdotool", "mousemove", "2390", "1590"])
        time.sleep(0.35)
        path = out / (name + ".png")
        run(["import", "-window", wid, str(path)])
        geometry = run(["xdotool", "getwindowgeometry", "--shell", wid]).stdout
        result["captures"].append({"file": path.name, "geometry": geometry})

    def type_text(value):
        run(["xdotool", "type", "--clearmodifiers", "--delay", "2", "--", value])

    def key(value):
        run(["xdotool", "key", "--clearmodifiers", value])
        time.sleep(0.1)

    def command(value):
        type_text(value)
        key("Return")
        time.sleep(0.35)

    try:
        read_fd, write_fd = os.pipe()
        server = spawn(["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "2400x1600x24", "-nolisten", "tcp"],
                       "xvfb", pass_fds=(write_fd,))
        os.close(write_fd)
        display = os.read(read_fd, 64).decode().strip()
        os.close(read_fd)
        assert display and server.poll() is None
        env["DISPLAY"] = ":" + display
        wait_for(lambda: subprocess.run(["xdpyinfo"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0,
                 "X11 readiness")
        wm_config = out / "openbox.xml"
        wm_config.write_text('<openbox_config xmlns="http://openbox.org/3.4/rc">'
                             '<focus><focusNew>yes</focusNew></focus>'
                             '<desktops><number>1</number></desktops>'
                             '<keyboard><chainQuitKey>C-g</chainQuitKey></keyboard>'
                             '<mouse/></openbox_config>')
        # Default Openbox owns Ctrl+Alt+Left for desktop switching. This private
        # fixture leaves application shortcuts to the actual focused client.
        spawn(["openbox", "--sm-disable", "--config-file", str(wm_config)], "openbox")
        result["window_manager_shortcuts"] = "private Openbox config without global keybindings"
        app = spawn([str(args.binary.resolve())], "frost")
        wid = wait_for(window, "native Frost window")
        run(["xdotool", "windowactivate", "--sync", wid])
        run(["xdotool", "windowsize", "--sync", wid, str(round(1356 * args.scale)), str(round(704 * args.scale))])
        run(["xdotool", "mousemove", "--window", wid, "600", "330"])
        time.sleep(2)
        assert "[Config] Cannot read" not in (out / "frost.log").read_text(), "Fixture config was refused"
        capture("before-sequences", wid)
        fields = dict(value.split("=", 1) for value in run(["xdotool", "getwindowgeometry", "--shell", wid]).stdout.splitlines() if "=" in value)
        x, y, width, height = [int(fields[key]) for key in ["X", "Y", "WIDTH", "HEIGHT"]]
        bbox = (x, y, x + width, y + height)
        assert [width, height] == [1356, 704]
        result["frame_sequences"] = []
        result["scope"] = "Actual native software-rendered pixels at 20fps; sampled frame evidence, visual review pending"
        def sequence(name, event, trigger):
            folder = out / name
            folder.mkdir()
            start = time.perf_counter()
            frames = []
            buffered = []
            ImageGrab.grab(bbox=bbox, xdisplay=env["DISPLAY"])
            start = time.perf_counter()
            trigger_record = {}
            for index in range(12):
                time.sleep(max(0.0, start + index * 0.05 - time.perf_counter()))
                if index == 2:
                    trigger_record["before_seconds"] = time.perf_counter() - start
                    trigger()
                    trigger_record["after_seconds"] = time.perf_counter() - start
                before = time.perf_counter() - start
                pixels = ImageGrab.grab(bbox=bbox, xdisplay=env["DISPLAY"]).convert("RGB")
                after = time.perf_counter() - start
                buffered.append((index, before, after, pixels))
            capture_duration = buffered[-1][2]
            for index, before, after, pixels in buffered:
                file = folder / f"{index:03d}.png"
                pixels.save(file, compress_level=1)
                center = pixels.crop((300, 91, 1040, 610)).resize((74, 52))
                bright = sum(all(c > 200 for c in value) for value in center.getdata()) / (74 * 52)
                frames.append({"file": file.name, "capture_start_seconds": before, "capture_end_seconds": after,
                               "png_sha256": hashlib.sha256(file.read_bytes()).hexdigest(),
                               "central_bright_fraction": bright, "rail_sample_rgb": pixels.getpixel((10, 350))})
            manifest = {"event": event, "trigger": trigger_record, "frames": frames,
                        "window_bbox_physical": bbox, "logical_size": [1356,704], "display_dpr": 1,
                        "app_scale": 1, "requested_fps": 20, "duration_seconds": capture_duration, "capture_plus_encoding_seconds": time.perf_counter() - start,
                        "binary_sha256": result["binary_sha256"], "binary_build_manifest": result["binary_build_manifest"],
                        "status": "captured; awaiting visual flash review"}
            (folder / "manifest.json").write_text(json.dumps(manifest, indent=2))
            result["frame_sequences"].append({"directory": name, "frames": len(frames),
                "duration_seconds": manifest["duration_seconds"], "min_central_bright_fraction": min(frame["central_bright_fraction"] for frame in frames)})
        type_text("printf 'frame-enter\\n'")
        sequence("enter", "real keyboard Enter submits existing native draft", lambda: run(["xdotool", "key", "--clearmodifiers", "Return"]))
        type_text("for i in $(seq 1 60); do printf 'frame-output %03d\\n' \"$i\"; sleep 0.01; done")
        sequence("output-burst", "real native PTY output burst after Enter", lambda: run(["xdotool", "key", "--clearmodifiers", "Return"]))
        time.sleep(1.2)
        command("for i in $(seq 1 240); do printf 'retained frame line %03d\\n' \"$i\"; done")
        run(["xdotool", "mousemove", "--window", wid, "650", "300"])
        sequence("wheel", "six real wheel-up events while reading retained output", lambda: spawn(["xdotool", "click", "--repeat", "6", "--delay", "55", "4"], "wheel-trigger"))
        time.sleep(1.2)
        run(["xdotool", "mousemove", "--window", wid, "675", "671"])
        sequence("tools-open", "real Tools click while retaining native history", lambda: run(["xdotool", "click", "1"]))
        sequence("tools-close", "real Escape dismisses Tools without moving native history", lambda: run(["xdotool", "key", "--clearmodifiers", "Escape"]))
        run(["xdotool", "mousemove", "--window", wid, "1300", "20"])
        sequence("panel-toggle", "real inspector toolbar toggle", lambda: run(["xdotool", "click", "1"]))
        assert app.poll() is None
        result["status"] = "captured"
    except Exception as error:
        result.update(status="fail", error=str(error))
        try:
            wid = window()
            if wid:
                capture("failure", wid)
        except Exception:
            pass
        raise
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
        for process in reversed(processes):
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        for log in logs:
            log.close()
        (out / "result.json").write_text(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
