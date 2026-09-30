#!/usr/bin/env python3
"""Print every `foo.rs:N[-M]` reference in docs/ together with the current
line text it points at, so a human can eyeball whether the anchor is still
semantically right (audit_anchors.py only proves the line is non-blank)."""

import re
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ANCHOR = re.compile(r"(?<![\w.:])([A-Za-z_][\w-]*\.rs):(\d+)(?:-(\d+))?")
DOCS = list((REPO / "docs").glob("*.md"))


def resolve(fname: str) -> Path | None:
    hits = sorted((REPO / "src").rglob(fname))
    return hits[0] if hits else None


for doc in sorted(DOCS):
    for lineno, line in enumerate(doc.read_text().splitlines(), 1):
        for m in ANCHOR.finditer(line):
            fname = m.group(1)
            start = int(m.group(2))
            end = int(m.group(3)) if m.group(3) else start
            path = resolve(fname)
            if path is None:
                print(f"{doc.name}:{lineno}\t{m.group(0)}\t<NONE>")
                continue
            src = path.read_text().splitlines()
            if start > len(src):
                print(f"{doc.name}:{lineno}\t{m.group(0)}\t<OUT OF RANGE, {len(src)} lines>")
                continue
            body = " ⏎ ".join(l.strip()[:60] for l in src[start - 1 : end])
            print(f"{doc.name}:{lineno}\t{m.group(0)}\t{body}")
