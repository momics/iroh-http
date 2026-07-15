#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
txt_binary="$(mktemp "${TMPDIR:-/tmp}/iroh-ios-txt-contract.XXXXXX")"
lifecycle_binary="$(mktemp "${TMPDIR:-/tmp}/iroh-ios-lifecycle-contract.XXXXXX")"
trap 'rm -f "$txt_binary" "$lifecycle_binary"' EXIT

xcrun swiftc \
  -warnings-as-errors \
  "$here/../Sources/PeerTxtPolicy.swift" \
  "$here/PeerTxtPolicyContract.swift" \
  -o "$txt_binary"
"$txt_binary"

xcrun swiftc \
  -warnings-as-errors \
  "$here/../Sources/DiscoveryLifecycle.swift" \
  "$here/DiscoveryLifecycleContract.swift" \
  -o "$lifecycle_binary"
"$lifecycle_binary"
