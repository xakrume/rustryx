#!/usr/bin/env python3
"""Print the hash cargo-deny expects in `licenses.clarify.license-files`.

cargo-deny does not use CRC32. It normalises every line ending to a plain
newline (and guarantees a final newline) and hashes the bytes with XXH32, seed
0. Written out here because the value cannot be read from cargo-deny output
when a clarification does not match (it falls back to the manifest silently).

Usage: ci/licence-hash.py <file> [<file>...]
Run it on the LICENSE file inside ~/.cargo/registry/src/*/<crate>-<version>/.
"""

import sys

MASK = 0xFFFFFFFF
P1, P2, P3, P4, P5 = 2654435761, 2246822519, 3266489917, 668265263, 374761393


def _rotl(x: int, r: int) -> int:
    return ((x << r) | (x >> (32 - r))) & MASK


def xxh32(data: bytes, seed: int = 0) -> int:
    n = len(data)
    i = 0
    if n >= 16:
        acc = [
            (seed + P1 + P2) & MASK,
            (seed + P2) & MASK,
            seed & MASK,
            (seed - P1) & MASK,
        ]
        while i <= n - 16:
            for k in range(4):
                lane = int.from_bytes(data[i : i + 4], "little")
                i += 4
                acc[k] = (_rotl((acc[k] + lane * P2) & MASK, 13) * P1) & MASK
        h = (_rotl(acc[0], 1) + _rotl(acc[1], 7) + _rotl(acc[2], 12) + _rotl(acc[3], 18)) & MASK
    else:
        h = (seed + P5) & MASK
    h = (h + n) & MASK
    while i + 4 <= n:
        lane = int.from_bytes(data[i : i + 4], "little")
        h = (_rotl((h + lane * P3) & MASK, 17) * P4) & MASK
        i += 4
    while i < n:
        h = (_rotl((h + data[i] * P5) & MASK, 11) * P1) & MASK
        i += 1
    h ^= h >> 15
    h = (h * P2) & MASK
    h ^= h >> 13
    h = (h * P3) & MASK
    h ^= h >> 16
    return h


def normalised(path: str) -> bytes:
    """Line endings to LF, final newline guaranteed, as cargo-deny reads files."""
    lines = open(path, "rb").read().split(b"\n")
    if lines and lines[-1] == b"":
        lines.pop()
    return b"".join(line.rstrip(b"\r\n") + b"\n" for line in lines)


def self_test() -> None:
    # Published XXH32 test vectors; the 16+ byte path is covered by the last one.
    vectors = {
        b"": 0x02CC5D05,
        b"a": 0x550D7456,
        b"abc": 0x32D153FF,
        b"Nobody inspects the spammish repetition": 0xE2293B2F,
    }
    for data, expected in vectors.items():
        if xxh32(data) != expected:
            sys.exit(f"xxh32 self-test failed for {data!r}")


def main(argv: list) -> int:
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    self_test()
    for path in argv[1:]:
        print(f"{path} hash = 0x{xxh32(normalised(path)):08x}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
