#!/usr/bin/env python3
"""Check that every intra-document `§X.Y` / `§X` reference resolves to a real
heading in the same file.

Cross-document refs (e.g. `docs/design.md §5.7` or `design.md §5.7`) are also
checked, against the *target* file's heading set.

This is the gap that let `docs/design.md` §597/§658 and `docs/roadmap.md` §24
point at a design.md §3.4 that never existed.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DOCS = ["AGENTS.md", "README.md", "README.zh-CN.md", "docs/design.md",
        "docs/design.zh-CN.md", "docs/deployment.md", "docs/deployment.zh-CN.md",
        "docs/reference.md", "docs/reference.zh-CN.md", "docs/retired.md",
        "docs/retired.zh-CN.md", "docs/opengaps.md", "docs/opengaps.zh-CN.md",
        "docs/charter.md", "docs/charter.zh-CN.md", "cloudflare-worker/README.md"]

HEADING = re.compile(r"^(#{2,4})\s+(\d+(?:\.\d+)?\.?)\s")
# §2.3, §5.7, §3 (bare), also `§2.3/§3.4` chains handled by finditer
SEC = re.compile(r"§(\d+(?:\.\d+)?)")
# an explicit target file sitting immediately before the §ref
TARGET_BEFORE = re.compile(
    r"(?:docs/)?(AGENTS\.md|README\.md|README\.zh-CN\.md|design(?:\.zh-CN)?\.md|"
    r"deployment(?:\.zh-CN)?\.md|reference(?:\.zh-CN)?\.md|retired(?:\.zh-CN)?\.md|opengaps(?:\.zh-CN)?\.md|charter(?:\.zh-CN)?\.md)\s*[`\s]*"
    r"§(\d+(?:\.\d+)?)")


def headings(path):
    """Return the set of section numbers present as headings."""
    out = set()
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            m = HEADING.match(line)
            if m:
                out.add(m.group(2).rstrip("."))
    return out


def main():
    names = {os.path.basename(p): p for p in DOCS}
    heads = {os.path.basename(p): headings(os.path.join(ROOT, p)) for p in DOCS}
    errors = []
    checked = 0

    for rel in DOCS:
        base = os.path.basename(rel)
        full = os.path.join(ROOT, rel)
        with open(full, encoding="utf-8") as fh:
            for lineno, line in enumerate(fh, 1):
                if line.lstrip().startswith("#"):
                    continue
                # Resolve against the current file by default; a table row
                # whose first cell names a document, or an earlier explicit
                # `foo.md §N`, overrides it for the rest of the line.
                target = base
                override_pos = -1
                if line.lstrip().startswith("|"):
                    first_cell = line.split("|")[1:2]
                    m_cell = re.search(r"(?:docs/)?([A-Za-z0-9_.-]+\.md)",
                                       first_cell[0]) if first_cell else None
                    if m_cell:
                        target = m_cell.group(1)
                        override_pos = line.find(m_cell.group(1))
                for m in TARGET_BEFORE.finditer(line):
                    target = m.group(1)
                    override_pos = m.end()
                    checked += 1
                    if m.group(2) not in heads.get(target, set()):
                        errors.append(
                            f"{rel}:{lineno}: §{m.group(2)} does not exist "
                            f"in {target}")
                for m in SEC.finditer(line):
                    # External references are not doc references. Two shapes:
                    # RFC-style three-part numbering (`§7.2.3`), caught below
                    # by the trailing-dot test, and an explicit RFC attribute
                    # on a two-part ref (`RFC 8620 §2.1`).
                    prefix = line[:m.start()]
                    if re.search(r"RFC\s+\d{3,5}\s*$", prefix):
                        continue
                    if line[m.end():m.end() + 1] == ".":
                        continue
                    if m.start() <= override_pos:
                        # already consumed as a cross-doc ref
                        continue
                    num = m.group(1)
                    # chained refs (`§2.3、§3.2、§5.7`) inherit `target`
                    if re.search(r"(?:\.md)[`\s]*$", prefix):
                        target = os.path.basename(
                            re.search(r"([A-Za-z0-9_.-]+\.md)[`\s]*$",
                                      prefix).group(1))
                    checked += 1
                    if num not in heads.get(target, set()):
                        errors.append(
                            f"{rel}:{lineno}: §{num} does not exist in {target}")

    print(f"=== SECTION-REF AUDIT ({checked} refs checked, "
          f"{len(errors)} errors) ===")
    for e in errors:
        print("  " + e)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
