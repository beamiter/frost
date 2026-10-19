#!/usr/bin/env python3
"""Temporary, fail-closed integration driver; excluded from the final product tree."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]

def replace_once(text, old, new):
    if new in text:
        return text
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f'Expected one integration anchor, found {count}: {old[:100]!r}')
    return text.replace(old, new, 1)

path = ROOT / 'src/main.rs'
text = path.read_text()
if 'mod vim9_ide;' not in text:
    text = replace_once(text, 'mod terminal_view;\n', 'mod terminal_view;\nmod vim9_ide;\n')
text = replace_once(text, '    env_logger::init();\n', '    env_logger::init();\n    if let Some(result) = vim9_ide::run_if_requested() {\n        return result;\n    }\n')
text = replace_once(text, 'enum Message {\n', 'enum Message {\n    OpenVim9Ide,\n')
anchor = '            Message::PtyOutput(id, fd, data, watch_stamp) => {\n'
text = replace_once(text, anchor, '            Message::OpenVim9Ide => {\n                vim9_ide::open_from_terminal(self);\n                Task::none()\n            }\n' + anchor)
text = replace_once(text, '        let bar = bar\n            .push(window_btn("—", Message::WindowMinimize))', '        let bar = bar\n            .push(window_btn("Vim9 IDE", Message::OpenVim9Ide))\n            .push(window_btn("—", Message::WindowMinimize))')
path.write_text(text)

old = '7abb4e1dbdd7f31dee4277e728227827ba1abb9d'
new = '5a34a710825900b897d56a27f6c7c806a7a925d5'
tracked = subprocess.check_output(['git', 'ls-files', '-z'], cwd=ROOT).decode().split('\0')
for name in tracked:
    path = ROOT / name
    if not name or name.startswith(('vendor/', '.vim9-review/', '.github/')) or not path.is_file():
        continue
    if path.suffix not in {'.toml', '.py', '.sh', '.rs', '.nix'} and path.name != 'Cargo.lock':
        continue
    if path.name == Path(__file__).name:
        continue
    content = path.read_text()
    if old in content:
        path.write_text(content.replace(old, new))
        print('Synchronized exact core source:', name)

path = ROOT / 'src/vim9_ide.rs'
text = path.read_text().replace('use std::sync::Arc;\n', '')
if 'mod native_tests;' not in text:
    text += '\n#[cfg(test)]\n#[path = "vim9_ide_native_tests.rs"]\nmod native_tests;\n'
path.write_text(text)
print('Vim9 workbench wiring applied')
