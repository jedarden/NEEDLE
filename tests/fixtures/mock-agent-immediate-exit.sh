#!/bin/sh
# Simulate the agent process that never produces a normal exit status.
# The parent shell observes this as exit_code=-1 with an effectively zero
# duration, matching the production failure shape from GitHub issue #22.
printf '%s\n' "${NEEDLE_ATTEMPT_ID:-missing}" >> "$NEEDLE_ATTEMPT_LOG"
kill -KILL "$$"
