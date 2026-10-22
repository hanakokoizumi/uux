#!/usr/bin/env python3
"""Create distro packages from the verified, architecture-specific release bundle."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tarfile

ARCHES = {
    'amd64': ('amd64', 'x86_64'),
    'i386': ('386', 'i686'),
    'aarch64': ('arm64', 'aarch64'),
}
DEPENDS = {
    'deb': ['libc6 (>= 2.36)', 'libgcc-s1', 'libstdc++6', 'libasound2', 'libpulse0',
            'libopus0', 'libx11-6', 'libxcursor1', 'libxrandr2', 'libxi6', 'libxcb1',
            'libxkbcommon0', 'libxkbcommon-x11-0', 'libwayland-client0', 'libegl1', 'libgl1', 'libva2',
            'libva-drm2', 'libdrm2', 'fontconfig', 'ca-certificates', 'xdg-utils'],
    'rpm': ['glibc >= 2.36', 'libgcc', 'libstdc++', 'alsa-lib', 'pulseaudio-libs',
            'opus', 'libX11', 'libXcursor', 'libXrandr', 'libXi', 'libxcb',
            'libxkbcommon', 'libxkbcommon-x11', 'libwayland-client', 'mesa-libEGL', 'mesa-libGL', 'libva',
            'libdrm', 'fontconfig', 'ca-certificates', 'xdg-utils'],
    'archlinux': ['glibc>=2.36', 'gcc-libs', 'alsa-lib', 'libpulse', 'opus',
                  'libx11', 'libxcursor', 'libxrandr', 'libxi', 'libxcb',
                  'libxkbcommon', 'libxkbcommon-x11', 'wayland', 'libglvnd', 'libva', 'libdrm',
                  'fontconfig', 'ca-certificates', 'xdg-utils'],
}


def configuration(bundle, arch, version):
    goarch, native = ARCHES[arch]
    dependencies = {kind: list(values) for kind, values in DEPENDS.items()}
    if arch == 'i386':
        # RPM ISA dependencies prevent 64-bit libraries satisfying a 32-bit package.
        # https://rpm.org/docs/latest/manual/more_dependencies.html
        for i, dependency in enumerate(dependencies['rpm']):
            name, separator, constraint = dependency.partition(' ')
            if name not in {'ca-certificates', 'xdg-utils'}:
                dependencies['rpm'][i] = name + '(x86-32)' + separator + constraint
    contents = [{'src': '/usr/lib/uux/uux', 'dst': '/usr/bin/uux', 'type': 'symlink'}]
    for path in sorted(bundle.rglob('*')):
        if not path.is_file():
            continue
        relative = path.relative_to(bundle).as_posix()
        if relative.startswith('share/licenses/'):
            destination = '/usr/share/licenses/uux/' + relative.removeprefix('share/licenses/')
        elif relative.startswith('share/'):
            destination = '/usr/' + relative
        elif relative == 'build.json':
            destination = '/usr/share/doc/uux/build.json'
        else:
            destination = '/usr/lib/uux/' + relative
        contents.append({'src': str(path), 'dst': destination,
                         'file_info': {'mode': 0o755 if relative == 'uux' else 0o644}})
    return {
        'name': 'uux', 'arch': goarch, 'platform': 'linux', 'version': version,
        'release': '1', 'section': 'net', 'priority': 'optional',
        'maintainer': 'Hanako <i@hanako.me>',
        'description': 'Linux-native UU remote desktop client built with Rust and egui.',
        'license': 'LicenseRef-Unspecified',
        'contents': contents,
        'rpm': {'arch': native, 'packager': 'Hanako <i@hanako.me>'},
        'archlinux': {'arch': native, 'packager': 'Hanako <i@hanako.me>'},
        'overrides': {kind: {'depends': values} for kind, values in dependencies.items()},
    }


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--arch', choices=ARCHES, required=True)
    p.add_argument('--bundle', type=Path, required=True)
    p.add_argument('--output', type=Path, default=Path('dist/packages'))
    a = p.parse_args()
    metadata = json.loads((a.bundle / 'build.json').read_text())
    assert metadata['architecture'] == a.arch, 'bundle architecture mismatch'
    version = metadata['version']
    a.output.mkdir(parents=True, exist_ok=True)
    stem = f'uux-{version}-linux-{a.arch}'
    config = a.output / f'{a.arch}-nfpm.json'
    config.write_text(json.dumps(configuration(a.bundle.resolve(), a.arch, version), indent=2))
    try:
        for kind, suffix in [('deb', '.deb'), ('rpm', '.rpm'), ('archlinux', '.pkg.tar.zst')]:
            subprocess.run(['nfpm', 'package', '--config', str(config), '--packager', kind,
                            '--target', str(a.output / (stem + suffix))], check=True)
    finally:
        config.unlink()
    epoch = int(os.environ.get('SOURCE_DATE_EPOCH', '0'))
    def normalize(info):
        info.uid = info.gid = 0
        info.uname = info.gname = 'root'
        info.mtime = epoch
        return info
    with tarfile.open(a.output / (stem + '.tar.gz'), 'w:gz') as archive:
        archive.add(a.bundle, arcname=stem, filter=normalize)


if __name__ == '__main__':
    main()
