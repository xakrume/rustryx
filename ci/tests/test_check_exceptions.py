"""Tests for ci/check-exceptions.py. Run: python3 -I -m unittest discover -s ci/tests"""

import copy
import datetime
import importlib.util
import pathlib
import tomllib
import unittest

CI = pathlib.Path(__file__).resolve().parent.parent
ROOT = CI.parent
_spec = importlib.util.spec_from_file_location("check_exceptions", CI / "check-exceptions.py")
ce = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ce)

TODAY = datetime.date(2026, 10, 24)
RAW = (ROOT / "deny.toml").read_text(encoding="utf-8")
DENY = tomllib.loads(RAW)
AUDIT = tomllib.loads((ROOT / ".cargo" / "audit.toml").read_text(encoding="utf-8"))


def with_reason(reason: str) -> dict:
    deny = copy.deepcopy(DENY)
    deny["advisories"]["ignore"] = [{"id": "RUSTSEC-0000-0001", "reason": reason}]
    return deny


class RepositoryFiles(unittest.TestCase):
    def test_committed_files_pass(self):
        self.assertEqual(ce.run(str(ROOT / "deny.toml"), str(ROOT / ".cargo" / "audit.toml"), TODAY), [])


class AdvisoryRecords(unittest.TestCase):
    GOOD = "issue=RUSTRYX-1; until=2026-12-01; no reachable code path, see the linked issue"

    def test_complete_record_is_accepted(self):
        self.assertEqual(ce.check_advisory_records(with_reason(self.GOOD), TODAY), [])

    def test_must_not_accept_ignore_without_reason(self):
        self.assertTrue(ce.check_advisory_records(with_reason(""), TODAY))

    def test_must_not_accept_ignore_without_issue_or_expiry(self):
        self.assertTrue(ce.check_advisory_records(with_reason("not reachable in our usage, trust us"), TODAY))

    def test_must_not_accept_expired_exception(self):
        expired = "issue=RUSTRYX-1; until=2026-10-23; no reachable code path, see the linked issue"
        problems = ce.check_advisory_records(with_reason(expired), TODAY)
        self.assertTrue(any("expired" in p for p in problems))

    def test_must_not_accept_exception_longer_than_cap(self):
        far = "issue=RUSTRYX-1; until=2028-01-01; no reachable code path, see the linked issue"
        self.assertTrue(ce.check_advisory_records(with_reason(far), TODAY))


class IgnoreSync(unittest.TestCase):
    def test_must_not_accept_ignore_present_in_one_tool_only(self):
        deny = with_reason(AdvisoryRecords.GOOD)
        problems = ce.check_ignore_sync(deny, AUDIT)
        self.assertEqual(len(problems), 2)


class Licences(unittest.TestCase):
    def test_must_not_accept_widened_allowlist(self):
        deny = copy.deepcopy(DENY)
        deny["licenses"]["allow"] = deny["licenses"]["allow"] + ["GPL-3.0-only"]
        self.assertTrue(ce.check_licences(deny, RAW))

    def test_must_not_accept_copyleft_exception(self):
        deny = copy.deepcopy(DENY)
        deny["licenses"]["exceptions"] = [{"crate": "webpki-roots", "allow": ["LGPL-2.1-or-later"]}]
        self.assertTrue(any("copyleft" in p for p in ce.check_licences(deny, RAW)))

    def test_must_not_accept_exception_without_comment(self):
        deny = copy.deepcopy(DENY)
        deny["licenses"]["exceptions"] = [{"crate": "somecrate", "allow": ["CDLA-Permissive-2.0"]}]
        self.assertTrue(any("comment" in p for p in ce.check_licences(deny, RAW)))


class Sources(unittest.TestCase):
    def test_must_not_accept_git_source(self):
        deny = copy.deepcopy(DENY)
        deny["sources"]["allow-git"] = ["https://example.org/x.git"]
        self.assertTrue(ce.check_sources(deny))

    def test_must_not_accept_relaxed_unknown_git(self):
        deny = copy.deepcopy(DENY)
        deny["sources"]["unknown-git"] = "warn"
        self.assertTrue(ce.check_sources(deny))


class Skips(unittest.TestCase):
    def test_must_not_accept_skip_without_reason(self):
        deny = copy.deepcopy(DENY)
        deny["bans"]["skip"] = [{"crate": "syn@2"}]
        self.assertTrue(ce.check_skips(deny))


if __name__ == "__main__":
    unittest.main()
