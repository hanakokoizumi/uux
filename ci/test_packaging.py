import importlib.util
from pathlib import Path
import tempfile
import unittest


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).parent / filename)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


packages = module('packages', 'package-linux.py')
release = module('release', 'release-files.py')


class PackagingTests(unittest.TestCase):
    def test_tag_must_match_manifest(self):
        release.validate_tag('0.2.0', 'v0.2.0')
        release.validate_tag('0.3.0-rc.1', 'v0.3.0-rc.1')
        for tag in ['v0.1.0', 'main', 'v0.2.0;echo bad', '0.2.0']:
            with self.assertRaises(ValueError):
                release.validate_tag('0.2.0', tag)

    def test_every_architecture_has_every_package(self):
        names = release.expected('0.2.0')
        self.assertEqual(len(names), 17)
        for arch in packages.ARCHES:
            for suffix in release.SUFFIXES:
                self.assertIn(f'uux-0.2.0-linux-{arch}{suffix}', names)

    def test_private_libraries_and_desktop_files_install_in_correct_locations(self):
        with tempfile.TemporaryDirectory() as tmp:
            bundle = Path(tmp)
            for name in ['uux', 'lib/libavcodec.so.63', 'share/applications/uux.desktop',
                         'share/licenses/COPYING.LGPLv2.1', 'build.json']:
                path = bundle / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            for arch, (goarch, native) in packages.ARCHES.items():
                config = packages.configuration(bundle, arch, '0.2.0')
                self.assertEqual(config['arch'], goarch)
                self.assertEqual(config['rpm']['arch'], native)
                self.assertEqual(config['archlinux']['arch'], native)
                entries = {entry['dst']: entry for entry in config['contents']}
                self.assertEqual(entries['/usr/bin/uux']['src'], '/usr/lib/uux/uux')
                self.assertEqual(entries['/usr/bin/uux']['type'], 'symlink')
                self.assertIn('/usr/lib/uux/lib/libavcodec.so.63', entries)
                self.assertIn('/usr/share/applications/uux.desktop', entries)
                self.assertIn('/usr/share/licenses/uux/COPYING.LGPLv2.1', entries)
                self.assertEqual(entries['/usr/lib/uux/uux']['file_info']['mode'], 0o755)

    def test_i386_rpm_requires_32_bit_runtime_libraries(self):
        with tempfile.TemporaryDirectory() as tmp:
            config = packages.configuration(Path(tmp), 'i386', '0.2.0')
            dependencies = config['overrides']['rpm']['depends']
            self.assertIn('glibc(x86-32) >= 2.36', dependencies)
            self.assertIn('libwayland-client(x86-32)', dependencies)
            self.assertIn('ca-certificates', dependencies)
            self.assertNotIn('glibc >= 2.36', dependencies)


if __name__ == '__main__':
    unittest.main()
