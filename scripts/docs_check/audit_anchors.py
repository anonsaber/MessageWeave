#!/usr/bin/env python3
"""Check every `foo.rs:N` / `foo.rs:N-M` line anchor in the docs.

LIMITATION: this only proves the cited line *exists and is not blank*. It does
not prove the line contains what the prose claims about it. A doc can pass this
audit with anchors that point at the wrong line -- a human must read the target
line to catch that. Keep this in mind when quoting "0 errors" as proof of
accuracy: it proves reachability, not correctness.
"""
import re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DOCS = ['AGENTS.md', 'README.md', 'README.zh-CN.md', 'docs/design.md',
        'docs/deployment.md', 'docs/reference.md', 'docs/retired.md', 'docs/roadmap.md']

REF = re.compile(r'(?:(src|cloudflare-worker/src)/)?([A-Za-z0-9_]+)\.(?:rs|js):(\d+)(?:\s*[,–\-]\s*(\d+))?')

errors = []
checked = 0
for d in DOCS:
    lines = (ROOT / d).read_text().splitlines()
    in_fence = False
    for i, line in enumerate(lines, 1):
        if re.match(r'^\s*(```|~~~)', line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for m in REF.finditer(line):
            sub, base, a, b = m.group(1), m.group(2), m.group(3), m.group(4)
            if sub == 'cloudflare-worker/src':
                p = ROOT / sub / f'{base}.js'
            elif sub == 'src' or sub is None:
                p = ROOT / 'src' / f'{base}.rs'
                if not p.exists() and sub is None:
                    q = ROOT / 'cloudflare-worker/src' / f'{base}.js'
                    if q.exists():
                        p = q
            else:
                p = ROOT / sub / f'{base}.rs'
            if not p.exists():
                errors.append(f'{d}:{i} unknown file {p.name}')
                continue
            src = p.read_text().splitlines()
            hi = int(b) if b else int(a)
            if hi > len(src):
                errors.append(f'{d}:{i} {p.name}:{a}-{b} out of range (file has {len(src)} lines)')
                continue
            lo = int(a)
            targets = {lo} | ({hi} if b else set())
            for n in sorted(targets):
                checked += 1
                if not src[n - 1].strip():
                    errors.append(f'{d}:{i} {p.name}:{a}{"-" + b if b else ""} -> line {n} is blank')

print(f'=== ANCHOR AUDIT ({checked} line-refs checked, {len(errors)} errors) ===')
for e in errors:
    print(e)
sys.exit(1 if errors else 0)
