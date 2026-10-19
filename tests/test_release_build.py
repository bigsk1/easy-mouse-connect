"""CI artifact provenance checks using synthetic, paginated GitHub API data."""

from copy import deepcopy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


SCRIPT_ROOT = Path(__file__).resolve().parents[1]


class ReleaseBuildTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        specification = importlib.util.spec_from_file_location(
            "release_build_under_test",
            SCRIPT_ROOT / "scripts/create-preview-release.py",
        )
        cls.module = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(cls.module)

    def setUp(self):
        self.root = SCRIPT_ROOT
        self.repo = "example/easy-mouse-connect"
        self.head = "a" * 40
        self.run_id = 783001
        self.attempt = 2
        self.workflow_id = 2305
        self.repository_id = 1042
        self.artifact_id = 9012
        self.artifact_name = f"easy-mouse-connect-windows-x64-unsigned-{self.head}"
        self.digest = "sha256:" + "b" * 64
        self.workflow_endpoint = f"repos/{self.repo}/actions/workflows/ci.yml"
        self.run_endpoint = f"repos/{self.repo}/actions/runs/{self.run_id}"
        self.main_endpoint = f"repos/{self.repo}/git/ref/heads/main"
        self.jobs_endpoint = (
            f"{self.run_endpoint}/attempts/{self.attempt}/jobs?per_page=100"
        )
        self.artifacts_endpoint = f"{self.run_endpoint}/artifacts?per_page=100"
        self.required_jobs = (
            "Core (ubuntu-latest)",
            "Core (windows-latest)",
            "Windows desktop and unsigned installer",
        )
        self.workflow = {"id": self.workflow_id, "path": ".github/workflows/ci.yml"}
        self.run = {
            "id": self.run_id,
            "workflow_id": self.workflow_id,
            "path": ".github/workflows/ci.yml",
            "status": "completed",
            "conclusion": "success",
            "event": "push",
            "head_branch": "main",
            "head_sha": self.head,
            "run_attempt": self.attempt,
            "repository": {"id": self.repository_id, "full_name": self.repo},
            "head_repository": {"id": self.repository_id, "full_name": self.repo},
        }
        self.main = {
            "ref": "refs/heads/main",
            "object": {"type": "commit", "sha": self.head},
        }
        self.jobs = [
            {"name": name, "status": "completed", "conclusion": "success"}
            for name in self.required_jobs
        ]
        self.artifact = {
            "id": self.artifact_id,
            "name": self.artifact_name,
            "size_in_bytes": 4096,
            "expired": False,
            "digest": self.digest,
            "workflow_run": {
                "id": self.run_id,
                "head_sha": self.head,
                "head_branch": "main",
                "repository_id": self.repository_id,
                "head_repository_id": self.repository_id,
            },
        }

    def pages(self):
        return {
            self.workflow_endpoint: [deepcopy(self.workflow)],
            self.run_endpoint: [deepcopy(self.run)],
            self.main_endpoint: [deepcopy(self.main)],
            self.jobs_endpoint: [{"jobs": deepcopy(self.jobs)}],
            self.artifacts_endpoint: [{"artifacts": [deepcopy(self.artifact)]}],
        }

    def check(self, pages, run_id=None):
        calls = []

        def api(root, endpoint):
            self.assertEqual(root, self.root)
            self.assertIn(endpoint, pages, f"Unexpected API endpoint: {endpoint}")
            calls.append(endpoint)
            return deepcopy(pages[endpoint])

        # An unexpected shell path must never turn a fixture test into a real
        # network request, download, or publication.
        with patch.object(self.module, "api", side_effect=api), patch.object(
            self.module, "command", side_effect=AssertionError("Unexpected command")
        ):
            result = self.module.checked_build(
                self.root,
                self.repo,
                str(self.run_id) if run_id is None else run_id,
                self.head,
            )
        return result, calls

    def assert_rejected(self, pages, run_id=None):
        with self.assertRaises(ValueError):
            self.check(pages, run_id)

    def assert_rejected_after_artifact_change(self, *, run_changes=None, main_changes=None):
        pages = self.pages()
        responses = {}

        def api(root, endpoint):
            self.assertEqual(root, self.root)
            self.assertIn(endpoint, pages, f"Unexpected API endpoint: {endpoint}")
            response = deepcopy(pages[endpoint])
            responses.setdefault(endpoint, []).append(response)
            if endpoint == self.artifacts_endpoint:
                # Earlier API replies remain independent snapshots. A helper
                # that never re-fetches cannot observe this fixture mutation.
                if run_changes is not None:
                    pages[self.run_endpoint][0].update(deepcopy(run_changes))
                if main_changes is not None:
                    pages[self.main_endpoint][0].update(deepcopy(main_changes))
            return response

        with patch.object(self.module, "api", side_effect=api), patch.object(
            self.module, "command", side_effect=AssertionError("Unexpected command")
        ):
            with self.assertRaises(ValueError):
                self.module.checked_build(self.root, self.repo, str(self.run_id), self.head)
        self.assertEqual(responses[self.run_endpoint][0][0], self.run)
        self.assertIsNot(responses[self.run_endpoint][0][0], pages[self.run_endpoint][0])
        self.assertEqual(len(responses[self.artifacts_endpoint]), 1)
        if run_changes is not None:
            self.assertGreaterEqual(len(responses[self.run_endpoint]), 2)
        if main_changes is not None:
            self.assertEqual(responses[self.main_endpoint][0][0], self.main)
            self.assertGreaterEqual(len(responses[self.main_endpoint]), 2)

    def test_completed_push_and_manual_builds_return_normalized_provenance(self):
        for event in ("push", "workflow_dispatch"):
            with self.subTest(event=event):
                pages = self.pages()
                pages[self.run_endpoint][0]["event"] = event
                result, calls = self.check(pages)
                self.assertEqual(result, {
                    "repository": self.repo,
                    "commit": self.head,
                    "run_id": self.run_id,
                    "run_attempt": self.attempt,
                    "workflow_id": self.workflow_id,
                    "artifact_id": self.artifact_id,
                    "artifact_name": self.artifact_name,
                    "artifact_digest": self.digest,
                })
                for key in ("run_id", "run_attempt", "workflow_id", "artifact_id"):
                    self.assertIs(type(result[key]), int)
                self.assertEqual(set(calls), set(pages))

    def test_required_jobs_and_matching_artifact_are_found_across_pages(self):
        pages = self.pages()
        pages[self.jobs_endpoint] = [
            {"jobs": [deepcopy(self.jobs[0])]},
            {"jobs": deepcopy(self.jobs[1:])},
        ]
        other_artifact = deepcopy(self.artifact)
        other_artifact.update(id=9013, name="unrelated-build-output")
        pages[self.artifacts_endpoint] = [
            {"artifacts": [other_artifact]},
            {"artifacts": [deepcopy(self.artifact)]},
        ]
        result, calls = self.check(pages)
        self.assertEqual(result["artifact_id"], self.artifact_id)
        self.assertIn(self.jobs_endpoint, calls)
        self.assertNotIn(f"{self.run_endpoint}/jobs?per_page=100", calls)

    def test_ci_run_accepts_bare_and_exact_main_qualified_workflow_paths(self):
        for suffix in ("", "@main", "@refs/heads/main"):
            with self.subTest(suffix=suffix):
                pages = self.pages()
                pages[self.run_endpoint][0]["path"] = ".github/workflows/ci.yml" + suffix
                result, _ = self.check(pages)
                self.assertEqual(result["workflow_id"], self.workflow_id)

    def test_ci_run_rejects_other_workflow_path_qualifiers(self):
        for suffix in (
            "@feature", "@refs/heads/feature", "@refs/pull/12/merge",
            "@refs/pull/12/head", "@refs/tags/v0.4.1", "@main/extra",
            "@refs/heads/main/extra", "@main@feature", "@main\n",
        ):
            with self.subTest(suffix=suffix):
                pages = self.pages()
                pages[self.run_endpoint][0]["path"] = ".github/workflows/ci.yml" + suffix
                self.assert_rejected(pages)

    def test_rerun_during_artifact_collection_cannot_reuse_old_attempt_jobs(self):
        for label, changes in (
            ("new attempt still running", {
                "run_attempt": self.attempt + 1,
                "status": "in_progress",
                "conclusion": None,
            }),
            ("new completed successful attempt at same SHA", {
                "run_attempt": self.attempt + 1,
            }),
        ):
            with self.subTest(case=label):
                self.assert_rejected_after_artifact_change(run_changes=changes)

    def test_run_status_and_authority_are_rechecked_after_artifact_collection(self):
        for label, changes in (
            ("success becomes failure", {"conclusion": "failure"}),
            ("repository changes", {"repository": {
                "id": self.repository_id + 1,
                "full_name": "other/easy-mouse-connect",
            }}),
            ("head repository changes", {"head_repository": {
                "id": self.repository_id + 1,
                "full_name": "fork/easy-mouse-connect",
            }}),
            ("workflow identity changes", {"workflow_id": self.workflow_id + 1}),
            ("workflow qualifier changes", {"path": ".github/workflows/ci.yml@feature"}),
            ("commit changes", {"head_sha": "c" * 40}),
        ):
            with self.subTest(case=label):
                self.assert_rejected_after_artifact_change(run_changes=changes)

    def test_remote_main_movement_during_artifact_collection_is_rejected(self):
        self.assert_rejected_after_artifact_change(main_changes={
            "object": {"type": "commit", "sha": "c" * 40},
        })

    def test_current_main_must_still_point_to_the_reviewed_commit(self):
        for label, change in (
            ("main moved", {"object": {"type": "commit", "sha": "c" * 40}}),
            ("tag instead of branch", {"ref": "refs/tags/v0.4.1"}),
            ("noncommit reference", {"object": {"type": "tag", "sha": self.head}}),
        ):
            with self.subTest(case=label):
                pages = self.pages()
                pages[self.main_endpoint][0].update(change)
                self.assert_rejected(pages)

    def test_pull_requests_tags_and_other_event_sources_are_rejected(self):
        for field, values in (
            ("event", ("pull_request", "pull_request_target", "release", "schedule")),
            ("head_branch", ("feature", "v0.4.1", "refs/tags/v0.4.1")),
        ):
            for value in values:
                with self.subTest(field=field, value=value):
                    pages = self.pages()
                    pages[self.run_endpoint][0][field] = value
                    self.assert_rejected(pages)

    def test_run_must_match_the_requested_run_commit_and_ci_workflow(self):
        for field, value in (
            ("id", self.run_id + 1),
            ("head_sha", "c" * 40),
            ("workflow_id", self.workflow_id + 1),
            ("path", ".github/workflows/other.yml"),
        ):
            with self.subTest(field=field):
                pages = self.pages()
                pages[self.run_endpoint][0][field] = value
                self.assert_rejected(pages)
        pages = self.pages()
        pages[self.workflow_endpoint][0]["path"] = ".github/workflows/other.yml"
        self.assert_rejected(pages)

    def test_forked_or_different_repository_runs_are_rejected(self):
        for container, field, value in (
            ("repository", "full_name", "other/easy-mouse-connect"),
            ("head_repository", "full_name", "fork/easy-mouse-connect"),
            ("head_repository", "id", self.repository_id + 1),
        ):
            with self.subTest(container=container, field=field):
                pages = self.pages()
                pages[self.run_endpoint][0][container][field] = value
                self.assert_rejected(pages)

    def test_run_must_be_completed_and_successful(self):
        for field, values in (
            ("status", ("queued", "in_progress", "waiting")),
            ("conclusion", (None, "failure", "cancelled", "skipped", "timed_out")),
        ):
            for value in values:
                with self.subTest(field=field, value=value):
                    pages = self.pages()
                    pages[self.run_endpoint][0][field] = value
                    self.assert_rejected(pages)

    def test_every_required_job_must_appear_exactly_once(self):
        for index, name in enumerate(self.required_jobs):
            for duplicate in (False, True):
                with self.subTest(job=name, duplicate=duplicate):
                    pages = self.pages()
                    if duplicate:
                        pages[self.jobs_endpoint].append({"jobs": [deepcopy(self.jobs[index])]})
                    else:
                        del pages[self.jobs_endpoint][0]["jobs"][index]
                    self.assert_rejected(pages)

    def test_failed_cancelled_skipped_or_incomplete_required_job_is_rejected(self):
        for index, name in enumerate(self.required_jobs):
            for field, value in (
                ("status", "in_progress"),
                ("conclusion", "failure"),
                ("conclusion", "cancelled"),
                ("conclusion", "skipped"),
                ("conclusion", None),
            ):
                with self.subTest(job=name, field=field, value=value):
                    pages = self.pages()
                    pages[self.jobs_endpoint][0]["jobs"][index][field] = value
                    self.assert_rejected(pages)

    def test_missing_or_ambiguous_matching_artifact_is_rejected(self):
        for label in ("missing", "wrong name", "duplicate across pages"):
            with self.subTest(case=label):
                pages = self.pages()
                if label == "missing":
                    pages[self.artifacts_endpoint] = [{"artifacts": []}]
                elif label == "wrong name":
                    pages[self.artifacts_endpoint][0]["artifacts"][0]["name"] = (
                        "easy-mouse-connect-windows-x64-unsigned-" + "c" * 40
                    )
                else:
                    duplicate = deepcopy(self.artifact)
                    duplicate["id"] += 1
                    pages[self.artifacts_endpoint].append({"artifacts": [duplicate]})
                self.assert_rejected(pages)

    def test_artifact_expiration_and_download_size_are_bounded(self):
        for field, value in (
            ("expired", True),
            ("size_in_bytes", 0),
            ("size_in_bytes", -1),
            ("size_in_bytes", 512 * 1024 * 1024 + 1),
        ):
            with self.subTest(field=field, value=value):
                pages = self.pages()
                pages[self.artifacts_endpoint][0]["artifacts"][0][field] = value
                self.assert_rejected(pages)
        pages = self.pages()
        pages[self.artifacts_endpoint][0]["artifacts"][0]["size_in_bytes"] = 512 * 1024 * 1024
        result, _ = self.check(pages)
        self.assertEqual(result["artifact_id"], self.artifact_id)

    def test_artifact_digest_requires_lowercase_sha256(self):
        for digest in (
            None, "", "b" * 64, "sha256:" + "b" * 63,
            "sha256:" + "B" * 64, "sha256:" + "g" * 64,
            "sha512:" + "b" * 64, self.digest + "\n",
        ):
            with self.subTest(digest=digest):
                pages = self.pages()
                pages[self.artifacts_endpoint][0]["artifacts"][0]["digest"] = digest
                self.assert_rejected(pages)

    def test_artifact_owner_must_match_run_commit_branch_and_both_repository_ids(self):
        for field, value in (
            ("id", self.run_id + 1),
            ("head_sha", "c" * 40),
            ("head_branch", "feature"),
            ("repository_id", self.repository_id + 1),
            ("head_repository_id", self.repository_id + 1),
        ):
            with self.subTest(field=field):
                pages = self.pages()
                owner = pages[self.artifacts_endpoint][0]["artifacts"][0]["workflow_run"]
                owner[field] = value
                self.assert_rejected(pages)

    def test_numeric_api_ids_and_attempts_must_be_positive_integers(self):
        for container, field in (
            ("workflow", "id"), ("run", "id"), ("run", "workflow_id"),
            ("run", "run_attempt"), ("repository", "id"),
            ("head_repository", "id"), ("artifact", "id"),
        ):
            for value in (0, -1, True, "123"):
                with self.subTest(container=container, field=field, value=value):
                    pages = self.pages()
                    if container == "workflow":
                        target = pages[self.workflow_endpoint][0]
                    elif container == "run":
                        target = pages[self.run_endpoint][0]
                    elif container == "artifact":
                        target = pages[self.artifacts_endpoint][0]["artifacts"][0]
                    else:
                        target = pages[self.run_endpoint][0][container]
                    target[field] = value
                    self.assert_rejected(pages)

    def test_nonnumeric_run_ids_are_rejected_without_query_interpolation(self):
        for run_id in ("", "abc", "-1", "0", "1/attempts/2", "1?per_page=100", "1\n"):
            with self.subTest(run_id=run_id):
                self.assert_rejected(self.pages(), run_id=run_id)


if __name__ == "__main__":
    unittest.main()
