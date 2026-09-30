#!/usr/bin/env python3
"""Re-anchor docs line references after a code edit.

`git diff -U0 HEAD -- src/<file>` gives the exact add/del blocks, so old -> new
line numbers are cumulative and need no text matching. Every `foo.rs:N` /
`foo.rs:N-M` reference in docs/ is translated in place; unchanged references are
left alone.

Usage: reanchor.py [--check]

    (default) apply fixes to docs/*.md and print what changed
    --check   report only; exit 1 if any anchor would move
"""

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ANCHOR = re.compile(r"(?<![\w.:])([A-Za-z_][\w-]*\.rs):(\d+)(?:-(\d+))?")
HUNK = re.compile(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
DOCS = sorted((REPO / "docs").glob("*.md"))
CHECK_ONLY = "--check" in sys.argv


def hunk_map(path: str) -> list[tuple[int, int, int, int]]:
    raw = subprocess.run(
        ["git", "diff", "-U0", "HEAD", "--", f"src/{path}"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    return [
        (int(m.group(1)), int(m.group(2) or 1), int(m.group(3)), int(m.group(4) or 1))
        for m in HUNK.finditer(raw.stdout)
    ]


def old_to_new(hunks: list[tuple[int, int, int, int]], old_line: int) -> int:
    """Translate one old line number through the hunks above it."""
    for o_start, o_len, n_start, n_len in hunks:
        if old_line < o_start:
            break
        if old_line <= o_start + o_len - 1:
            # Inside a replaced block: map by position; a pure deletion has no
            # successor, so fall back to the hunk's new start.
            return n_start + max(n_len - 1, 0) if n_len == 0 else n_start + (old_line - o_start)
    delta = sum(
        n_len - o_len for o_start, o_len, _n_start, n_len in hunks if o_start <= old_line
    )
    return old_line + delta


def head_text(doc: Path) -> list[str]:
    """The pre-edit version of a doc, so --check compares like with like."""
    raw = subprocess.run(
        ["git", "show", f"HEAD:{doc.relative_to(REPO)}"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    return raw.stdout.splitlines() if raw.returncode == 0 else doc.read_text().splitlines()


def main() -> int:
    caches: dict[str, list[tuple[int, int, int, int]]] = {}
    changed_files = 0
    changed_refs = 0

    for doc in DOCS:
        current = doc.read_text().splitlines()
        baseline = head_text(doc) if CHECK_ONLY else current
        lines = list(current)
        dirty = False
        for lineno, line in enumerate(lines):
            if CHECK_ONLY:
                if lineno < len(baseline):
                    line = baseline[lineno]
                else:
                    continue
            out: list[str] = []
            pos = 0
            for m in ANCHOR.finditer(line):
                fname = m.group(1)
                if not sorted((REPO / "src").rglob(fname)):
                    continue
                hunks = caches.setdefault(fname, hunk_map(fname))
                if not hunks:
                    continue
                old_start = int(m.group(2))
                old_end = int(m.group(3)) if m.group(3) else old_start
                new_start, new_end = old_to_new(hunks, old_start), old_to_new(hunks, old_end)
                if (new_start, new_end) == (old_start, old_end):
                    continue
                replacement = f"{fname}:{new_start}" if new_start == new_end else f"{fname}:{new_start}-{new_end}"
                if CHECK_ONLY:
                    # A doc may have been restructured since HEAD, so compare by
                    # value across the whole file instead of by line index.
                    if m.group(0) in "\n".join(current):
                        changed_refs += 1
                        print(f"  STALE {doc.name} {m.group(0)} still present; want {replacement}")
                    continue
                out.append(line[pos : m.start()])
                out.append(replacement)
                pos = m.end()
                changed_refs += 1
                print(f"  {doc.name}:{lineno + 1} {m.group(0)} -> {replacement}")
                dirty = True
            if out:
                out.append(line[pos:])
                lines[lineno] = "".join(out)
                dirty = True
        if dirty:
            doc.write_text("\n".join(lines) + "\n")
            changed_files += 1

    print(f"refs={changed_refs} files={changed_files}" + (" (check only)" if CHECK_ONLY else ""))
    return 1 if changed_refs and CHECK_ONLY else 0


if __name__ == "__main__":
    sys.exit(main())
