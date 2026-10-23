"""Tests for ci/audit-gate.py. Run: python3 -I -m unittest discover -s ci/tests"""

import contextlib
import importlib.util
import io
import json
import pathlib
import sys
import tempfile
import unittest

CI = pathlib.Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("audit_gate", CI / "audit-gate.py")
ag = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ag)

HIGH = "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"
MEDIUM = "CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N"


def report(vulns=(), warnings=None, crates=485, advisories=1351) -> dict:
    return {
        "database": {"advisory-count": advisories, "last-commit": "0123456789abcdef"},
        "lockfile": {"dependency-count": crates},
        "vulnerabilities": {"list": [
            {"advisory": {"id": i, "title": "t", "cvss": c}, "package": {"name": "x", "version": "1.0.0"}}
            for i, c in vulns
        ]},
        "warnings": warnings or {},
    }


def run_gate(data, *extra) -> int:
    with tempfile.TemporaryDirectory() as tmp:
        path = pathlib.Path(tmp) / "audit.json"
        path.write_text(json.dumps(data) if not isinstance(data, str) else data, encoding="utf-8")
        argv = ["audit-gate.py", str(path), *extra]
        old = sys.argv
        sys.argv = argv
        try:
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                return ag.main()
        finally:
            sys.argv = old


class Gate(unittest.TestCase):
    def test_clean_report_passes(self):
        self.assertEqual(run_gate(report()), 0)

    def test_medium_passes_at_high_threshold(self):
        self.assertEqual(run_gate(report([("RUSTSEC-1", MEDIUM)])), 0)

    def test_high_blocks(self):
        self.assertEqual(run_gate(report([("RUSTSEC-1", HIGH)])), 1)

    def test_medium_blocks_at_medium_threshold(self):
        self.assertEqual(run_gate(report([("RUSTSEC-1", MEDIUM)]), "--fail-on", "medium"), 1)

    def test_must_not_pass_unscored_vulnerability(self):
        self.assertEqual(run_gate(report([("RUSTSEC-1", None)])), 1)

    def test_must_not_pass_cvss4_vector_it_cannot_score(self):
        self.assertEqual(run_gate(report([("RUSTSEC-1", "CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:L/VI:N/VA:N/SC:N/SI:N/SA:N")])), 1)

    def test_unmaintained_warning_does_not_block(self):
        warn = {"unmaintained": [{"advisory": {"id": "RUSTSEC-2", "title": "gone"}, "package": {"name": "y", "version": "0.1.0"}}]}
        self.assertEqual(run_gate(report(warnings=warn)), 0)

    def test_must_not_report_green_when_nothing_was_scanned(self):
        self.assertEqual(run_gate(report(crates=0)), 3)

    def test_must_not_report_green_with_empty_database(self):
        self.assertEqual(run_gate(report(advisories=0)), 3)

    def test_must_not_report_green_on_garbage_input(self):
        self.assertEqual(run_gate("not json"), 3)


class Scorer(unittest.TestCase):
    def test_known_vectors(self):
        self.assertEqual(ag.cvss3_score(HIGH), 9.8)
        self.assertEqual(ag.cvss3_score(MEDIUM), 5.9)
        self.assertEqual(ag.cvss3_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H"), 10.0)
        self.assertEqual(ag.cvss3_score("CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:C/C:L/I:L/A:N"), 6.1)


if __name__ == "__main__":
    unittest.main()
