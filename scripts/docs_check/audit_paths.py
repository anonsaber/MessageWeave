#!/usr/bin/env python3
"""PATH AUDIT — no document may name a file or directory that does not exist.

Why this exists
---------------
The docs used to name directories, modules and tests that were never created.
A reader who acts on a documented path loses trust in the whole document, and
nobody can tell "we decided against this" from "this is wrong". So the rule is:
**every path-shaped token written in backticks in a document must resolve**,
including in the retired/deprecated registry (`docs/retired.md` is scanned too)
— there, a fictional path reads the same as a real one. The invented names are
deliberately not repeated here, so the examples cannot be copied back in;
`git log -p docs/retired.md` shows what they were.

What this check proves
----------------------
Every path-shaped token in inline code in the scanned documents is one of:
  (a) resolvable — it exists in this repo (see ROOT_DIRS, a token is resolved
      against several real roots because docs cite files as `config.rs`, not
      always `src/config.rs`);
  (b) attributed to an external repo by an owning prefix (EXEMPT_EXTERNAL),
      e.g. `stalwartlabs/jmap-client/src/client.rs`;
  (c) provably deleted — the file is gone but `git log --all --diff-filter=A`
      shows it was committed at some point (e.g. `docs/todo.md`);
  (d) on the commented CONVENTIONAL allowlist — a well-known filename that the
      prose explicitly says this repo does not have.

  Anything else is reported as an error.

What it does NOT prove
----------------------
- That the path means what the prose says, same limitation as audit_anchors.
- Anything outside inline code. Backticks are the contract: if you write a
  path in prose without backticks, this check cannot see it.
- Bare filenames unambiguously: `client.rs` resolves against any file with
  that basename in the repo.
- Anything in scripts. Code legitimately contains path-like string literals
  (this check is defined by string patterns, after all); those are not
  documentation claims.

Non-path shapes that look like paths are excluded by is_noise(), and a
`seg/` token only counts as a directory reference when it is the whole
inline-code span. Both rules are structural rather than denylisted: is_noise
recognises the shapes that cannot be repo-relative paths (absolute paths and
URIs, dot-prefixed URL parts, dotted hosts, media types, JMAP method names,
all-numeric pairs, shell and glob fragments), and the whole-span rule means
`Email/changes`, `application/json` and `method/path/origin/失败类别` are never
mistaken for a directory. Adding a special case to either should make you
suspect the document instead.
"""

import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))

DOCS = [
    os.path.join(ROOT, "docs", f) for f in sorted(os.listdir(os.path.join(ROOT, "docs")))
    if f.endswith(".md")
]
AGENTS = os.path.join(ROOT, "AGENTS.md")
TARGETS = [t for t in DOCS + [AGENTS] if os.path.isfile(t)]

# Directories a doc legitimately roots a path at. Every entry is real.
ROOT_DIRS = [
    ".",
    "src",
    "src/domain",
    "src/domain/jmap",
    "docs",
    "scripts",
    "scripts/docs_check",
    "cloudflare-worker",
    "cloudflare-worker/src",
    "web",
]

EXTS = "rs|js|mjs|ts|tsx|json|toml|ya?ml|md|sh|sql|cfg|lock|html|css|proto|txt"

# A file token (ends in a known extension) or a directory token (ends in '/').
TOKEN = re.compile(
    r"(?<![\w./:-])(?:"
    r"(?:[A-Za-z0-9_.{}\-]+/)*[A-Za-z0-9_.{}\-]+\.(?:" + EXTS + r")"
    r"|(?:[A-Za-z0-9_.{}\-]+/){1,}"
    r")"
)
INLINE_CODE = re.compile(r"`([^`\n]+)`")

# --- exemptions -------------------------------------------------------------

# (b) An owning-prefix that is not a directory in this repo. The prefix itself
# must be documented in prose on the same line as the citation.
EXEMPT_EXTERNAL = ("stalwartlabs/",)

# (d) Well-known filenames that are named only to say "this repo does not
# have it". Each entry carries its justification so the list cannot grow
# silently.
CONVENTIONAL = {
    "docker-compose.yml": "docker compose's default filename; deployment.md "
                          "states the repo ships no compose file and gives the "
                          "image-based command instead",
}

