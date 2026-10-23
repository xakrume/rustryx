#!/usr/bin/env python3
"""Fail on an SBOM that lists nothing.

DSO-08: an empty SBOM (zero components with a purl) is a failed result, not a
clean one. A generator that scanned the wrong directory or an image without the
dependency data exits 0 and writes a valid, empty document, and the pipeline
would call that green.

Exit codes: 0 ok, 3 empty or unreadable (the tool did not do its job).
Usage: check-sbom.py [--min N] [--expect-purl-prefix pkg:cargo/] [--summary FILE] sbom.json
"""

import argparse
import json
import sys


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("sbom")
    parser.add_argument("--min", type=int, default=1, help="minimum number of components with a purl")
    parser.add_argument("--expect-purl-prefix", default="", help="at least one purl must start with this")
    parser.add_argument("--summary", help="append a markdown line to this file")
    args = parser.parse_args()

    try:
        with open(args.sbom, encoding="utf-8") as fh:
            doc = json.load(fh)
        if doc.get("bomFormat") != "CycloneDX":
            raise ValueError(f"bomFormat is {doc.get('bomFormat')!r}, not CycloneDX")
    except (OSError, ValueError) as exc:
        print(f"check-sbom: unreadable SBOM {args.sbom}: {exc}", file=sys.stderr)
        return 3

    components = doc.get("components") or []
    purls = [c["purl"] for c in components if c.get("purl")]
    line = (f"{args.sbom}: CycloneDX {doc.get('specVersion', '?')}, {len(components)} components, "
            f"{len(purls)} with a purl")
    print(line)
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as fh:
            fh.write(f"- SBOM `{args.sbom}`: {len(components)} components, {len(purls)} with a purl\n")

    if len(purls) < args.min:
        print(f"check-sbom: {len(purls)} components with a purl, at least {args.min} required", file=sys.stderr)
        return 3
    if args.expect_purl_prefix and not any(p.startswith(args.expect_purl_prefix) for p in purls):
        print(f"check-sbom: no component matches {args.expect_purl_prefix}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
