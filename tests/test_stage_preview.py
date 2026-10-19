"""Source-publication safeguards using an isolated, entirely synthetic package."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile


class StagePreviewTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "source"
        self.package = Path(self.temporary.name) / "package"
        self.destination = Path(self.temporary.name) / "staged"
        self.script = self.root / "scripts/stage-preview.py"
        self.script.parent.mkdir(parents=True)
        shutil.copyfile(Path(__file__).resolve().parents[1] / "scripts/stage-preview.py", self.script)
        self.write("apps/desktop/tauri.conf.json", json.dumps({"version": "0.3.5"}))
        self.write("README.md", "Synthetic source fixture.\n")
        self.write("CHANGELOG.md", "Synthetic release notes.\n")
        self.write("ui/app.js", "// Synthetic UI fixture.\n")
        self.package.mkdir()
        self.package_files = {
            "easy-mouse-connect.exe": b"synthetic portable fixture",
            "EasyMouseConnect-0.3.5-preview-setup.exe": b"synthetic installer fixture",
            "START-HERE.txt": b"synthetic instructions\n",
        }
        for name, contents in self.package_files.items():
            (self.package / name).write_bytes(contents)

    def write(self, name, contents):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")

    def stage(self):
        return subprocess.run(
            [sys.executable, str(self.script), "--package", str(self.package),
             "--destination", str(self.destination)],
            capture_output=True, text=True, check=False,
        )

    def test_private_material_excluded_optional_notices_and_source_preserved(self):
        private_paths = [
            ".codex/config.toml", ".agents/local.md", ".aws/credentials",
            "local-notes/private.md", "docs/local-notes/private.md",
            "docs/.CoDeX/config.toml", "apps/.agents/local.md", "scripts/.aws/credentials",
            "ui/node_modules/private.txt", "apps/desktop/gen/private.json",
            "docs/runtime/profile.json", "tests/received/private.txt",
            "docs/logs/summary.json", "docs/diagnostic-exports/archive.zip",
            "docs/clipboard-images/private.png", "tests/CLIPBOARD-IMAGES/private.png",
            "docs/.env", "scripts/.env.local", "docs/IDENTITY.DAT",
            "tests/identity.dat.backup", "docs/emc-diagnostics.zip",
            "docs/EMC-DIAGNOSTICS-old.ZIP", "packaging/signing.PFX",
            "packaging/signing.p12", "docs/private.key", "docs/private.pem",
            "docs/private.dpapi", "docs/session.log", "docs/private.tmp",
        ]
        for name in private_paths:
            self.write(name, "PRIVATE-STAGING-CANARY")
        optional_notices = ["LICENSE", "LICENSE.txt", "NOTICE", "NOTICE.txt"]
        for name in optional_notices:
            self.write(name, "Synthetic optional notice fixture.\n")
        self.write("docs/public.md", "Ordinary documentation remains included.\n")

        result = self.stage()
        self.assertEqual(result.returncode, 0, result.stderr)
        snapshot = self.destination / "Easy-Mouse-Connect-source-0.3.5-preview.zip"
        with zipfile.ZipFile(snapshot) as archive:
            self.assertIsNone(archive.testzip())
            names = set(archive.namelist())
            self.assertTrue({"Easy-Mouse-Connect/" + name for name in optional_notices} <= names)
            for name in ["README.md", "CHANGELOG.md", "ui/app.js", "docs/public.md", "apps/desktop/tauri.conf.json"]:
                self.assertEqual(archive.read("Easy-Mouse-Connect/" + name), (self.root / name).read_bytes())
            for name in private_paths:
                self.assertNotIn("Easy-Mouse-Connect/" + name, names)
            for name in names:
                self.assertNotIn(b"PRIVATE-STAGING-CANARY", archive.read(name))
                self.assertTrue(name.startswith("Easy-Mouse-Connect/"))
                self.assertNotIn("..", Path(name).parts)
        for name, payload in self.package_files.items():
            self.assertEqual((self.destination / name).read_bytes(), payload)
        manifest = (self.destination / "SHA256SUMS.txt").read_text(encoding="ascii").splitlines()
        self.assertEqual(len(manifest), 4)
        for line in manifest:
            expected, name = line.split("  ", 1)
            self.assertEqual(hashlib.sha256((self.destination / name).read_bytes()).hexdigest(), expected)

    def test_existing_destination_is_not_overwritten(self):
        self.destination.mkdir()
        preserved = self.destination / "preserve.txt"
        preserved.write_bytes(b"existing destination fixture")
        result = self.stage()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Destination is not empty", result.stderr)
        self.assertEqual(preserved.read_bytes(), b"existing destination fixture")
        self.assertEqual(list(self.destination.iterdir()), [preserved])


if __name__ == "__main__":
    unittest.main()
