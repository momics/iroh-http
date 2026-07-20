#!/usr/bin/env bash
set -euo pipefail

# Print exactly one curated CHANGELOG.md release section. The build workflow
# uses this instead of regenerating notes from commits, so the reviewed
# changelog is the public GitHub Release body.

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 vX.Y.Z [CHANGELOG.md]" >&2
  exit 2
fi

TAG="$1"
VERSION="${TAG#v}"
CHANGELOG="${2:-CHANGELOG.md}"

if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid release tag: $TAG" >&2
  exit 2
fi

awk -v heading="## [$VERSION]" '
  /^## \[/ {
    suffix = substr($0, length(heading) + 1)
    is_target = index($0, heading) == 1 && (suffix == "" || index(suffix, " - ") == 1)
    if (is_target) {
      matches += 1
      capture = 1
    } else {
      capture = 0
    }
  }
  capture {
    lines[++line_count] = $0
    if ($0 !~ /^## \[/ && $0 !~ /^[[:space:]]*$/) content_lines += 1
  }
  END {
    if (matches != 1 || content_lines == 0) exit 1
    for (i = 1; i <= line_count; i += 1) print lines[i]
  }
' "$CHANGELOG" || {
  echo "expected exactly one non-empty changelog section for $TAG in $CHANGELOG" >&2
  exit 1
}
