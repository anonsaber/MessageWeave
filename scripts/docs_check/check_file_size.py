#!/usr/bin/env python3
"""Enforce the AGENTS.md §2.1 file-size rule.

A file over SOFT_MAX lines must carry a `SPLIT-EVAL:` marker comment in its
first WINDOW lines, stating the split decision and why it was not split.
Over limit without the marker is a hard error.

The marker only needs to contain the literal token, so the rule is stated once
and stays valid regardless of the source language used.

LIMITATION: this proves the marker exists, not that the written reason is
sound. Someone must read it.
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

SOFT_MAX = 500   # AGENTS.md §2.1 soft cap
WINDOW = 60      # the marker must appear this many lines from the top
MARKER = 'SPLIT-EVAL:'

# Source only: Rust, the admin SPA, the worker. Docs and tests are excluded.
GLOBS = ['src/**/*.rs', 'web/*.js', 'cloudflare-worker/src/*.js']


def scan():
    files = []
    for pattern in GLOBS:
        files.extend(p for p in ROOT.glob(pattern) if p.is_file())
    return sorted(files)


def main():
    over = []
    errors = []
    for path in scan():
        try:
            lines = path.read_text(encoding='utf-8').splitlines()
        except UnicodeDecodeError:
            continue
        n = len(lines)
        if n <= SOFT_MAX:
            continue
        over.append(path)
        if MARKER not in '\n'.join(lines[:WINDOW]):
            errors.append(path)
            print(f'ERROR: {path.relative_to(ROOT)}: {n} lines > {SOFT_MAX}, '
                  f'no "{MARKER}" marker within the first {WINDOW} lines')

    for path in over:
        print(f'  {path.relative_to(ROOT)}: '
              f'{len(path.read_text(encoding="utf-8").splitlines())} lines')

    print(f'=== SIZE CHECK ({len(errors)} errors) ===')
    return 1 if errors else 0


if __name__ == '__main__':
    sys.exit(main())
