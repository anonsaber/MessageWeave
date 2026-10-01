#!/usr/bin/env python3
"""Audit the docs' source anchors against the working tree.

For every "`src/foo.rs:123`" style anchor in the docs, check that the file
exists, the line is in range, and the line is not blank.

LIMITATION: this only checks that the anchor TARGETS A REAL, NON-BLANK LINE.
It does not check that the line says what the doc claims.
A doc could say "foo.rs:123 defines get_by_key" when line 123 is actually
`fn get_by_name`. This audit would pass.
Someone still has to read the anchors to verify they're accurate.

The one exception is a *phantom identifier*: if a backticked name sits right
next to an anchor, we check the name actually appears in that file. Catches
docs claiming an identifier that doesn't exist.
This is a loose substring check, so it has the same limitation as the anchor
check above. It catches the obvious case but not a near-miss name.

Two shapes of claim live next to an anchor, and they are checked with
deliberately different strictness:

  - Numbered anchor (`src/state.rs:283`). Kept as a loose substring check on
    the member name. PAIR's EXT only matches a name followed by a colon and a
    line number, so the anchor already pins down the location and the reader
    can verify the line. Widening the verdict here would change the count of
    findings in this branch, i.e. perturb the established baseline, for no
    gain: any doc written next to an anchor names the place and the name.
  - Bare file name (`src/channel.rs`, no line number). Checked with the strict
    `member_present` test and reported as a `phantom member`. No line number
    means the reader has nothing to verify, so the only thing this check can
    prove is that the named member actually exists in the file. And a loose
    test provably fails here: a doc can write `TelegramClient::send` and
    satisfy a substring -- even a word-boundary -- test twice over, because
    `channel.rs` contains `send_text` and reqwest's `.send()` is `send` at a
    word boundary. Hence definition/unqualified-reference semantics.
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

# `Ident` or `Module::Ident` inside backticks, optional () after.
IDENT = r'[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)?'
# A namespaced path: at least two segments joined by `::`. Required for the
# bare-file-name branch -- PAIR's IDENT would also match a bare `Foo` and
# produce a phantom-member hit on every ordinary mention.
IDENT_NSP = (r'[A-Za-z_][A-Za-z0-9_]*' + r'::'
             + r'(?:[A-Za-z_][A-Za-z0-9_]*::)*[A-Za-z_][A-Za-z0-9_]*')

# Optional directory prefix, then the bare file stem.
FILE = r'((?:[A-Za-z0-9_.-]+/)?[A-Za-z0-9_-]+)'

# Line anchor: `.rs:123`. The optional range suffix lives in REF, not here.
EXT = r'\.(rs|js):(\d+)'

# Bare file name: `.rs` not immediately followed by a colon. The negative
# lookahead keeps PAIR_BARE from re-matching anchors PAIR already consumed
# (`channel.rs:110`), so a numbered anchor is never reported twice.
EXT_BARE = r'\.(rs|js)(?!:)'

REF = re.compile(
    r'((?:src|cloudflare-worker/src|web)(?:/[A-Za-z0-9_.-]+)*/)?'
    + FILE + EXT + r'(?:\s*[,–\-]\s*(\d+))?'
)

# "Backticked identifier, some slack, right next to a file name" -- the shape
# shared by PAIR and PAIR_BARE. They differ only in the file-name half (EXT vs
# EXT_BARE) and in how strict the identifier is allowed to be.
GAP = (r'`(' + IDENT + r')(?:\(\))?`'
       r'[^`\n|]{0,40}?`?[^`\n|]{0,40}?'
       r'(?<![A-Za-z0-9_.])`?')
GAP_NSP = GAP.replace(IDENT, IDENT_NSP)

# Ident backticked next to an anchored file name: `Foo::bar` (`src/foo.rs:12`).
PAIR = re.compile(GAP + FILE + EXT)
# Ident backticked next to a bare file name: `Foo::bar` (`src/foo.rs`).
PAIR_BARE = re.compile(GAP_NSP + FILE + EXT_BARE)

# A Rust/JS definition site, or a keyword that introduces a binding. Used by
# member_present to distinguish "defined here" from "mentioned in passing".
DEFN_KW = (r'\b(?:pub(?:\(crate\))?\s+)*(?:async\s+)?'
           r'(?:fn|function|const|static|let|var|class|struct|enum|trait'
           r'|type|union|interface|use)\s+')


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


def member_present(symbol, text):
    """`symbol` 在 text 里是否真实存在（被定义，或被非限定地引用）。

    刻意比 `symbol in text` 严：文档声称 `T::send` 时，不能由 `send_text`
    （子串）或 reqwest 的 `.send()`（词边界）满足——这两者都在 src/channel.rs
    里真实存在。要求命中定义关键字，或命中一个前面不跟 `.` / 单词字符的引用。
    只排除 `.` 而不排除 `:`，是为了让 `worker::claim_dedup`、`std::env::var`
    这类限定调用仍然算数（排除 `:` 会产生 17 个假阳性）。
    """
    return bool(re.search(DEFN_KW + symbol + r'\b', text)) or \
        bool(re.search(r'(?<![.\w])' + symbol + r'\b', text))


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

        # Same pair shape, but the file name has no line number. `checked`
        # counts line-numbered references only, so this branch deliberately
        # never adds to it -- the reported total must stay the number of
        # anchors, not the number of identifier claims.
        for m in PAIR_BARE.finditer(line):
            ident, raw, ext = m.group(1), m.group(2), m.group(3)
            raw = raw.rsplit('/', 1)[-1]
            if '.' in raw:
                continue
            p = resolve(None, raw, ext)
            if p is None:
                continue
            symbol = ident.rsplit('::', 1)[-1]
            if not member_present(symbol, p.read_text()):
                errors.append(
                    f'{d}:{i} phantom member `{ident}` not defined in {raw}.{ext}'
                )

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
