#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FIXTURE="$(mktemp)"
trap 'rm -f "$FIXTURE"' EXIT

cat >"$FIXTURE" <<'EOF'
# Changelog

## [0.6.1] - 2026-07-19

### Fixed

- Curated wording.

## [0.6.0] - 2026-07-18

- Older release.
EOF

EXPECTED='## [0.6.1] - 2026-07-19

### Fixed

- Curated wording.'
ACTUAL="$(bash "$ROOT/scripts/extract-release-notes.sh" v0.6.1 "$FIXTURE")"
[[ "$ACTUAL" == "$EXPECTED" ]]

if bash "$ROOT/scripts/extract-release-notes.sh" v9.9.9 "$FIXTURE" >/dev/null 2>&1; then
  echo "missing section unexpectedly succeeded" >&2
  exit 1
fi

if bash "$ROOT/scripts/extract-release-notes.sh" 0.6.1 "$FIXTURE" >/dev/null 2>&1; then
  echo "untagged version unexpectedly succeeded" >&2
  exit 1
fi

cat >>"$FIXTURE" <<'EOF'

## [0.6.1] - duplicate

- Duplicate section.
EOF
if bash "$ROOT/scripts/extract-release-notes.sh" v0.6.1 "$FIXTURE" >/dev/null 2>&1; then
  echo "duplicate section unexpectedly succeeded" >&2
  exit 1
fi

cat >"$FIXTURE" <<'EOF'
## [0.6.1] - 2026-07-19

## [0.6.0] - 2026-07-18

- Older release.
EOF
if bash "$ROOT/scripts/extract-release-notes.sh" v0.6.1 "$FIXTURE" >/dev/null 2>&1; then
  echo "empty section unexpectedly succeeded" >&2
  exit 1
fi

echo "  5 passed, 0 failed"
