#!/usr/bin/env bash
# zlib 1.3.1 -- the original canary.
#
# Nothing else in CI builds third-party code, which is why the archive bug
# (`ar r` matching members by name, so a renamed source left its stale object
# behind) was found by hand on a real package instead of by a test. The
# fixtures in tests/cli_integration.rs are two- and three-file projects; a
# library with fifteen translation units, a public header its consumers must
# find, and platform-conditional defines exercises paths those cannot.
#
# zlib was the first canary because it has no dependencies, builds in
# seconds, and has a behaviour worth asserting: compress a buffer,
# decompress it, compare. Deliberately *not* openssl -- 1100+ sources per
# platform would dominate the CI bill on a free-tier account.
#
# The body of this script now lives in ../lib.sh, shared with the other
# canaries. It was four-fifths boilerplate, and the one part that matters --
# "a second build must reuse everything" -- is exactly the kind of check that
# rots in the copy nobody is looking at.
#
# Usage: ci/canary/zlib/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

URL="https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz"
SHA256=9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23

canary_standard_run \
  "$here" "$URL" "$SHA256" "zlib-1.3.1" "zlib" "zlib-canary" "15" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-zlib}"
