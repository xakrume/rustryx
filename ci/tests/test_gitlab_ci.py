"""Tests for ci/check-gitlab-ci.py (needs PyYAML, skipped without it)."""

import contextlib
import importlib.util
import io
import pathlib
import tempfile
import unittest

try:
    import yaml  # noqa: F401
except ImportError:  # pragma: no cover - environment dependent
    yaml = None

CI = pathlib.Path(__file__).resolve().parent.parent
ROOT = CI.parent

GOOD = """
stages: [lint, test, code-security, build, image-security, publish]
lint:
  stage: lint
  interruptible: true
  script: [echo]
test:
  stage: test
  interruptible: true
  script: [echo]
image-security:
  stage: image-security
  script: [echo]
"""


def load():
    spec = importlib.util.spec_from_file_location("check_gitlab_ci", CI / "check-gitlab-ci.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@unittest.skipIf(yaml is None, "PyYAML is not installed")
class GitlabCi(unittest.TestCase):
    def check(self, text: str) -> int:
        module = load()
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / ".gitlab-ci.yml"
            path.write_text(text, encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                return module.main(str(path))

    def test_committed_file_passes(self):
        self.assertEqual(self.check((ROOT / ".gitlab-ci.yml").read_text(encoding="utf-8")), 0)

    def test_minimal_valid_file_passes(self):
        self.assertEqual(self.check(GOOD), 0)

    def test_must_not_accept_a_job_in_an_undeclared_stage(self):
        self.assertEqual(self.check(GOOD + "extra:\n  stage: deploy\n  script: [echo]\n"), 1)

    def test_must_not_accept_a_missing_required_stage(self):
        self.assertEqual(self.check(GOOD.replace("publish]", "]").replace("[lint, test, code-security, build, image-security, ]", "[lint, test, code-security, build, image-security]")), 1)

    def test_must_not_accept_allow_failure_without_the_observation_marker(self):
        self.assertEqual(self.check(GOOD.replace("  script: [echo]\nimage", "  script: [echo]\n  allow_failure: true\nimage", 1)), 1)

    def test_must_not_let_image_security_fail(self):
        self.assertEqual(self.check(GOOD + "  allow_failure: true # OBSERVATION MODE\n"), 1)

    def test_must_not_accept_a_lint_job_that_cannot_be_cancelled(self):
        self.assertEqual(self.check(GOOD.replace("  interruptible: true\n", "", 1)), 1)


if __name__ == "__main__":
    unittest.main()
