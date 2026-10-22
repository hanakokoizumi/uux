#!/usr/bin/env python3
"""Reject incomplete release matrices and create an aggregate checksum manifest."""
import argparse
import hashlib
import re
from pathlib import Path
import tomllib

ARCHES = ('amd64', 'i386', 'aarch64')
SUFFIXES = ('.tar.gz', '.deb', '.rpm', '.pkg.tar.zst', '.nix.tar.xz')


def expected(version):
    return {f'uux-{version}-linux-{arch}{suffix}' for arch in ARCHES for suffix in SUFFIXES} | {
        'ffmpeg-9.0.2-source.tar.xz', f'uux-{version}-source.tar.gz',
    }


def validate_tag(version, tag):
    if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?', version):
        raise ValueError('Cargo package version must be a release or prerelease semver')
    if tag != 'v' + version:
        raise ValueError(f'tag {tag!r} must match Cargo.toml v{version}')


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--directory', type=Path)
    p.add_argument('--tag')
    a = p.parse_args()
    version = tomllib.loads(Path('Cargo.toml').read_text())['package']['version']
    if a.tag:
        validate_tag(version, a.tag)
    if a.directory:
        files = expected(version)
        present = {path.name for path in a.directory.iterdir() if path.is_file()}
        missing = files - present
        if missing:
            raise SystemExit('Missing release assets: ' + ', '.join(sorted(missing)))
        unexpected = present - files - {'SHA256SUMS'}
        if unexpected:
            raise SystemExit('Unexpected release assets: ' + ', '.join(sorted(unexpected)))
        lines = []
        for name in sorted(files):
            path = a.directory / name
            if path.stat().st_size == 0:
                raise SystemExit(f'Empty asset: {name}')
            with path.open('rb') as stream:
                digest = hashlib.file_digest(stream, 'sha256').hexdigest()
            lines.append(f'{digest}  {name}')
        (a.directory / 'SHA256SUMS').write_text('\n'.join(lines) + '\n')
    print(version)


if __name__ == '__main__':
    main()
