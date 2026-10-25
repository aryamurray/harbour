#!/usr/bin/env bash
# lz4 1.9.4 -- the leaf-dependency canary for the Redpanda feasibility study.
#
# Four C files in one directory, no generated header, nothing conditional. It
# exists because Redpanda (via Seastar's RPC compressor and the Kafka batch
# codec) needs lz4 1.9.4, and because it is the cheapest way to prove the
# dependency is not the obstacle: when this is red, something broad is broken.
#
# The consumer touches each of the four translation units on purpose (see
# `consumer/src/main.c`); the object count is the other half, and catches a
# source list that shrank while the consumer still linked.
#
# Usage: ci/canary/lz4/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

# lz4's published release asset (the same bytes as the auto-generated
# archive, but with its own .sha256 beside it).
URL="https://github.com/lz4/lz4/releases/download/v1.9.4/lz4-1.9.4.tar.gz"
SHA256=0b0e3aa07c8c063ddf40b082bdf7e37a1562bda40a0ff5272957f3e987e0e54b

canary_standard_run \
  "$here" "$URL" "$SHA256" "lz4-1.9.4" "lz4" "lz4-canary" "4" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-lz4.$$}"
