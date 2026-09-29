#!/usr/bin/env bash
# Compatibility gate for installations that still require change evidence.
# NEEDLE's native shipped-work/acceptance gates remain authoritative.
set -uo pipefail

pre=$(cat .needle-predispatch-sha 2>/dev/null || true)
[ -z "$pre" ] && exit 0

# Bound the producer itself. An early-exiting grep under pipefail can give
# git SIGPIPE and reject a successful dispatch with a long commit history.
if commits=$(git rev-list --count --max-count=1 --end-of-options "${pre}..HEAD" 2>/dev/null) \
    && [[ "$commits" == 1 ]]; then
    exit 0
fi

# Consume the complete status output too; large dirty trees must not turn
# successful change detection into another SIGPIPE failure.
if git status --porcelain 2>/dev/null \
    | grep -v '^?? \.needle-predispatch-sha$' \
    | grep . >/dev/null; then
    exit 0
fi
exit 1
