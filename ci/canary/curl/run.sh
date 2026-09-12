#!/usr/bin/env bash
# curl 8.22.0, built by Harbour with **no vendored `curl_config.h`**.
#
# This is the canary the probe subsystem exists for. Everything curl's code
# reads out of a configure-generated header is measured here by a probe
# against the real toolchain: 35 header checks, 54 symbol checks that link,
# 6 type checks, 6 constant checks and 7 sizes, written into a generated
# `curl_config.h` in the build tree. The file it replaces was 793 lines per
# (os, arch) pair, and the job that produced it
# (`.github/workflows/harvest.yml:76-86`) said in its own comment that the
# config header "is the part that genuinely cannot be produced on another
# platform".
#
# `ci/canary/curl-config/` is the companion and they answer different
# questions. That one compares Harbour's answers against curl's own cmake,
# question by question, and is the check that the answers are *right*. This
# one compiles 196 real translation units against those answers and runs the
# result, and is the check that they are *sufficient* -- which is not the
# same property, and which nothing short of building curl demonstrates.
#
# Two things worth knowing about how this fails:
#
#   - curl is full of `#error` directives for inconsistent configs, so a
#     missing answer usually fails the compile with curl's own message.
#     Dropping the `HAVE_STRUCT_TIMEVAL` type probe produces "redefinition of
#     'timeval'" from `curl_setup.h:898`; dropping both non-blocking constant
#     probes produces "no non-blocking method was found/used/set" from
#     `curlx/nonblock.c:90`. Both were watched failing.
#   - But not always, which is why there is a consumer. A wrong answer curl
#     tolerates at compile time shows up as a library that links and
#     misbehaves, so the consumer drives real transfers and checks the
#     bytes.
#
# No TLS and no network. `curl_easy_perform` on a `file://` URL is a real
# transfer through curl's whole machinery and touches none of the paths a
# missing TLS backend would.
#
# Usage: ci/canary/curl/run.sh [work-dir]
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$here/../lib.sh"

URL="https://github.com/curl/curl/releases/download/curl-8_22_0/curl-8.22.0.tar.gz"
SHA256=d54dd598bf05927a726deb38df31c6a255ba83ff1de57c5d1464dac3ed8f44a1

# 196 objects: `CSOURCES` from `lib/Makefile.inc`. curl compiles all of them
# unconditionally and empties the disabled ones out with `#ifdef`, so the
# count is the same on every platform -- which makes it a useful assertion
# rather than a platform-specific one.
canary_standard_run \
  "$here" "$URL" "$SHA256" "curl-8.22.0" "curl" "curl-canary" "196" \
  "${1:-${TMPDIR:-/tmp}/harbour-canary-curl}"
