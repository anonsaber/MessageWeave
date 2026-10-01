#!/usr/bin/env python3
"""Audit whether a docs anchor actually sits next to the identifier it claims.

`audit_anchors.py` proves only that an anchor targets a real, non-blank line.
It cannot tell you that the doc says "`refresh_business_config` at
`notify.rs:381`" when line 381 is `async fn worker`. This script asks that
weaker follow-up question: does the identifier the doc names next to an anchor
appear anywhere near the anchored line?

If it does not, the anchor is a *candidate* for being wrong -- not a verdict.
Several shapes are legal on purpose, so this script never fails the gate:
it prints an INFO-level candidate list, always exits 0, and is deliberately
not wired into `run_all.sh`.

Intended use: after a batch of anchor re-pointings, re-run this and compare the
count. A shrinking list means the anchors are landing where the docs say; a
flat list means something is still off. It is the only automated substitute for
reading every anchor by hand, which is why it is worth existing.

How an identifier is matched to an anchor: docs pair them in prose order
("key built at `foo.rs:424`, `claim_dedup` at `foo.rs:458`"), so an identifier
claims the *next* anchor after it, never the nearest one. On a table row
carrying several anchors the preceding anchor sits just as close on the wrong
side, and pairing by distance reports every such row as drift.

Known-legal anchors -- do not "fix" these:
  - the doc anchors a call site while the definition lives elsewhere
  - the identifier is on an adjacent line the anchor does not name
  - the "identifier" is a value, e.g. an env var name such as `DEBUG_TOKEN`
  - the identifier is a data literal the doc deliberately points at (a Lua
    script body, a JSON string)
The shape filter drops the third shape (no underscore); the LEGAL table drops
the rest. LEGAL is keyed on the anchor target's span, not the doc line, because
re-pointing the docs moves the doc line while the target does not.

Constants ROOT / DOCS / FILE / EXT / REF / resolve are copied verbatim from
`audit_anchors.py` so this script stays standalone. `audit_anchors.py` cannot
be imported: its module-level code runs the audit and calls `sys.exit`. Keep
the copies in sync if the anchor grammar changes.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

DOCS = ['AGENTS.md', 'README.md', 'README.zh-CN.md', 'docs/design.md',
        'docs/design.zh-CN.md', 'docs/deployment.md', 'docs/deployment.zh-CN.md',
        'docs/reference.md', 'docs/reference.zh-CN.md', 'docs/retired.md',
        'docs/retired.zh-CN.md', 'docs/opengaps.md', 'docs/opengaps.zh-CN.md',
        'docs/charter.md', 'docs/charter.zh-CN.md', 'cloudflare-worker/README.md',
        'cloudflare-worker/README.zh-CN.md']

FILE = r'((?:[A-Za-z0-9_.-]+/)?[A-Za-z0-9_-]+)'
EXT = r'\.(rs|js):(\d+)'
REF = re.compile(
    r'((?:src|cloudflare-worker/src|web)(?:/[A-Za-z0-9_.-]+)*/)?'
    + FILE + EXT + r'(?:\s*[,–\-]\s*(\d+))?'
)

# Lowercase snake_case only: `refresh_business_config`, `retry_or_dlq`.
# Requiring at least one underscore is the main noise filter -- it drops bare
# words like `worker` or `batch` that a doc line mentions incidentally, and it
# drops UPPER_SNAKE env names like `DEBUG_TOKEN`, which are values, not Rust
# identifiers. Length >= 4 is enforced separately.
IDENTIFIER = re.compile(r'`([a-z][a-z0-9]*(?:_[a-z0-9]+)+)`')

# Generic accessors too common to be meaningful evidence either way. The first
# four are already excluded by the shape filter; they are listed explicitly so
# the intent stays visible if the filter is ever loosened.
GENERIC = frozenset(['none', 'ok', 'err', 'some',
                     'is_none', 'is_some', 'is_ok', 'is_err', 'to_string'])

WINDOW = 2          # lines above and below the anchor target
MIN_LEN = 4         # minimum identifier length
NEIGHBOURHOOD = 60  # max chars from an identifier to the anchor it claims

# Anchors that are correct as written but sit outside the +/- WINDOW purely by
# design. Keyed on (identifier, source file) -> the span the target may fall in.
# Not keyed on the doc line: re-pointing the docs moves the doc line while the
# target these anchors claim does not. `None` -> main.rs:135 and
# `DEBUG_TOKEN` -> main.rs:106 belong to the same class but are already dropped
# by the shape filter (no underscore), so they need no entry here.
LEGAL = {
    ('refresh_business_config', 'notify.rs'): (381, 381),
    ('invalid_request', 'notify.rs'): (402, 402),
    ('reconcile_token', 'notify.rs'): (284, 284),
    ('worker_token', 'notify.rs'): (393, 393),
    # retry_or_dlq spans state.rs:472-503, its Lua script body 484-491. A doc
    # may anchor anywhere inside and never mention the identifier.
    ('retry_or_dlq', 'state.rs'): (472, 503),
}


def resolve(prefix, base, ext):
    """Map an anchor's path fragment to a file on disk, or None."""
    if prefix is None:
        if ext == 'rs':
            candidates = [ROOT / 'src' / f'{base}.{ext}']
        else:
            candidates = [ROOT / 'cloudflare-worker/src' / f'{base}.{ext}',
                          ROOT / 'web' / f'{base}.{ext}']
    else:
        candidates = [ROOT / prefix.rstrip('/') / f'{base}.{ext}']
    return next((c for c in candidates if c.is_file()), None)


