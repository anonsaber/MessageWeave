import re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DOCS = ['AGENTS.md', 'README.md', 'README.zh-CN.md', 'docs/design.md',
        'docs/design.zh-CN.md', 'docs/deployment.md', 'docs/deployment.zh-CN.md',
        'docs/reference.md', 'docs/reference.zh-CN.md', 'docs/retired.md',
        'docs/retired.zh-CN.md', 'docs/opengaps.md', 'docs/opengaps.zh-CN.md',
        'docs/charter.md', 'docs/charter.zh-CN.md',
        'docs/quickstart.md', 'docs/quickstart.zh-CN.md',
        'cloudflare-worker/README.md',
        'cloudflare-worker/README.zh-CN.md']

CODE = re.compile(r'`[^`\n]*`')
SEP = re.compile(r'^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)+\|?\s*$')

def cells(line):
    """Split a table row into cells, ignoring pipes inside inline code spans."""
    blanks = [' ' * len(s) for s in CODE.findall(line)]
    stripped = CODE.sub(lambda m: blanks.pop(0), line)
    s = stripped.strip()
    if s.startswith('|'):
        s = s[1:]
    if s.endswith('|'):
        s = s[:-1]
    return s.split('|')

errors = []
for d in DOCS:
    lines = (ROOT / d).read_text().splitlines()
    in_fence = False
    i = 0
    while i < len(lines):
        if re.match(r'^\s*(```|~~~)', lines[i]):
            in_fence = not in_fence
            i += 1
            continue
        if in_fence:
            i += 1
            continue
        # detect table start: a row containing a pipe, followed by a separator row
        if '|' in lines[i] and i + 1 < len(lines) and SEP.match(lines[i + 1]):
            n_head = len(cells(lines[i]))
            n_sep = len(cells(lines[i + 1]))
            if n_head != n_sep:
                errors.append(f'{d}:{i+1} header/separator column mismatch: {n_head} vs {n_sep}')
            j = i + 2
            while j < len(lines) and '|' in lines[j] and not re.match(r'^\s*$', lines[j]):
                n = len(cells(lines[j]))
                if n != n_head:
                    errors.append(f'{d}:{j+1} row has {n} cells, header has {n_head}: {lines[j].strip()[:90]}')
                j += 1
            i = j
            continue
        i += 1

print(f'=== TABLE STRUCTURE ({len(errors)} mismatches) ===')
for e in errors:
    print(e)
sys.exit(1 if errors else 0)