# --- non-path shapes that look like paths -----------------------------------

# Two-segment `Media/type` content types: application/json, text/html, ...
MIME_MEDIA = ("application", "text", "multipart", "message", "image", "audio",
              "video", "font")
# JMAP / REST method names, always CapitalisedName/lowercaseVerb, 2 segments.
JMAP_METHOD = re.compile(r"^[A-Z][A-Za-z]*\/[a-z][A-Za-z]*$")
# Dotted domains (api.telegram.org, crates.io) and numeric code pairs
# (200/503) are classified structurally inside is_noise.
#
# Shell or glob placeholders that are not real paths.
PLACEHOLDER = re.compile(r"[\s()*,?]")


def is_noise(tok):
    if tok.startswith("/") or "://" in tok:
        return "absolute path or URI"
    if tok.startswith("./"):
        return None  # './x' IS a real relative-path candidate
    if tok.startswith("."):
        return "dot-prefixed URL part"   # .well-known/jmap
    first, _, rest = tok.partition("/")
    if rest and "." in first:
        return "domain"                  # api.telegram.org, crates.io
    if rest and first in MIME_MEDIA:
        return "MIME type"               # application/json
    if JMAP_METHOD.match(tok):
        return "JMAP method"             # Email/changes
    if all(s.isdigit() for s in tok.rstrip("/").split("/")):
        return "code pair"               # 200/503
    if PLACEHOLDER.search(tok):
        return "shell or glob"
    return None


def variants(tok):
    """Expand {a,b,c} brace alternatives so globs cannot hide a failure."""
    m = re.search(r"\{([^{}]*)\}", tok)
    if not m:
        return [tok]
    head, tail = tok[:m.start()], tok[m.end():]
    return [head + a + tail for a in m.group(1).split(",")]


def resolve(tok):
    t = tok.lstrip("./")
    if not t:
        return None
    for d in ROOT_DIRS:
        if os.path.exists(os.path.join(ROOT, d, t)):
            return "resolves"
    return None


def was_deleted(tok):
    t = tok.lstrip("./")
    try:
        out = subprocess.run(
            ["git", "log", "--all", "--diff-filter=A", "--name-only",
             "--format=", "--", t],
            cwd=ROOT, capture_output=True, text=True, timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return "deleted but committed before" if out.stdout.strip() else None


def is_dir_token(tok):
    """A bare `seg/seg/` reference, as opposed to a `name.ext` file reference."""
    return tok.endswith("/") and "." not in tok.rsplit("/", 1)[-1]


def main():
    checked = 0
    errors = []
    for path in TARGETS:
        rel = os.path.relpath(path, ROOT)
        with open(path, encoding="utf-8") as fh:
            lines = fh.read().splitlines()
        fenced = False
        for i, line in enumerate(lines, 1):
            if re.match(r"\s*(```|~~~)", line):
                fenced = not fenced
                continue
            if fenced:
                continue
            for code in INLINE_CODE.findall(line):
                for tok in TOKEN.findall(code):
                    # A `seg/` token is only a directory reference when it IS the
                    # whole span. Otherwise it is a prefix of something else that
                    # just happens to contain a slash — a JMAP method
                    # (`Email/changes`), a content type (`application/json`), a
                    # template (`{jmap_origin}/.well-known/jmap`), or a log
                    # format (`method/path/origin/失败类别`). None of those are
                    # filesystem paths, and trying to classify them by pattern
                    # produces either false alarms or a growing denylist.
                    if is_dir_token(tok) and code.strip() != tok:
                        continue
                    if is_noise(tok):
                        continue
                    checked += 1
                    for v in variants(tok):
                        verdict = (
                            resolve(v)
                            or ("external repo" if v.startswith(EXEMPT_EXTERNAL) else None)
                            or was_deleted(v)
                            or ("conventional name" if v in CONVENTIONAL else None)
                        )
                        if not verdict:
                            errors.append(f"{rel}:{i} `{v}` does not resolve "
                                          f"(ROOT_DIRS, git history, or CONVENTIONAL)")
                            break
    print(f"=== PATH AUDIT ({len(errors)} errors, "
          f"{checked} path-refs checked) ===")
    for e in errors:
        print("  " + e)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
