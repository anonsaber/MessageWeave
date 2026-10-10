import re, sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
DOCS = ['AGENTS.md', 'README.md', 'README.zh-CN.md', 'docs/design.md',
        'docs/design.zh-CN.md',
        'docs/reference.md', 'docs/reference.zh-CN.md', 'docs/retired.md',
        'docs/retired.zh-CN.md', 'docs/opengaps.md', 'docs/opengaps.zh-CN.md',
        'docs/charter.md', 'docs/charter.zh-CN.md',
        'cloudflare-worker/README.md',
        'cloudflare-worker/README.zh-CN.md']
errors = []

def slug(t):
    t = re.sub(r'[^\w\s\-]', '', t.strip().lower(), flags=re.UNICODE)
    return t.replace(' ', '-')

def heading_set(text):
    return set(slug(m.group(2)) for m in re.finditer(r'^(#{1,6})\s+(.*)$', text, re.M))

HEADS = {d: heading_set((ROOT / d).read_text()) for d in DOCS}

def markdown_links(text):
    for m in re.finditer(r'(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)', text):
        yield m.group(1)

for d in DOCS:
    lines = (ROOT / d).read_text().splitlines()
    in_fence = False
    for i, line in enumerate(lines, 1):
        if re.match(r'^\s*(```|~~~)', line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if line.count('`') % 2:
            errors.append(f'{d}:{i} odd backtick count')
        for url in markdown_links(line):
            if url.startswith(('http://', 'https://', 'mailto:')):
                continue
            if url.startswith('#'):
                frag = url[1:]
                if frag and frag not in HEADS[d]:
                    errors.append(f'{d}:{i} in-page anchor "{frag}" not found')
                continue
            if '#' in url:
                tgt_rel, frag = url.split('#', 1)
            else:
                tgt_rel, frag = url, ''
            tgt_rel = tgt_rel or d
            base = (ROOT / d).parent
            try:
                resolved = Path(base, tgt_rel).resolve()
                tgt_rel = str(resolved.relative_to(ROOT))
            except ValueError:
                errors.append(f'{d}:{i} escapes repo: {url}')
                continue
            if not resolved.exists():
                errors.append(f'{d}:{i} broken link: {url}')
                continue
            if frag:
                hs = HEADS.get(tgt_rel)
                if hs is None:
                    if resolved.suffix == '.md':
                        hs = heading_set(resolved.read_text())
                    else:
                        continue
                if frag not in hs:
                    errors.append(f'{d}:{i} anchor "{frag}" not in {tgt_rel}')
    if in_fence:
        errors.append(f'{d}: unclosed fenced code block')

# cross-document section references (allowed: same-doc "本文件 §", AGENTS.md index table)
for d in DOCS:
    lines = (ROOT / d).read_text().splitlines()
    in_fence = False
    for i, line in enumerate(lines, 1):
        if re.match(r'^\s*(```|~~~)', line):
            in_fence = not in_fence
            continue
        if in_fence or '`' in line or line.lstrip().startswith(('#', '|')):
            continue
        if re.search(r'docs/[a-z]+\.md|README(?:\.zh-CN)?\.md', line) and '§' in line:
            # AGENTS.md and charter.md hold the stable-ID index, which legitimately
            # cites other docs' sections (registry definition-file column).
            if d in ('AGENTS.md', 'docs/charter.md') or '本文件' in line:
                continue
            errors.append(f'{d}:{i} cross-doc section ref: {line.strip()}')

print(f'\n=== VALIDATION ({len(errors)} errors) ===')
for e in errors:
    print(e)
sys.exit(1 if errors else 0)
