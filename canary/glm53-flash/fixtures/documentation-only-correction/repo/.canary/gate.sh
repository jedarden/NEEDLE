#!/usr/bin/env bash
set -euo pipefail
grep -Fq 'effort' README.md
! grep -Fq 'effert' README.md