def window_text(src, line):
    """The anchor's target line plus WINDOW lines each side."""
    lo = max(0, line - 1 - WINDOW)
    return '\n'.join(src[lo:line + WINDOW])


def gap(left, right):
    """Chars of text between two non-overlapping spans."""
    return max(left.start() - right.end(), right.start() - left.end(), 0)


def identifiers_near(line):
    """Backticked snake_case identifiers on the doc line worth checking."""
    return [m for m in IDENTIFIER.finditer(line)
            if len(m.group(1)) >= MIN_LEN and m.group(1) not in GENERIC]


def partner_for(im, anchors):
    """The anchor an identifier claims.

    Prose pairs them in document order -- "key built at `foo.rs:424`,
    `claim_dedup` at `foo.rs:458`" -- so an identifier belongs to the next
    anchor after it. Nearest-anchor pairing mis-reads that line: the
    preceding anchor sits two characters closer and swallows the claim.
    Falls back to the previous anchor when the identifier trails the line.
    """
    following = [a for a in anchors if a.start() >= im.end()]
    if following:
        return following[0]
    preceding = [a for a in anchors if a.end() < im.start()]
    return preceding[-1] if preceding else None


candidates = []
seen = set()
for d in DOCS:
    lines = (ROOT / d).read_text().splitlines()
    in_fence = False
    for i, line in enumerate(lines, 1):
        if re.match(r'^\s*(```|~~~)', line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        anchors = list(REF.finditer(line))
        if not anchors:
            continue
        for im in identifiers_near(line):
            best = partner_for(im, anchors)
            if best is None or gap(im, best) > NEIGHBOURHOOD:
                continue
            prefix, base, ext, a, b = best.groups()
            p = resolve(prefix, base, ext)
            if p is None:
                continue
            src = p.read_text().splitlines()
            lo, hi = int(a), int(b) if b else int(a)
            if hi > len(src):
                continue
            tok = im.group(1)
            key = (d, i, tok)
            if key in seen:
                continue
            seen.add(key)
            span = LEGAL.get((tok, p.name))
            if span and span[0] <= lo <= span[1]:
                continue  # correct as written; the gap is intentional
            if any(tok in window_text(src, n) for n in {lo, hi}):
                continue  # identifier is on the anchored line (+/- 2)
            candidates.append(
                f'{d}:{i} `{tok}` -> {p.relative_to(ROOT)}:{lo}'
                + (f'-{hi}' if b else '')
                + f' | {src[lo - 1].strip()}'
            )

print(f'=== ANCHOR SEMANTICS ({len(candidates)} candidates) ===')
for c in candidates:
    print(c)
sys.exit(0)
