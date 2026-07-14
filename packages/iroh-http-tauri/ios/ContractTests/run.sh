#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
binary="$(mktemp "${TMPDIR:-/tmp}/iroh-ios-txt-contract.XXXXXX")"
trap 'rm -f "$binary"' EXIT

xcrun swiftc \
  -warnings-as-errors \
  "$here/../Sources/PeerTxtPolicy.swift" \
  "$here/PeerTxtPolicyContract.swift" \
  -o "$binary"
"$binary"
