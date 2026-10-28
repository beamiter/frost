#!/usr/bin/env python3
"""Capture the real native shell with public shell integration and actual PTY input.

All content is a deterministic QA fixture. No task state or successful test
result is injected into the application. This does not certify hardware IME.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shlex
import subprocess
import time


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
    parser.add_argument("--runtime-wrapper", type=Path,
                        help="Task-local runtime loader; metadata still hashes the actual native binary")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--mode", choices=["block", "chat"], required=True)
    parser.add_argument("--scale", type=float, choices=[1.0, 1.25, 1.5], default=1.0)
    parser.add_argument("--companion-output", action="store_true", help="Capture quiet expanded companion retreating during real continuous stdout, then original-shell recovery")
    parser.add_argument("--vim-tools-only", action="store_true", help="Verify Tools against a real embedded Vim draft/cursor and undo state")
    parser.add_argument("--companion-only", action="store_true", help="Capture quiet native companion at16px font and dismiss its real bubble")
    parser.add_argument("--expanded-organism", action="store_true", help="Start the existing unique companion with its real state bubble expanded")
    parser.add_argument("--fixture", action="store_true", help="Show explicitly labelled rich component sample data")
    parser.add_argument("--chrome-font-size", type=float, default=14.0)
    parser.add_argument("--content-font-size", type=float, default=14.0)
    parser.add_argument("--font-probe-only", action="store_true", help="Capture actual shaped fixture roles without running the interaction matrix")
    args = parser.parse_args()
    if args.companion_only or args.companion_output:
        args.expanded_organism = True
        args.content_font_size = 16.0
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
        'font_family = "DejaVu Sans Mono"\n' + f'font_size = {args.content_font_size}\nchrome_font_size = {args.chrome_font_size}\n'
        'show_repo_strip = false\nbottom_bar = false\n' + f'ui_scale = {args.scale}\n'
    )
    if args.expanded_organism:
        config_file.write_text(config_file.read_text() + "ascii_organism_expanded = true\n")
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
               RUST_LOG="warn,frost::reference_tools=debug,frost::reference_organism=debug", SHELL="/bin/bash", WINIT_X11_SCALE_FACTOR="1")
    if args.vim_tools_only:
        vim_probe = out / "vim-state.json"
        vim_temp = out / "vim-state.json.tmp"
        vim_fixture = out / "vim-fixture.txt"
        vim_original = "first native line\nsecond native line\n"
        vim_fixture.write_text(vim_original)
        vimrc = out / "native-probe.vim"
        vq = lambda value: "'" + str(value).replace("'", "''") + "'"
        vimrc.write_text("set nocompatible nomore noswapfile viminfofile=NONE\n"
            + "let g:frost_sample_sequence=0\nfunction! FrostNativeSample(timer) abort\n"
            + "let g:frost_sample_sequence+=1\n"
            + "call writefile([json_encode({'sequence': g:frost_sample_sequence, 'pid': getpid(), 'buffer': bufnr(), 'window': win_getid(), 'mode': mode(1), 'line': line('.'), 'column': col('.'), 'tick': b:changedtick, 'modified': &modified, 'text': getline(1,'$'), 'file': expand('%:p')})], " + vq(vim_temp) + ")\n"
            + "call rename(" + vq(vim_temp) + ", " + vq(vim_probe) + ")\nendfunction\n"
            + "autocmd VimEnter * call timer_start(40, function('FrostNativeSample'), {'repeat': -1})\n")
        env["VIMINIT"] = "execute 'source ' . fnameescape(" + vq(vimrc) + ")"
    if args.fixture:
        env["JTERM_REFERENCE_FIXTURE"] = "1"
        if args.font_probe_only:
            env["JTERM_NATIVE_FONT_PROBE"] = str(out / "native-fonts.json")
    else:
        env.pop("JTERM_REFERENCE_FIXTURE", None)
    processes, logs = [], []
    result = {"mode": args.mode, "status": "running", "fixture": True, "rich_component_fixture": args.fixture,
              "native_backend": "Iced X11 / " + env.get("ICED_BACKEND", "software Vulkan"),
              "runtime_wrapper": str(args.runtime_wrapper.resolve()) if args.runtime_wrapper else None,
              "captures": [],
              "behavior": {}, "logical_reference_size": [1356, 704], "app_scale": args.scale,
              "configured_font_groups": {"main": args.content_font_size, "chrome": args.chrome_font_size},
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
        assert run(["xdotool", "getactivewindow"]).stdout.strip() == wid, "Native Frost lost keyboard focus before typing"
        run(["xdotool", "type", "--clearmodifiers", "--delay", "2", "--", value])

    def key(value):
        assert run(["xdotool", "getactivewindow"]).stdout.strip() == wid, "Native Frost lost keyboard focus before key input"
        run(["xdotool", "key", "--clearmodifiers", value])
        time.sleep(0.1)

    def click(x, y):
        run(["xdotool", "mousemove", "--window", wid, str(round(x * args.scale)), str(round(y * args.scale))])
        run(["xdotool", "click", "1"])
        time.sleep(0.25)

    def tools():
        click(675, 671)

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
        result["display"] = env["DISPLAY"]
        result["xvfb_pid"] = server.pid
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
        app_argv = [str(args.binary.resolve())]
        if args.runtime_wrapper:
            app_argv.insert(0, str(args.runtime_wrapper.resolve(strict=True)))
        app = spawn(app_argv, "frost")
        wid = wait_for(window, "native Frost window")
        result["initial_visible_frost_windows"] = run(["xdotool", "search", "--onlyvisible", "--class", "io.github.beamiter.frost"]).stdout.strip().splitlines()
        assert result["initial_visible_frost_windows"] == [wid], "Private X11 session contains multiple Frost windows"
        run(["xdotool", "windowactivate", "--sync", wid])
        run(["xdotool", "windowsize", "--sync", wid, str(round(1356 * args.scale)), str(round(704 * args.scale))])
        run(["xdotool", "mousemove", "--window", wid, "600", "330"])
        time.sleep(2)
        assert "[Config] Cannot read" not in (out / "frost.log").read_text(), "Fixture config was refused"
        capture("empty-1356", wid)
        time.sleep(3)
        capture("idle-organism", wid)
        if args.font_probe_only:
            assert args.fixture, "Native role measurement requires the labelled fixture"
            wait_for(lambda: (out / "native-fonts.json").exists(), "actual native shaped text probe")
            result["native_fonts"] = json.loads((out / "native-fonts.json").read_text())
            if args.chrome_font_size > 14:
                # At the measured 1.3x UI font the unavailable attachment
                # actions move into the same real Tools menu, freeing the row.
                click(495, 671)
                assert "toggle open=true" in (out / "frost.log").read_text(), "Compact Tools did not open"
                capture("compact-tools-menu", wid)
                key("Escape")
            result.update(status="pass", scope="native shaped fixture role measurement", chrome_font_size=args.chrome_font_size, content_font_size=args.content_font_size)
            return
        if args.companion_output:
            from PIL import ImageGrab
            import threading
            owner = out / "companion-shell-owner.txt"
            sizes = out / "companion-pty-sizes.txt"
            command("printf 'before:%s\\n' \"$$\" > " + shlex.quote(str(owner)) + "; stty size > " + shlex.quote(str(sizes)))
            wait_for(owner.exists, "original shell PID before companion stream")
            original_pid = owner.read_text().strip().split(":")[1]
            trace_offset = len((out / "frost.log").read_text())
            wait_for(lambda: "speech=Some" in (out / "frost.log").read_text()[trace_offset:], "expanded companion safely reappears after real prompt input")
            capture("companion-before-stream", wid)
            probe = out / "native-companion-output.py"
            progress, child, stopped = [out / name for name in ["native-progress.txt", "native-child.txt", "native-interrupted.txt"]]
            probe.write_text("import os,time\nfrom pathlib import Path\n"
                + "progress=Path(" + repr(str(progress)) + "); stopped=Path(" + repr(str(stopped)) + ")\n"
                + "Path(" + repr(str(child)) + ").write_text(str(os.getpid()))\n"
                + "print('Native companion avoidance fixture · real stdout',flush=True)\n"
                + "try:\n for index in range(1,401):\n  print(('companion stdout %04d | '%index)+'native cells '*7,flush=True)\n  progress.write_text(str(index))\n  time.sleep(0.02)\nexcept KeyboardInterrupt:\n stopped.write_text(str(os.getpid()))\n print('native stdout interrupted',flush=True)\n")
            fields = dict(value.split("=",1) for value in run(["xdotool","getwindowgeometry","--shell",wid]).stdout.splitlines() if "=" in value)
            wx,wy,ww,wh = [int(fields[k]) for k in ["X","Y","WIDTH","HEIGHT"]]
            bbox = (wx,wy,wx+ww,wy+wh)
            def sequence(name, count, trigger=None):
                folder=out/name; folder.mkdir(); records=[]; buffered=[]; action={}; start=time.perf_counter(); errors=[]
                worker=None
                for index in range(count):
                    time.sleep(max(0,start+index*0.05-time.perf_counter()))
                    if index==2 and trigger:
                        def invoke():
                            action["before_seconds"]=time.perf_counter()-start
                            try:trigger()
                            except Exception as error:errors.append(str(error))
                            action["after_seconds"]=time.perf_counter()-start
                        worker=threading.Thread(target=invoke);worker.start()
                    before=time.perf_counter()-start
                    pixels=ImageGrab.grab(bbox=bbox,xdisplay=env["DISPLAY"]).convert("RGB")
                    buffered.append((index,before,time.perf_counter()-start,pixels))
                if worker:worker.join(timeout=5);assert not worker.is_alive() and not errors, errors
                for index,before,after,pixels in buffered:
                    file=folder/("%03d.png"%index);pixels.save(file,compress_level=1)
                    records.append({"file":file.name,"capture_start_seconds":before,"capture_end_seconds":after,"png_sha256":hashlib.sha256(file.read_bytes()).hexdigest()})
                manifest={"event":name,"frames":records,"trigger":action,"requested_fps":20,"duration_seconds":buffered[-1][2],"logical_size":[1356,704],"terminal_font_size":16,"app_scale":1,"display_dpr":1,"binary_sha256":result["binary_sha256"],"binary_build_manifest":result["binary_build_manifest"],"scope":"actual native software pixels; supervising visual review pending"}
                (folder/"manifest.json").write_text(json.dumps(manifest,indent=2));return manifest
            quiet = sequence("quiet-to-stream",30,lambda: command("python3 -u " + shlex.quote(str(probe))))
            wait_for(lambda: progress.exists() and int(progress.read_text() or "0")>=100, "real stdout fills the native viewport")
            before_live_trace=len((out/"frost.log").read_text())
            filled=sequence("stream-filled-viewport",30)
            live_trace=(out/"frost.log").read_text()[before_live_trace:]
            assert "body=Rectangle" not in live_trace,"Companion was still painted over a filled native card"
            from PIL import Image
            raw_roi=(330,150,1000,592)
            hashes=[hashlib.sha256(Image.open(out/"stream-filled-viewport"/record["file"]).crop(raw_roi).tobytes()).hexdigest() for record in filled["frames"]]
            assert len(set(hashes))>1,"The live native viewport did not actually change during capture"
            count_before_stop=int(progress.read_text())
            interrupted=sequence("stream-native-stop",12,lambda:key("ctrl+c"))
            wait_for(stopped.exists,"actual foreground child acknowledges native interrupt")
            command("printf 'after:%s\\n' \"$$\" >> " + shlex.quote(str(owner)) + "; stty size >> " + shlex.quote(str(sizes)))
            wait_for(lambda: "after:" in owner.read_text(),"same shell accepts real input after stopping stdout")
            returned_pid=owner.read_text().splitlines()[-1].split(":")[1]
            assert original_pid==returned_pid,"Stopping stdout changed original shell identity"
            measured=sizes.read_text().splitlines();assert len(measured)==2 and measured[0]==measured[1],"Companion changed native PTY dimensions"
            capture("companion-stream-stopped-input-restored",wid)
            # A full raw grid cannot host the companion. Real shell clear makes
            # genuine default-cell space; the GUI never manufactures a blank card.
            trace_offset=len((out/"frost.log").read_text())
            command("clear")
            wait_for(lambda:"speech=Some" in (out/"frost.log").read_text()[trace_offset:],"companion returns only after real native clear creates safe space")
            capture("companion-safe-restored-after-clear",wid)
            result.update(status="pass",scope="quiet expanded → real continuous stdout through old body region/full viewport → real interrupt/same shell → safe return after actual clear",terminal_font_size=16)
            result["behavior"]["companion_stream_avoidance"]={"original_shell_pid":original_pid,"returned_shell_pid":returned_pid,"foreground_child_pid":child.read_text(),"interrupted_child_pid":stopped.read_text(),"stdout_lines_before_stop":count_before_stop,"pty_sizes":measured,"filled_viewport_body_paint_events":0,"filled_native_roi":raw_roi,"filled_native_hashes":hashes,"return_condition":"real shell clear; no native card extension","guard_source":"src/terminal_view.rs::organism_geometry: complete active card, native clip, cursor suffix and exact default-cell occupancy; organism_block::travel_band","frame_sequences":[quiet,filled,interrupted]}
            return
        if args.vim_tools_only:
            def vim_state():
                try: return json.loads(vim_probe.read_text())
                except (FileNotFoundError, json.JSONDecodeError): return {}
            returned = out / "vim-returned.txt"
            command("vim -- " + shlex.quote(str(vim_fixture)) + "; printf '%s\\n' \"$$\" > " + shlex.quote(str(returned)))
            wait_for(lambda: vim_state().get("file") == str(vim_fixture), "real shell handoff into embedded Vim")
            assert len(run(["xdotool", "search", "--onlyvisible", "--pid", str(app.pid)]).stdout.splitlines()) == 1, "Vim opened a separate app window"
            type_text("iVIM-UNSAVED")
            wait_for(lambda: vim_state().get("text", [""])[0] == "VIM-UNSAVEDfirst native line", "real unsaved Vim draft")
            baseline = vim_state()
            fields = ["pid", "buffer", "window", "mode", "line", "column", "tick", "modified", "text", "file"]
            stable = {key: baseline[key] for key in fields}
            capture("vim-before-tools", wid)
            tools()
            key("Down")
            key("Up")
            capture("vim-tools-open", wid)
            key("Escape")
            wait_for(lambda: vim_state().get("sequence",0) > baseline["sequence"] + 2, "fresh independent Vim state after menu dismissal")
            assert {key: vim_state()[key] for key in fields} == stable, "Tools navigation/Escape changed Vim draft or cursor"
            tools()
            click(1000, 100)
            time.sleep(0.2)
            assert {key: vim_state()[key] for key in fields} == stable, "Tools outside click changed Vim state"
            tools()
            key("Return")
            time.sleep(0.25)
            assert {key: vim_state()[key] for key in fields} == stable, "Tools confirmation leaked Enter into Vim"
            type_text("X")
            expected = ["VIM-UNSAVEDXfirst native line", "second native line"]
            wait_for(lambda: vim_state().get("text") == expected, "native editor focus restored and key inserted exactly once")
            resumed = vim_state()
            assert resumed["pid"] == baseline["pid"] and resumed["buffer"] == baseline["buffer"]
            assert resumed["column"] == baseline["column"] + 1
            assert vim_fixture.read_text() == vim_original, "An unsaved draft was unexpectedly written"
            capture("vim-restored-focus", wid)
            key("Escape")
            type_text("u")
            wait_for(lambda: vim_state().get("text") == vim_original.splitlines(), "original Vim undo history retained")
            key("ctrl+r")
            wait_for(lambda: vim_state().get("text") == expected, "original Vim redo retained")
            type_text(":qa!")
            key("Return")
            wait_for(returned.exists, "original shell resumes after embedded Vim exit")
            result.update(status="pass", scope="actual embedded Vim Tools input ownership, cursor/draft identity and undo/redo")
            result["behavior"]["embedded_vim_tools"] = {"before": stable, "resumed": resumed, "exact_single_key": "X", "undo_redo": True, "original_file_unchanged": True, "shell_resumed": returned.read_text().strip()}
            return
        if args.expanded_organism:
            capture("organism-expanded", wid)
            matches = re.findall(r"speech=Some\(Rectangle \{ x: ([0-9.]+), y: ([0-9.]+), width: ([0-9.]+), height: ([0-9.]+) \}\)", (out / "frost.log").read_text())
            assert matches, "No safe native companion speech surface was actually painted"
            sx, sy, sw, sh = map(float, matches[-1])
            click(sx + sw - 12, sy + 12)
            capture("organism-dismissed", wid)
            dismiss_trace = (out / "frost.log").read_text()
            dismiss_states = re.findall(r"companion-(?:before|after) open=(?:true|false) raw=(Rect \{[^}]+\}) position_rows=([^ ]+) following=(true|false)", dismiss_trace)
            assert len(dismiss_states) == 2 and dismiss_states[0] == dismiss_states[1], "Companion chrome dismissal changed native reading/follow intent"
            result["behavior"]["companion_dismiss_preserves_reading"] = dismiss_states
            result["behavior"]["native_unique_companion_speech_dismissed"] = {"speech_rect": [sx,sy,sw,sh], "provenance": "existing owner presentation; verified blank native suffix"}
        if args.companion_only:
            time.sleep(5)
            capture("unique-graphical-organism", wid)
            result.update(status="pass", scope="quiet native companion expanded/dismissed; no claim of full interaction matrix", terminal_font_size=16)
            return
        tools()
        capture("tools-popover", wid)
        key("Escape")
        tools()
        key("Down")
        key("Return")
        capture("tools-command-palette", wid)
        key("Escape")
        tools()
        key("Down")
        key("Down")
        key("Return")
        capture("tools-canvas-search", wid)
        key("Escape")
        sentinel = out / "input-sentinels.txt"
        command("printf 'native-input:%s:%s\\n' \"$$\" \"$(tty)\" >> " + shlex.quote(str(sentinel)))
        wait_for(sentinel.exists, "real PTY sentinel")
        lines = sentinel.read_text().splitlines()
        assert len(lines) == 1 and lines[0].startswith("native-input:")
        result["behavior"]["keyboard_input_exactly_once"] = lines
        command("printf '\\033]2;Canvas renderer workspace\\007'")
        command("bash /home/ubuntu/Documents/Codex/2026-10-28/task/design/reference-session-fixture.sh")
        capture("reference-1356", wid)
        # Files remains a real right-hand surface when left navigation closes.
        if args.mode == "chat":
            if args.fixture:
                click(925, 439)
                copied = run(["xclip", "-selection", "clipboard", "-o"]).stdout
                assert "fn viewport_offset" in copied and "let anchor = row;" in copied
                result["behavior"]["structured_diff_copy"] = True
                run(["xdotool", "mousemove", "--window", wid, "650", "350"])
                run(["xdotool", "click", "--repeat", "6", "--delay", "80", "5"])
                capture("rich-component-detail", wid)
                run(["xdotool", "click", "--repeat", "20", "--delay", "30", "4"])
            else:
                # The stable 80px raw budget removes three native rows from
                # the former 36px-tools-only grid. In the named fixture the
                # actual Native workspace row is now y230 (pixel-inspected).
                run(["xdotool", "mousemove", "--window", wid,
                     str(round(330 * args.scale)), str(round(230 * args.scale))])
                time.sleep(0.15)
                run(["xdotool", "mousedown", "1"])
                time.sleep(0.15)
                run(["xdotool", "mousemove", "--window", wid,
                     str(round(800 * args.scale)), str(round(230 * args.scale))])
                time.sleep(0.2)
                run(["xdotool", "mouseup", "1"])
                time.sleep(0.2)
                capture("native-cell-selection", wid)
                key("ctrl+shift+c")
                copied = run(["xclip", "-selection", "clipboard", "-o"]).stdout
                assert "Native workspace" in copied, copied
                result["behavior"]["native_cell_selection_copy"] = True
            (out / "copied-native-text.txt").write_text(copied)
        click(1201, 57)
        capture("files-inspector", wid)
        click(27, 73)
        capture("files-navigation-hidden", wid)
        run(["xdotool", "mousemove", "--window", wid, str(round(1180 * args.scale)), str(round(180 * args.scale))])
        time.sleep(0.2)
        key("ctrl+f")
        type_text("Cargo")
        capture("files-filter-focus", wid)
        key("Escape")
        key("Escape")
        click(680, 500)
        command("printf 'files-focus:%s\\n' \"$$\" >> " + shlex.quote(str(sentinel)))
        wait_for(lambda: sentinel.read_text().count("files-focus:") == 1, "Files keys do not leak into shell")
        result["behavior"]["files_navigation_hidden_focus"] = True
        click(27, 73)
        click(1086, 57)
        # Switch presentation using the real native control and prove that the
        # same shell PID survives. Return to the requested capture mode.
        click(1214 if args.mode == "chat" else 1262, 20)
        command("printf 'mode-switch:%s\\n' \"$$\" >> " + shlex.quote(str(sentinel)))
        wait_for(lambda: sentinel.read_text().count("mode-switch:") == 1, "Mode switch preserves shell")
        switched_pid = next(line.split(":", 1)[1] for line in sentinel.read_text().splitlines() if line.startswith("mode-switch:"))
        assert switched_pid == lines[0].split(":")[1], "Mode switch replaced the PTY shell"
        result["behavior"]["mode_switch_same_shell"] = switched_pid
        click(1262 if args.mode == "chat" else 1214, 20)
        capture("mode-switch-restored", wid)
        # Real search/focus restoration: close the native input and prove a
        # subsequent command reaches the same shell, exactly once.
        key("ctrl+shift+f")
        type_text("canvas")
        capture("search-focus", wid)
        key("Escape")
        command("printf 'focus-restored:%s\\n' \"$$\" >> " + shlex.quote(str(sentinel)))
        wait_for(lambda: sentinel.read_text().count("focus-restored:") == 1, "focus restored to real shell")
        result["behavior"]["focus_restoration"] = True
        # Raw viewport reservation must remain stable across ordinary native
        # foreground ownership, rather than resizing on every Enter transition.
        size_before = out / "viewport-before.txt"
        command("stty size > " + shlex.quote(str(size_before)))
        wait_for(size_before.exists, "initial real PTY dimensions")
        initial_size = size_before.read_text().strip()
        read_value = out / "foreground-read.txt"
        type_text("read -r native_answer; printf '%s\\n' \"$native_answer\" > " + shlex.quote(str(read_value)))
        key("Return")
        time.sleep(0.35)
        capture("foreground-read", wid)
        type_text("native-read-input")
        key("Return")
        wait_for(read_value.exists, "foreground read receives native stdin")
        assert read_value.read_text() == "native-read-input\n"
        cat_value = out / "foreground-cat.txt"
        type_text("cat > " + shlex.quote(str(cat_value)))
        key("Return")
        time.sleep(0.35)
        # Modal menu arrows/Escape and confirming Terminal must not reach cat.
        # The exact stdin assertion below also rejects leaked Enter/releases.
        tools()
        key("Down")
        key("Up")
        key("Escape")
        tools()
        key("Return")
        type_text("native-cat-first")
        key("Return")
        type_text("native-cat-second")
        key("Return")
        capture("foreground-cat", wid)
        key("ctrl+d")
        wait_for(lambda: cat_value.exists() and cat_value.read_text() == "native-cat-first\nnative-cat-second\n",
                 "foreground cat preserves exact native stdin")
        size_after = out / "viewport-after.txt"
        command("stty size > " + shlex.quote(str(size_after)))
        wait_for(size_after.exists, "PTY dimensions after foreground commands")
        assert size_after.read_text().strip() == initial_size, "Prompt/Running changed PTY row budget"
        result["behavior"]["foreground_read_cat_exact_stdin"] = True
        result["behavior"]["tools_modal_keyboard_does_not_leak_to_pty"] = True
        result["behavior"]["raw_viewport_stable_prompt_running"] = initial_size
        # An actual alternate-screen child reports ioctl dimensions and accepts
        # real native keyboard input. Resize is verified from its SIGWINCH data.
        tui_script = out / "native-viewport-probe.py"
        tui_log, tui_ready, tui_input = out / "tui-size.jsonl", out / "tui-ready", out / "tui-key.bin"
        tui_script.write_text("import os,sys,json,signal,termios,tty\nfrom pathlib import Path\n"
            + "log=Path(" + repr(str(tui_log)) + "); ready=Path(" + repr(str(tui_ready)) + "); output=Path(" + repr(str(tui_input)) + ")\n"
            + "def draw(*args):\n size=os.get_terminal_size(0)\n with log.open('a') as f:f.write(json.dumps([size.lines,size.columns])+'\\n')\n sys.stdout.write('\\x1b[2J\\x1b[1;1HNative viewport probe · real TUI\\x1b[%d;1HLAST RAW ROW %d'%(size.lines,size.lines));sys.stdout.flush()\n"
            + "saved=termios.tcgetattr(0)\ntry:\n sys.stdout.write('\\x1b[?1049h\\x1b[40;97m');sys.stdout.flush()\n tty.setraw(0)\n signal.signal(signal.SIGWINCH,draw)\n draw()\n ready.touch()\n data=b''\n while len(data)<3:data+=os.read(0,3-len(data))\n output.write_bytes(data)\nfinally:\n termios.tcsetattr(0,termios.TCSANOW,saved)\n sys.stdout.write('\\x1b[0m\\x1b[?1049l');sys.stdout.flush()\n")
        type_text("python3 " + shlex.quote(str(tui_script)))
        key("Return")
        wait_for(tui_ready.exists, "real alternate-screen child")
        time.sleep(0.4)
        capture("raw-tui-1356", wid)
        original_tui_size = json.loads(tui_log.read_text().splitlines()[-1])
        run(["xdotool", "windowsize", "--sync", wid, "800", "704"])
        wait_for(lambda: json.loads(tui_log.read_text().splitlines()[-1])[1] != original_tui_size[1],
                 "TUI receives real PTY SIGWINCH resize")
        resized_tui_size = json.loads(tui_log.read_text().splitlines()[-1])
        capture("raw-tui-800", wid)
        key("Up")
        wait_for(tui_input.exists, "foreground TUI receives real keyboard")
        assert tui_input.read_bytes() == b"\x1b[A", tui_input.read_bytes().hex()
        run(["xdotool", "windowsize", "--sync", wid, "1356", "704"])
        time.sleep(0.4)
        restored_size = out / "viewport-restored.txt"
        command("stty size > " + shlex.quote(str(restored_size)))
        wait_for(restored_size.exists, "shell returns from TUI")
        assert restored_size.read_text().strip() == initial_size
        result["behavior"]["raw_tui_resize_and_keyboard"] = {"initial": original_tui_size, "resized": resized_tui_size, "key_hex": tui_input.read_bytes().hex()}
        # The native composer button must interrupt the displayed foreground
        # process rather than leaving a live sleep behind or sending twice.
        type_text("sleep 30")
        key("Return")
        time.sleep(0.5)
        capture("running-command", wid)
        if args.mode == "chat":
            click(999, 675)
        else:
            key("ctrl+c")
        time.sleep(0.35)
        stopped = out / "stop-resumed.txt"
        command("printf 'native-stop-resumed\\n' > " + shlex.quote(str(stopped)))
        wait_for(stopped.exists, "Native Stop restores shell within five seconds", timeout=5)
        assert stopped.read_text() == "native-stop-resumed\n"
        result["behavior"]["native_stop_foreground_sleep" if args.mode == "chat" else "keyboard_interrupt_foreground_sleep"] = True
        capture("stop-resumed", wid)
        # Native Unicode arrives through the clipboard protocol. IME preedit
        # itself is tested separately by native-ime-smoke.py.
        multiline = out / "multiline-input.txt"
        type_text("cat <<'FROST_EOF' > " + shlex.quote(str(multiline)))
        key("Return")
        for line in ["first native line", "second native line", "third native line", "fourth native line"]:
            type_text(line)
            key("Return")
        capture("multiline-native-input", wid)
        type_text("FROST_EOF")
        key("Return")
        wait_for(multiline.exists, "multiline heredoc reaches real shell")
        assert multiline.read_text() == "first native line\nsecond native line\nthird native line\nfourth native line\n"
        result["behavior"]["multiline_native_input"] = True
        wrap_text = "printf '%s\\n' '" + "native-wrap-" * 100 + "' > " + shlex.quote(str(out / "wrapped-input.txt"))
        type_text(wrap_text)
        capture("long-wrapped-input", wid)
        key("Return")
        wait_for((out / "wrapped-input.txt").exists, "wrapped input reaches shell")
        assert (out / "wrapped-input.txt").read_text() == "native-wrap-" * 100 + "\n"
        result["behavior"]["long_wrapped_native_input"] = True
        clip = spawn(["xclip", "-selection", "clipboard", "-quiet"], "unicode-clipboard", stdin=subprocess.PIPE)
        clip.stdin.write("printf '中文焦点\\n'\n".encode())
        clip.stdin.close()
        key("ctrl+shift+v")
        time.sleep(0.5)
        capture("unicode-paste", wid)
        command("for i in $(seq 1 240); do printf 'canvas line %03d: continuous native output\\n' \"$i\"; done")
        capture("long-content", wid)
        run(["xdotool", "mousemove", "--window", wid, "650", "300"])
        run(["xdotool", "click", "--repeat", "7", "--delay", "80", "4"])
        capture("history-scroll", wid)
        # A settled reading anchor and actual native viewport must survive
        # opening/dismissing a presentation-only popup with no output running.
        time.sleep(1.2)
        capture("tools-reading-before", wid)
        trace_start = len((out / "frost.log").read_text())
        tools()
        capture("tools-reading-open", wid)
        key("Escape")
        capture("tools-reading-after", wid)
        from PIL import Image
        # Menu begins x590; exclude its12px native shadow as well as its card.
        roi = (330, 150, 560, 570)
        crops = [Image.open(out / (name + ".png")).crop(roi).tobytes() for name in
                 ["tools-reading-before", "tools-reading-open", "tools-reading-after"]]
        assert len({hashlib.sha256(value).hexdigest() for value in crops}) == 1, "Tools moved uncovered native history pixels"
        trace = (out / "frost.log").read_text()[trace_start:]
        states = re.findall(r"(?:toggle|dismiss) open=(?:true|false) raw=(Rect \{[^}]+\}) position_rows=([^ ]+) following=(true|false)", trace)
        assert len(states) >= 2 and states[-1] == states[-2], "Tools changed raw allocation or live reading/follow intent"
        layouts = re.findall(r"native_surface canonical=(Rectangle \{[^}]+\})", trace)
        if args.mode == "chat":
            assert layouts and len(set(layouts)) == 1, "Tools changed the actual retained native layout rect"
        result["behavior"]["tools_reading_anchor_and_viewport"] = {"uncovered_roi": roi,
            "unchanged_pixel_sha256": hashlib.sha256(crops[0]).hexdigest(), "reading_states": states[-2:],
            "actual_native_rects": sorted(set(layouts)), "scope": "same viewport/menu open/Escape closed, settled history"}

        result["behavior"]["long_output_and_wheel"] = "native commands and wheel events captured for visual review"
        for width in [800, 620, 1600]:
            run(["xdotool", "windowsize", "--sync", wid, str(round(width * args.scale)), str(round(704 * args.scale))])
            time.sleep(0.4)
            capture(f"window-{width}", wid)
        split = out / "split-identities.txt"
        key("ctrl+shift+e")
        time.sleep(0.5)
        command("printf 'split:%s\\n' \"$$\" >> " + shlex.quote(str(split)))
        wait_for(split.exists, "new split real PTY")
        split_pid = split.read_text().splitlines()[0].split(":")[1]
        assert split_pid != lines[0].split(":")[1], "Split did not create a different PTY"
        capture("native-split", wid)
        key("ctrl+alt+Left")
        command("printf 'returned:%s\\n' \"$$\" >> " + shlex.quote(str(split)))
        wait_for(lambda: "returned:" in split.read_text(), "split keyboard focus")
        returned_pid = next(value.split(":")[1] for value in split.read_text().splitlines() if value.startswith("returned:"))
        assert returned_pid == lines[0].split(":")[1], "Split focus failed to return to original PTY"
        result["behavior"]["native_split_keyboard_focus"] = {"new_pid": split_pid, "returned_pid": returned_pid}
        assert app.poll() is None, "Frost exited during resize"
        result["status"] = "pass"
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
