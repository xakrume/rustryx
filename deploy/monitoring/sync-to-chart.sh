#!/usr/bin/env bash
# Copies the monitoring sources into the chart. Helm can only read files inside the chart
# directory, and a symlink would break in a GitHub zip download, so the chart carries copies and
# deploy/helm/test.sh fails when they differ from these sources.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dest="$here/../helm/rustryx/files"
mkdir -p "$dest"
cp "$here/rustryx-alerts.yaml" "$dest/rustryx-alerts.yaml"
cp "$here/rustryx-dashboard.json" "$dest/rustryx-dashboard.json"
echo "synced monitoring assets into $dest"
