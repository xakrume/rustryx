"""Tests for check-trivy.py, check-sbom.py and coverage-gate.py.

Run: python3 -I -m unittest discover -s ci/tests
Every gate here has a "must not" test: a scanner that scanned nothing, crashed, or
a coverage drop must turn the job red, because a green job proves nothing (DSO-00).
"""

import contextlib
import importlib.util
import io
import json
import pathlib
import sys
import tempfile
import unittest

CI = pathlib.Path(__file__).resolve().parent.parent


def load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, CI / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


trivy = load("check_trivy", "check-trivy.py")
sbom = load("check_sbom", "check-sbom.py")
cov = load("coverage_gate", "coverage-gate.py")


def run_main(module, argv) -> int:
    old = sys.argv
    sys.argv = ["x", *argv]
    try:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return module.main()
    finally:
        sys.argv = old


class Files(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = pathlib.Path(self.tmp.name)

    def write(self, name: str, data) -> str:
        path = self.dir / name
        path.write_text(json.dumps(data), encoding="utf-8")
        return str(path)


def trivy_report(packages=10, vulns=()):
    return {"Results": [{
        "Target": "image (debian 13)",
        "Packages": [{"Name": f"p{i}"} for i in range(packages)],
        "Vulnerabilities": [
            {"VulnerabilityID": cve, "PkgName": "libx", "InstalledVersion": "1", "FixedVersion": fixed, "Severity": sev}
            for cve, sev, fixed in vulns
        ],
    }]}


class Trivy(Files):
    def judge(self, report, *extra):
        return run_main(trivy, [self.write("t.json", report), *extra])

    def test_clean_report_passes(self):
        self.assertEqual(self.judge(trivy_report()), 0)

    def test_fixable_critical_blocks_in_observation_mode(self):
        self.assertEqual(self.judge(trivy_report(vulns=[("CVE-1", "CRITICAL", "2")])), 1)

    def test_high_is_a_warning_in_observation_mode(self):
        self.assertEqual(self.judge(trivy_report(vulns=[("CVE-1", "HIGH", "2")])), 2)

    def test_high_blocks_when_enforced(self):
        self.assertEqual(self.judge(trivy_report(vulns=[("CVE-1", "HIGH", "2")]), "--enforce", "true"), 1)

    def test_medium_stays_a_warning_when_enforced_at_high(self):
        self.assertEqual(self.judge(trivy_report(vulns=[("CVE-1", "MEDIUM", "2")]), "--enforce", "true"), 2)

    def test_unfixed_critical_does_not_block_by_default(self):
        self.assertEqual(self.judge(trivy_report(vulns=[("CVE-1", "CRITICAL", "")])), 0)

    def test_unfixed_critical_blocks_when_asked(self):
        self.assertEqual(self.judge(trivy_report(vulns=[("CVE-1", "CRITICAL", "")]), "--include-unfixed"), 1)

    def test_must_not_pass_a_scan_that_listed_no_packages(self):
        self.assertEqual(self.judge(trivy_report(packages=0)), 3)

    def test_must_not_pass_when_the_scanner_itself_failed(self):
        self.assertEqual(self.judge(trivy_report(), "--scanner-exit-code", "1"), 3)

    def test_must_not_pass_an_unreadable_report(self):
        self.assertEqual(run_main(trivy, [str(self.dir / "missing.json")]), 3)


class Sbom(Files):
    def check(self, doc, *extra):
        return run_main(sbom, [self.write("s.json", doc), *extra])

    def test_non_empty_sbom_passes(self):
        doc = {"bomFormat": "CycloneDX", "components": [{"purl": "pkg:cargo/tokio@1.0.0"}]}
        self.assertEqual(self.check(doc, "--expect-purl-prefix", "pkg:cargo/"), 0)

    def test_must_not_pass_an_sbom_without_components(self):
        self.assertEqual(self.check({"bomFormat": "CycloneDX", "components": []}), 3)

    def test_must_not_count_components_without_purl(self):
        self.assertEqual(self.check({"bomFormat": "CycloneDX", "components": [{"name": "x"}]}), 3)

    def test_must_not_pass_when_expected_ecosystem_is_missing(self):
        doc = {"bomFormat": "CycloneDX", "components": [{"purl": "pkg:deb/debian/base-files@1"}]}
        self.assertEqual(self.check(doc, "--expect-purl-prefix", "pkg:cargo/"), 3)

    def test_must_not_pass_a_document_of_another_format(self):
        self.assertEqual(self.check({"spdxVersion": "SPDX-2.3"}), 3)


def llvm_cov(files: dict) -> dict:
    return {"data": [{"files": [
        {"filename": f"/work/crates/{crate}/src/lib.rs", "summary": {"lines": {"count": count, "covered": covered}}}
        for crate, (count, covered) in files.items()
    ]}]}


GOOD = {"rustryx-auth": (100, 95), "rustryx-rbac": (100, 92), "rustryx-api": (100, 80), "rustryx-format-maven": (100, 91)}


class Coverage(Files):
    def gate(self, report, *extra):
        return run_main(cov, [self.write("c.json", llvm_cov(report)), *extra])

    def test_meeting_every_threshold_passes(self):
        self.assertEqual(self.gate(GOOD), 0)

    def test_must_not_pass_workspace_below_80(self):
        self.assertEqual(self.gate({**GOOD, "rustryx-api": (1000, 100)}), 1)

    def test_must_not_pass_auth_below_90(self):
        self.assertEqual(self.gate({**GOOD, "rustryx-auth": (100, 85), "rustryx-api": (100, 100)}), 1)

    def test_must_not_pass_a_parser_crate_below_90(self):
        self.assertEqual(self.gate({**GOOD, "rustryx-format-maven": (100, 89), "rustryx-api": (100, 100)}), 1)

    def test_format_core_only_needs_the_workspace_threshold(self):
        self.assertEqual(self.gate({**GOOD, "rustryx-format-core": (100, 82)}), 0)

    def test_must_not_pass_a_regression_against_main(self):
        baseline = self.write("b.json", {"workspace": 92.0, "crates": {"rustryx-auth": 99.0}})
        report = {**GOOD, "rustryx-auth": (100, 95), "rustryx-api": (100, 100)}
        self.assertEqual(self.gate(report, "--baseline", baseline), 1)

    def test_small_rounding_noise_is_not_a_regression(self):
        baseline = self.write("b.json", {"workspace": 91.5, "crates": {"rustryx-auth": 95.05}})
        self.assertEqual(self.gate({**GOOD, "rustryx-api": (100, 100)}, "--baseline", baseline), 0)

    def test_missing_baseline_is_skipped(self):
        self.assertEqual(self.gate(GOOD, "--baseline", str(self.dir / "none.json")), 0)

    def test_must_not_report_green_for_a_report_without_lines(self):
        self.assertEqual(self.gate({"rustryx-auth": (0, 0)}), 3)

    def test_must_not_report_green_for_an_unreadable_report(self):
        self.assertEqual(run_main(cov, [str(self.dir / "missing.json")]), 3)


if __name__ == "__main__":
    unittest.main()
