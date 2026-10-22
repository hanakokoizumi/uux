#!/usr/bin/env python3
"""Bundle only UUX's private FFmpeg libraries; keep desktop drivers system-owned."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tomllib

p = argparse.ArgumentParser()
p.add_argument('--arch', choices=['amd64', 'i386', 'aarch64'], required=True)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--media', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
version = tomllib.loads(Path('Cargo.toml').read_text())['package']['version']
machine = {'amd64': 62, 'i386': 3, 'aarch64': 183}[a.arch]
header = a.binary.read_bytes()[:20]
assert header[:4] == b'\x7fELF' and int.from_bytes(header[18:20], 'little') == machine, 'wrong ELF architecture'
if a.output.exists():
    raise SystemExit(f'Output already exists: {a.output}')
(a.output / 'lib').mkdir(parents=True)
shutil.copy2(a.binary, a.output / 'uux')
for path in a.media.glob('lib/lib*.so*'):
    if path.is_file():
        # Copy SONAME aliases as regular files; Nix can patch each without dangling links.
        shutil.copy2(path, a.output / 'lib' / path.name)
zimg = subprocess.check_output(['pkg-config', '--variable=libdir', 'zimg'], text=True).strip()
for path in Path(zimg).glob('libzimg.so*'):
    shutil.copy2(path, a.output / 'lib' / path.name)
subprocess.run(['patchelf', '--set-rpath', '$ORIGIN/lib', str(a.output / 'uux')], check=True)
for path in (a.output / 'lib').iterdir():
    subprocess.run(['patchelf', '--set-rpath', '$ORIGIN', str(path)], check=True)
shutil.copytree(a.media / 'share/licenses', a.output / 'share/licenses')
if (a.media / 'include/ffnvcodec').is_dir():
    # These headers carry the MIT notices for the dynamically loaded NVDEC API.
    shutil.copytree(a.media / 'include/ffnvcodec', a.output / 'share/licenses/ffnvcodec')
for source, target in [('assets/uux.svg', 'share/icons/hicolor/scalable/apps/uux.svg'),
                       ('assets/uux.png', 'share/icons/hicolor/256x256/apps/uux.png'),
                       ('packaging/uux.desktop', 'share/applications/uux.desktop')]:
    dest = a.output / target
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, dest)
(a.output / 'build.json').write_text(json.dumps({
    'name': 'uux', 'version': version, 'architecture': a.arch,
    'glibc_minimum': '2.36', 'ffmpeg': '9.0.2',
}, indent=2) + '\n')
# Running without the builder's media path proves the relative RPATH is sufficient.
import os
env = dict(os.environ)
env.pop('LD_LIBRARY_PATH', None)
subprocess.run([str((a.output / 'uux').resolve()), '--help'], env=env, check=True)
