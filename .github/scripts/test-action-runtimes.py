"""Keep workflow action references on the reviewed Node.js 24 release lines."""

from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = (ROOT / ".github/workflows/ci.yml", ROOT / ".github/workflows/release.yml")
NODE24_RELEASES = {
    "actions/checkout": "v5",
    "actions/cache/restore": "v5",
    "actions/cache/save": "v5",
    "actions/upload-artifact": "v6",
    "actions/download-artifact": "v7",
    "docker/setup-buildx-action": "v4",
    "docker/setup-docker-action": "v5",
    "docker/setup-compose-action": "v2",
    "devcontainers/ci": "v0.3",
}


def action_references(workflow):
    return re.findall(r"^\s*-?\s*uses:\s*([^\s#]+)", workflow.read_text(), re.MULTILINE)


class ActionRuntimeTests(unittest.TestCase):
    def test_all_workflow_actions_use_reviewed_node24_release_lines(self):
        seen = set()
        for workflow in WORKFLOWS:
            for reference in action_references(workflow):
                action, separator, release = reference.rpartition("@")
                self.assertTrue(separator, f"unversioned action in {workflow}: {reference}")
                self.assertIn(action, NODE24_RELEASES,
                              f"unaudited action in {workflow}: {reference}")
                self.assertEqual(release, NODE24_RELEASES[action],
                                 f"deprecated or unaudited runtime in {workflow}: {reference}")
                seen.add(action)
        self.assertEqual(seen, set(NODE24_RELEASES))

    def test_ci_and_release_cover_checkout_artifact_and_native_agent_paths(self):
        for workflow in WORKFLOWS:
            references = set(action_references(workflow))
            for reference in (
                "actions/checkout@v5",
                "actions/upload-artifact@v6",
                "actions/download-artifact@v7",
                "docker/setup-buildx-action@v4",
            ):
                self.assertIn(reference, references, f"{workflow} does not exercise {reference}")


if __name__ == "__main__":
    unittest.main()
