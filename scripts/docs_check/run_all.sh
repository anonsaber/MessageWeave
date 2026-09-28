#!/usr/bin/env bash
# GATE-DOCS: run every docs-check validator and exit non-zero if any fails.
#
# Run after editing docs or after a code commit that moves line numbers:
#   bash scripts/docs_check/run_all.sh
#
# What each check proves -- read before quoting the totals as "accurate":
#
#   validate_docs   markdown fences balanced, inline backticks even, and every
#                   local link + #anchor target exists.
#   check_tables    markdown tables: header/separator/row column counts agree,
#                   pipes inside inline code spans are not counted.
#   audit_anchors   every `foo.rs:N` / `foo.rs:N-M` anchor in the docs points at
#                   a line that EXISTS and is not blank. LIMITATION: it does not
#                   prove the line says what the prose claims. A doc can pass
#                   with anchors on the wrong line -- someone must read them.
#   check_sec_refs  every "§N.N" cross-reference resolves to a real heading in
#                   the document it is attributed to.
#
# audit_anchors and check_sec_refs only scan prose: lines inside fenced code
# blocks are skipped, so examples and tables holding code do not raise noise.
#
# Root is derived from this file's own location, so the whole suite works from a
# clean clone without editing any path.

set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
rc=0
fail=0

for script in validate_docs check_tables audit_anchors check_sec_refs; do
    out="$(python3 "$here/$script.py" 2>&1)"
    code=$?
    # Each check prints "=== NAME (n errors) ===" as its summary, but not
    # necessarily as its first line (validate_docs.py leads with a blank line),
    # so match on the "===" prefix instead of assuming a position.
    summary="$(printf '%s\n' "$out" | grep -m1 '^===' || true)"
    printf '%s\n' "${summary:-$script: no summary line emitted}"
    if [ "$code" -ne 0 ]; then
        printf '%s\n' "$out" | grep -v '^===' | grep -v '^[[:space:]]*$' \
            | sed 's/^/  /' || true
        rc=1
        fail=$((fail + 1))
    fi
done

if [ "$fail" -eq 0 ]; then
    echo "GATE-DOCS: PASS (4/4 checks clean)"
else
    echo "GATE-DOCS: FAIL ($fail check(s) reported errors)"
fi
exit "$rc"
