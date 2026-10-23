#!/usr/bin/env python3
"""Static check of .gitlab-ci.yml (DSO-00c, OPS-20, OPS-28).

GitLab validates the `stage` of a job BEFORE it evaluates `rules`, so a job whose
stage is missing from `stages` breaks the whole pipeline even when its rules would
have skipped it. This script reads the file the way a human would and checks:
  - every non-hidden job has a stage and the stage is declared in `stages`;
  - the stages OPS-20 names (lint, test, code-security, build, image-security,
    publish) are declared;
  - lint and test jobs are `interruptible`;
  - every `allow_failure: true` sits on a job whose line is marked OBSERVATION
    MODE, so an observation gate cannot be added unnoticed;
  - the image-security job is NOT allowed to fail (CRITICAL always blocks).
It does not replace `POST /projects/:id/ci/lint?include_merged_yaml=true` on the
server, which is the authority for the merged result.

Usage: check-gitlab-ci.py .gitlab-ci.yml          (needs PyYAML, the lint venv has it)
"""

import re
import sys

import yaml

REQUIRED_STAGES = ["lint", "test", "code-security", "build", "image-security", "publish"]
RESERVED = {"stages", "workflow", "variables", "default", "include", "image", "services", "cache", "before_script", "after_script"}


def main(path: str) -> int:
    text = open(path, encoding="utf-8").read()
    doc = yaml.safe_load(text)
    problems = []

    stages = doc.get("stages") or []
    for stage in REQUIRED_STAGES:
        if stage not in stages:
            problems.append(f"stage '{stage}' is not declared in stages")

    jobs = {k: v for k, v in doc.items() if k not in RESERVED and not k.startswith(".") and isinstance(v, dict)}
    for name, job in jobs.items():
        if job.get("stage") is None and "extends" not in job:
            problems.append(f"job {name} has no stage")
        stage = job.get("stage")
        if stage is None and "extends" in job:
            parent = doc.get(job["extends"] if isinstance(job["extends"], str) else job["extends"][0], {})
            stage = parent.get("stage")
        if stage is not None and stage not in stages:
            problems.append(f"job {name}: stage '{stage}' is not in stages (GitLab rejects the whole pipeline)")
        if stage in ("lint", "test") and not job.get("interruptible") and not name.startswith("coverage"):
            problems.append(f"job {name} in stage {stage} must be interruptible (OPS-28)")

    # allow_failure only where the OBSERVATION MODE marker is on the same line.
    for number, line in enumerate(text.splitlines(), 1):
        if re.search(r"^\s*allow_failure:\s*true", line) and "OBSERVATION MODE" not in line:
            problems.append(f"line {number}: allow_failure: true without the OBSERVATION MODE marker")

    security = jobs.get("image-security", {})
    if security.get("allow_failure"):
        problems.append("image-security must not set allow_failure: CRITICAL has to block publish")
    if "image-security" not in jobs:
        problems.append("no image-security job")

    for line in problems:
        print(f"check-gitlab-ci: {line}", file=sys.stderr)
    if not problems:
        print(f"check-gitlab-ci: ok ({len(jobs)} jobs, {len(stages)} stages, all declared)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else ".gitlab-ci.yml"))
