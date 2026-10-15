#!/usr/bin/env bash
# Compatibility entry point; SDK preparation no longer creates or pushes commits.
set -euo pipefail
cd -P "$(dirname "$0")/.."
case "${1:-}" in
  "") exec python3 scripts/sdk-prepare.py ;;
  --check) exec python3 scripts/sdk-prepare.py check ;;
  --verify-each) exec python3 scripts/sdk-prepare.py --verify-each ;;
  *) echo "usage: $0 [--check|--verify-each]" >&2; exit 2 ;;
esac
