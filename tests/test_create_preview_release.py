"""Draft-publication guards using synthetic Git/source/package data and mock gh."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile


SCRIPT_ROOT = Path(__file__).resolve().parents[1]
STAGE_SCRIPT = SCRIPT_ROOT / "scripts/stage-preview.py"


class PreviewReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        # Windows TEMP may use an 8.3 alias; compare canonical fixture paths
        # consistently with the publisher's validated, resolved asset paths.
        self.base = Path(self.temporary.name).resolve()
        self.root = self.base / "source"
        self.root.mkdir()
        self.package = self.base / "package"
        self.assets = self.base / "staged"
        self.package.mkdir()
        self.version = "0.3.6"
        self.repo = "example/easy-mouse-connect"
        self.notes = self.root / "packaging/RELEASE-NOTES.md"
        self.write(".gitignore", "/artifacts/\ndocs/ignored-private.txt\n")
        self.write("apps/desktop/tauri.conf.json", json.dumps({"version": self.version}))
        self.write("Cargo.toml", f'[workspace.package]\nversion = "{self.version}"\n')
        self.write("package.json", json.dumps({"version": self.version}))
        self.write("package-lock.json", json.dumps({"version": self.version, "packages": {"": {"version": self.version}}}))
        self.write("packaging/START-HERE.txt", f"Synthetic {self.version} preview instructions.\n")
        self.write("packaging/RELEASE-NOTES.md", f"# Easy Mouse Connect {self.version} preview\n\nUnsigned Windows preview; synthetic notes.\n")
        self.write("README.md", "Synthetic public source.\n")
        self.write("docs/public.md", "Public fixture.\n")
        self.write("ui/app.js", "// Synthetic frontend.\n")
        self.copy_scripts()
        # The copied helper derives its repository root from its own __file__.
        self.git("init", "-q")
        self.git("config", "core.autocrlf", "false")
        self.commit()
        self.head = self.git("rev-parse", "HEAD").strip()
        for name in ["easy-mouse-connect.exe", f"EasyMouseConnect-{self.version}-preview-setup.exe"]:
            (self.package / name).write_bytes(b"synthetic executable fixture")
        shutil.copyfile(self.root / "packaging/START-HERE.txt", self.package / "START-HERE.txt")
        self.stage()
        specification = importlib.util.spec_from_file_location("preview_release_under_test", self.root / "scripts/create-preview-release.py")
        self.module = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(self.module)
        self.original_command = self.module.command
        self.gh_calls = []
        self.releases = []
        self.refs = []
        self.tags = {}
        self.fail_api = False
        self.fail_create = False

    def copy_scripts(self):
        (self.root / "scripts").mkdir()
        shutil.copyfile(SCRIPT_ROOT / "scripts/create-preview-release.py", self.root / "scripts/create-preview-release.py")
        shutil.copyfile(STAGE_SCRIPT, self.root / "scripts/stage-preview.py")

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def git(self, *args):
        result = subprocess.run(["git", *args], cwd=self.root, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def commit(self):
        self.git("add", ".")
        self.git("-c", "user.name=Synthetic Test", "-c", "user.email=synthetic@example.invalid",
                 "commit", "-q", "-m", "Synthetic test fixture")

    def stage(self):
        shutil.rmtree(self.assets, ignore_errors=True)
        result = subprocess.run([sys.executable, str(self.root / "scripts/stage-preview.py"),
                                 "--package", str(self.package), "--destination", str(self.assets)],
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)

    def manifest(self):
        lines = []
        for path in sorted(self.assets.iterdir()):
            if path.name != "SHA256SUMS.txt":
                lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}")
        (self.assets / "SHA256SUMS.txt").write_text("\n".join(lines) + "\n", encoding="ascii")

    def rewrite_source(self, change):
        snapshot = self.assets / f"Easy-Mouse-Connect-source-{self.version}-preview.zip"
        with zipfile.ZipFile(snapshot) as archive:
            entries = {name: archive.read(name) for name in archive.namelist()}
        change(entries)
        with zipfile.ZipFile(snapshot, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, content in entries.items():
                archive.writestr(name, content)
        self.manifest()

    def mock_gh(self, args, root):
        if args[0] != "gh":
            return self.original_command(args, root)
        self.gh_calls.append(args)
        if args[1:4] == ["api", "--paginate", "--slurp"]:
            if self.fail_api:
                return subprocess.CompletedProcess(args, 1, "", "Synthetic API failure")
            endpoint = args[4]
            if "/releases?" in endpoint:
                value = [self.releases]
            elif "/git/matching-refs/" in endpoint:
                value = [self.refs]
            elif "/git/tags/" in endpoint:
                value = [{"object": self.tags[endpoint.rsplit("/", 1)[-1]]}]
            else:
                self.fail("Unexpected gh API call: " + repr(args))
            return subprocess.CompletedProcess(args, 0, json.dumps(value), "")
        self.assertEqual(args[1:3], ["release", "create"])
        return subprocess.CompletedProcess(args, int(self.fail_create), "https://example.invalid/unpublished-draft\n", "")

    def run_publisher(self, **environment):
        env = {"GITHUB_ACTIONS": "", "GITHUB_REPOSITORY": self.repo, "GITHUB_SHA": self.head}
        env.update(environment)
        arguments = [str(self.root / "scripts/create-preview-release.py"), "--assets", str(self.assets),
                     "--notes-file", str(self.notes)]
        with patch.dict(os.environ, env), patch.object(sys, "argv", arguments), \
                patch.object(self.module, "command", self.mock_gh), contextlib.redirect_stdout(io.StringIO()):
            self.module.main()

    def refused(self, message, **environment):
        with self.assertRaisesRegex(ValueError, message):
            self.run_publisher(**environment)
        self.assertFalse(any(call[1:3] == ["release", "create"] for call in self.gh_calls))

    def test_verified_assets_create_only_draft_prerelease_at_exact_head(self):
        self.run_publisher()
        create = self.gh_calls[-1]
        self.assertEqual(create[:4], ["gh", "release", "create", "v0.3.6"])
        self.assertIn("--draft", create)
        self.assertIn("--prerelease", create)
        self.assertEqual(create[create.index("--target") + 1], self.head)
        self.assertEqual(create[create.index("--title") + 1], "Easy Mouse Connect 0.3.6 preview")
        self.assertEqual(create[create.index("--notes-file") + 1], str(self.notes))
        self.assertFalse({"--clobber", "--force", "--latest"}.intersection(create))
        assets = [Path(value).name for value in create if value.startswith(str(self.assets))]
        self.assertEqual(set(assets), {path.name for path in self.assets.iterdir()})
        self.assertEqual(len(assets), 5)

    def test_mismatched_head_and_dirty_index_are_refused_before_github(self):
        self.refused("HEAD differs", GITHUB_SHA="0" * 40)
        self.write("README.md", "Uncommitted change.\n")
        self.git("add", "README.md")
        self.refused("dirty")
        self.assertFalse(self.gh_calls)

    def test_package_and_source_version_mismatch(self):
        self.write("Cargo.toml", '[workspace.package]\nversion = "0.3.7"\n')
        self.commit()
        self.head = self.git("rev-parse", "HEAD").strip()
        self.stage()
        self.refused("versions differ")

    def test_invalid_numeric_semver(self):
        self.write("apps/desktop/tauri.conf.json", json.dumps({"version": "00.3.6"}))
        self.commit()
        self.head = self.git("rev-parse", "HEAD").strip()
        self.refused("numeric semver")

    def test_corrupted_asset_hash_and_unexpected_file(self):
        (self.assets / "easy-mouse-connect.exe").write_bytes(b"different executable")
        self.refused("checksum mismatch")
        (self.assets / "private.txt").write_bytes(b"do not upload")
        self.refused("exact required")

    def test_manifest_missing_duplicate_and_traversal_names(self):
        manifest = self.assets / "SHA256SUMS.txt"
        original = manifest.read_text(encoding="ascii")
        for text, message in [(original.splitlines()[0] + "\n", "does not cover"),
                              (original + original.splitlines()[0] + "\n", "duplicate"),
                              (original + "0" * 64 + "  ../private.txt\n", "Malformed")]:
            with self.subTest(message=message):
                manifest.write_text(text, encoding="ascii")
                self.refused(message)

    def test_source_bytes_and_source_version_must_match_checkout(self):
        self.rewrite_source(lambda entries: entries.__setitem__("Easy-Mouse-Connect/README.md", b"Altered public source.\n"))
        self.refused("Source ZIP bytes differ")
        self.stage()
        self.rewrite_source(lambda entries: entries.__setitem__("Easy-Mouse-Connect/apps/desktop/tauri.conf.json", json.dumps({"version": "0.3.5"}).encode()))
        self.refused("Source ZIP bytes differ")

    def test_private_material_and_ignored_file_are_never_uploaded(self):
        for name in ["docs/local-notes/private.md", "docs/.env", "docs/identity.dat",
                     "docs/credentials.pem", "docs/logs/private.txt"]:
            self.write(name, "PRIVATE-TEST-CANARY")
        self.stage()
        self.run_publisher()
        self.gh_calls.clear()
        self.write("docs/ignored-private.txt", "IGNORED-TEST-CANARY")
        self.stage()  # Existing staging traverses roots; publisher rejects ignored/untracked bytes.
        self.refused("sanitized tracked source")

    def test_private_and_untracked_zip_additions_rejected_even_with_correct_manifest(self):
        for name in ["Easy-Mouse-Connect/docs/identity.dat", "Easy-Mouse-Connect/docs/untracked.txt", "../escape.txt"]:
            with self.subTest(name=name):
                self.stage()
                self.rewrite_source(lambda entries: entries.__setitem__(name, b"PRIVATE-TEST-CANARY"))
                self.refused("sanitized tracked source")

    def test_missing_source_member_is_refused(self):
        self.rewrite_source(lambda entries: entries.pop("Easy-Mouse-Connect/README.md"))
        self.refused("sanitized tracked source")

    def test_duplicate_source_member_is_refused(self):
        snapshot = self.assets / f"Easy-Mouse-Connect-source-{self.version}-preview.zip"
        with zipfile.ZipFile(snapshot) as archive:
            entries = [(name, archive.read(name)) for name in archive.namelist()]
        with zipfile.ZipFile(snapshot, "w") as archive:
            for name, content in entries:
                archive.writestr(name, content)
            with self.assertWarns(UserWarning):
                archive.writestr(entries[0][0], entries[0][1])
        self.manifest()
        self.refused("duplicate names")

    def test_instructions_from_another_package_are_refused_even_with_valid_hash(self):
        (self.assets / "START-HERE.txt").write_bytes(b"Instructions from a different package.\n")
        self.manifest()
        self.refused("instructions differ")

    def test_release_notes_must_be_tracked_and_versioned(self):
        self.notes = self.base / "external.md"
        self.notes.write_text("# Easy Mouse Connect 0.3.6 preview\n", encoding="utf-8")
        self.refused("inside this checkout")
        self.notes = self.root / "packaging/RELEASE-NOTES.md"
        self.write("packaging/RELEASE-NOTES.md", "# Easy Mouse Connect 0.3.5 preview\n")
        self.commit()
        self.head = self.git("rev-parse", "HEAD").strip()
        self.refused("heading")

    def test_published_prerelease_and_existing_draft_are_never_replaced(self):
        for draft in [False, True]:
            with self.subTest(draft=draft):
                self.releases = [{"tag_name": "v0.3.6", "draft": draft, "prerelease": True, "assets": [{"name": "existing.exe"}]}]
                self.refused("existing " + ("draft" if draft else "published"))

    def test_local_and_remote_tag_head_mismatch(self):
        self.git("tag", "v0.3.6")
        self.write("README.md", "Later committed source.\n")
        self.commit()
        self.head = self.git("rev-parse", "HEAD").strip()
        self.stage()
        self.refused("local tag")
        self.git("tag", "-d", "v0.3.6")
        self.refs = [{"ref": "refs/tags/v0.3.6", "object": {"type": "commit", "sha": "f" * 40}}]
        self.refused("remote tag")

    def test_matching_annotated_tag_is_allowed_and_prefix_tag_is_ignored(self):
        self.refs = [{"ref": "refs/tags/v0.3.6-old", "object": {"type": "commit", "sha": "f" * 40}},
                     {"ref": "refs/tags/v0.3.6", "object": {"type": "tag", "sha": "e" * 40}}]
        self.tags["e" * 40] = {"type": "commit", "sha": self.head}
        self.run_publisher()
        self.assertEqual(self.gh_calls[-1][1:3], ["release", "create"])

    def test_api_failure_does_not_create_release(self):
        self.fail_api = True
        self.refused("API preflight failed")

    def test_head_is_rechecked_after_remote_preflight(self):
        original = self.mock_gh

        def race(args, root):
            result = original(args, root)
            if args[0] == "gh" and "/git/matching-refs/" in args[-1]:
                self.write("README.md", "Concurrent committed source.\n")
                self.commit()
            return result

        with patch.object(self, "mock_gh", side_effect=race):
            self.refused("HEAD differs")

    def test_manual_main_ci_gate(self):
        for event, ref in [("push", "refs/heads/main"), ("workflow_dispatch", "refs/heads/feature")]:
            with self.subTest(event=event, ref=ref):
                self.refused("manual main run", GITHUB_ACTIONS="true", GITHUB_EVENT_NAME=event, GITHUB_REF=ref)
        self.run_publisher(GITHUB_ACTIONS="true", GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_REF="refs/heads/main")

    def test_optional_build_info_must_match_current_run_and_be_hashed(self):
        info = {"version": self.version, "commit": self.head, "repository": self.repo,
                "run_id": "123", "run_attempt": "1"}
        (self.assets / "BUILD-INFO.json").write_text(json.dumps(info), encoding="utf-8")
        ci = {"GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF": "refs/heads/main",
              "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "1"}
        self.refused("does not cover", **ci)
        self.manifest()
        self.refused("exact CI run", **{**ci, "GITHUB_RUN_ID": "124"})
        self.run_publisher(**ci)

    def test_partial_draft_creation_failure_is_clear_without_retry(self):
        self.fail_create = True
        with self.assertRaisesRegex(ValueError, "unpublished partial draft may exist"):
            self.run_publisher()
        self.assertEqual(sum(call[1:3] == ["release", "create"] for call in self.gh_calls), 1)


if __name__ == "__main__":
    unittest.main()
