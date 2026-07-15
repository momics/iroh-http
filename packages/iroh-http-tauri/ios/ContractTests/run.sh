#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lifecycle_binary="$(mktemp "${TMPDIR:-/tmp}/iroh-ios-lifecycle-contract.XXXXXX")"
trap 'rm -f "$lifecycle_binary"' EXIT

xcrun swiftc \
  -warnings-as-errors \
  "$here/../Sources/DiscoveryLifecycle.swift" \
  "$here/DiscoveryLifecycleContract.swift" \
  -o "$lifecycle_binary"
"$lifecycle_binary"
