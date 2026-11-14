#!/bin/sh
# Release bundles must contain all three platforms built from the same commit.
set -eu
printf '%s\n' 'Use the Release binaries workflow and follow RELEASING.md; a single-host release would leave the other platforms stale.' >&2
exit 1
