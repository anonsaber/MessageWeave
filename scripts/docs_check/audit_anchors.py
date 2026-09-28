#!/usr/bin/env python3
"""Check every `foo.rs:N` / `foo.rs:N-M` / `web/config.js:N` line anchor in the docs.

Paths may be bare (`notify.rs:320`, resolved under `src/`), prefixed with a directory
(`src/domain/jmap/client.rs:108`, `cloudflare-worker/src/backends.js:9`,
`web/config.js:585`), or unprefixed JavaScript (`config.js:585`, resolved under `web/`
or the Worker).

LIMITATION: this only proves the cited line *exists and is not blank*. It does
not prove the line contains what the prose claims about it. A doc can pass this
audit with anchors that point at the wrong line -- a human must read the target
line to catch that. Keep this in mind when quoting "0 errors" as proof of
accuracy: it proves reachability, not correctness.

The one exception is a *phantom identifier*: if a backticked name sits right next
to an anchor (the common "`foo` ... `bar.rs:N`" phrasing) and that name appears
nowhere in bar.rs, the anchor is reported. Existence, not position -- the symbol
may still be on the wrong line.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DOCS = ['AGENTS.md', 'README.md', 'README.zh-CN.md', 'docs/design.md',
        'docs/deployment.md', 'docs/reference.md', 'docs/retired.md',
        'docs/roadmap.md', 'docs/charter.md', 'cloudflare-worker/README.md']

IDENT = r'[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)?'
FILE = r'((?:[A-Za-z0-9_.-]+/)?[A-Za-z0-9_-]+)'
EXT = r'\.(rs|js):(\d+)'

# A directory part is zero or more full segments (`/seg`), so the regex can never
# split a file name like `debug.rs` into a "directory" plus a stray base.
REF = re.compile(
    r'((?:src|cloudflare-worker/src|web)(?:/[A-Za-z0-9_.-]+)*/)?'
    + FILE + EXT + r'(?:\s*[,–\-]\s*(\d+))?'
)

# A backticked identifier sitting right next to an anchor is an implicit claim:
# "`foo` ... `bar.rs:N`". If `foo` never appears in bar.rs the anchor is a phantom.
# The gap may not cross more than one backtick boundary and is capped at 40
# characters on each side, so only a genuinely coupled identifier/anchor pair
# matches. An optional `()` after the identifier is tolerated, since prose often
# writes function names that way.
PAIR = re.compile(
    r'`(' + IDENT + r')(?:\(\))?`'
    r'[^`\n|]{0,40}?`?[^`\n|]{0,40}?'
    r'(?<![A-Za-z0-9_.])`?'
    + FILE + EXT
)


def resolve(prefix, base, ext):
    """Map an anchor's path fragment to a file on disk, or None."""
    if prefix is None:
        # Basename only. `.rs` lives in src/; `.js` may live in the Worker or in
        # the served SPA, so try both.
        if ext == 'rs':
            candidates = [ROOT / 'src' / f'{base}.{ext}']
        else:
            candidates = [ROOT / 'cloudflare-worker/src' / f'{base}.{ext}',
                          ROOT / 'web' / f'{base}.{ext}']
    else:
        candidates = [ROOT / prefix.rstrip('/') / f'{base}.{ext}']
    return next((c for c in candidates if c.is_file()), None)


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
        for m in PAIR.finditer(line):
            ident, raw, ext = m.group(1), m.group(2), m.group(3)
            raw = raw.rsplit('/', 1)[-1]
            if '.' in raw:
                continue
            p = resolve(None, raw, ext)
            if p is None:
                continue
            symbol = ident.rsplit('::', 1)[-1]
            if symbol not in p.read_text():
                errors.append(f'{d}:{i} phantom identifier `{ident}` not in {raw}.{ext}')
        for m in REF.finditer(line):
            prefix, base, ext, a, b = m.groups()
            p = resolve(prefix, base, ext)
            if p is None:
                errors.append(f'{d}:{i} unknown file {base}.{ext}')
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
