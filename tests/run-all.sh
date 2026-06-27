#!/usr/bin/env bash
# tests/run-all.sh — Run all test suites for a given runtime.
#
# Usage:
#   ./tests/run-all.sh                  # Node + Deno
#   ./tests/run-all.sh --node           # Node only
#   ./tests/run-all.sh --deno           # Deno only
#   ./tests/run-all.sh --tauri          # Tauri webview (requires tauri-driver)
#   ./tests/run-all.sh --node --tauri   # any combination

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

SELECTED=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --node) SELECTED+=(node); shift ;;
    --deno) SELECTED+=(deno); shift ;;
    --tauri) SELECTED+=(tauri); shift ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

# Default selection (Tauri is opt-in: it needs tauri-driver + a native webview driver).
if [[ ${#SELECTED[@]} -eq 0 ]]; then
  SELECTED=(node deno)
fi

selected() {
  local want="$1"
  for s in "${SELECTED[@]}"; do
    [[ "$s" == "$want" ]] && return 0
  done
  return 1
}

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
NC='\033[0m'

echo "iroh-http Test Suite"
echo "===================="

PASS=0
FAIL=0

if selected node; then
  echo ""
  echo -e "${BOLD}── Node.js ──${NC}"
  cd "$ROOT_DIR"
  if node --test tests/runners/node.mjs; then
    echo -e "${GREEN}✓ Node PASSED${NC}"
    PASS=$((PASS + 1))
  else
    echo -e "${RED}✗ Node FAILED${NC}"
    FAIL=$((FAIL + 1))
  fi
fi

if selected deno; then
  echo ""
  echo -e "${BOLD}── Deno ──${NC}"
  cd "$ROOT_DIR"
  if deno test -A tests/runners/deno.ts; then
    echo -e "${GREEN}✓ Deno PASSED${NC}"
    PASS=$((PASS + 1))
  else
    echo -e "${RED}✗ Deno FAILED${NC}"
    FAIL=$((FAIL + 1))
  fi
fi

if selected tauri; then
  echo ""
  echo -e "${BOLD}── Tauri (webview) ──${NC}"
  if ! command -v tauri-driver >/dev/null 2>&1; then
    echo -e "${RED}✗ Tauri FAILED — tauri-driver not found${NC}"
    echo -e "${YELLOW}  Install it with: cargo install tauri-driver --locked${NC}"
    echo -e "${YELLOW}  On Linux you also need WebKitWebDriver (apt: webkit2gtk-driver) and xvfb.${NC}"
    FAIL=$((FAIL + 1))
  else
    cd "$ROOT_DIR/tests/tauri-app"
    [[ -d node_modules ]] || npm install
    # Use a virtual display on Linux so the webview can run headless.
    if command -v xvfb-run >/dev/null 2>&1; then
      RUNNER=(xvfb-run npm run test:e2e)
    else
      RUNNER=(npm run test:e2e)
    fi
    if "${RUNNER[@]}"; then
      echo -e "${GREEN}✓ Tauri PASSED${NC}"
      PASS=$((PASS + 1))
    else
      echo -e "${RED}✗ Tauri FAILED${NC}"
      FAIL=$((FAIL + 1))
    fi
  fi
fi

echo ""
echo "════════════════════════════════════════════════════════════"
echo -e "  ${GREEN}Passed: $PASS${NC}  ${RED}Failed: $FAIL${NC}"
echo ""
[[ $FAIL -eq 0 ]]
