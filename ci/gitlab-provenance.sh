#!/usr/bin/env bash
# SLSA provenance v1 predicate for the GitLab pipeline, from the CI variables of
# the running job. Printed to stdout, attested by the pipeline with cosign.
#
# What this is and is not: the statement is signed by the CI job identity, so a
# consumer can tell which project, ref and pipeline produced the image. It is
# produced by a job of the same pipeline, not by an isolated builder, which is why
# the GitLab option claims SLSA Build L2 at most (the GitHub option uses the
# isolated slsa-github-generator).
#
# Usage: ci/gitlab-provenance.sh <variant>      (musl or glibc)
set -euo pipefail

variant=${1:?usage: gitlab-provenance.sh <musl|glibc>}
: "${CI_SERVER_URL:?not running in GitLab CI}" "${CI_PROJECT_PATH:?}" "${CI_COMMIT_SHA:?}" \
  "${CI_PIPELINE_ID:?}" "${CI_JOB_ID:?}" "${CI_COMMIT_REF_NAME:?}"

jq -n \
  --arg variant "$variant" \
  --arg server "$CI_SERVER_URL" \
  --arg project "$CI_PROJECT_PATH" \
  --arg sha "$CI_COMMIT_SHA" \
  --arg ref "$CI_COMMIT_REF_NAME" \
  --arg pipeline "$CI_PIPELINE_ID" \
  --arg job "$CI_JOB_ID" \
  --arg runner "${CI_RUNNER_DESCRIPTION:-unknown}" \
  --arg started "${CI_JOB_STARTED_AT:-}" \
  '{
    buildDefinition: {
      buildType: ($server + "/" + $project + "/-/blob/" + $sha + "/.gitlab-ci.yml"),
      externalParameters: { source: ($server + "/" + $project), ref: $ref, variant: $variant },
      internalParameters: { pipeline: $pipeline, job: $job, runner: $runner },
      resolvedDependencies: [
        { uri: ("git+" + $server + "/" + $project + ".git"), digest: { gitCommit: $sha } }
      ]
    },
    runDetails: {
      builder: { id: ($server + "/" + $project + "//.gitlab-ci.yml") },
      metadata: { invocationId: ($server + "/" + $project + "/-/jobs/" + $job), startedOn: $started }
    }
  }'
