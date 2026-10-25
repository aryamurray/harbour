#!/usr/bin/env bash
# Reproduce the Seastar spike from docs/superpowers/specs/*-redpanda-feasibility.md.
#
# Run it as root in a Debian 13 (trixie) container with this repository mounted:
#
#   docker run --rm -v "$PWD":/src:ro -w /work rust:latest \
#     bash /src/docs/superpowers/specs/redpanda-seastar-spike/reproduce.sh
#
# It builds Harbour, fetches Seastar at the commit Redpanda v26.2.2 pins, generates
# the inputs Seastar's own build generates (three ragel parsers, metrics2.pb.{h,cc}),
# builds the library with the manifest beside this script, then builds and runs a
# one-coroutine reactor program against it. Each compiler process can take about
# 1 GB, so `JOBS` (default 5) matters: one job per core was killed by the kernel
# for lack of memory in a 7.7 GiB container.
set -euo pipefail

spike="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
jobs="${JOBS:-5}"

apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
  libboost-dev libboost-program-options-dev libboost-filesystem-dev \
  libboost-thread-dev libboost-system-dev libfmt-dev liblz4-dev libyaml-cpp-dev \
  libc-ares-dev libssl-dev libhwloc-dev libsctp-dev liburing-dev libprotobuf-dev \
  protobuf-compiler libabsl-dev ragel python3 pkg-config valgrind libnuma-dev \
  xfslibs-dev curl ca-certificates >/dev/null

mkdir -p /work && cd /work

# Harbour, from the mounted checkout (without its host `target/` or `.git`).
mkdir -p harbour-src
tar -C /src --exclude=./target --exclude=./.git -cf - . | tar -C harbour-src -xf -
(cd harbour-src && cargo build --release 2>&1 | tail -1)
harbour=/work/harbour-src/target/release/harbour

# Seastar at the commit redpanda's bazel/thirdparty/seastar.BUILD builds, and the
# unordered_dense header it includes (pinned the same way).
mkdir -p seastar-dl && curl -fsSL \
  https://github.com/redpanda-data/seastar/archive/5bf36c507142f1f82cd92ca78be3adc6030f4c3f.tar.gz \
  | tar -xz -C seastar-dl
mv seastar-dl/seastar-5bf36c507142f1f82cd92ca78be3adc6030f4c3f seastar
mkdir -p ud-dl && curl -fsSL \
  https://github.com/martinus/unordered_dense/archive/f30ed41b58af8c79788e8581fe57a6faf856258e.tar.gz \
  | tar -xz -C ud-dl
mkdir -p seastar/include/ankerl
cp ud-dl/unordered_dense-*/include/ankerl/unordered_dense.h seastar/include/ankerl/

# What seastar.BUILD's three `ragel` genrules and its `cc_proto_library` produce.
cd seastar
for f in chunk_parsers request_parser response_parser; do
  ragel -G2 -o "include/seastar/http/$f.hh" "src/http/$f.rl"
done
protoc -I src/proto --cpp_out=src/proto src/proto/metrics2.proto
cp "$spike/Harbour.toml" Harbour.toml

echo "== building seastar (-j$jobs)"
time "$harbour" build -j "$jobs"

echo "== building and running the consumer"
cd /work && rm -rf hello && cp -R "$spike/hello" hello && cd hello
"$harbour" build -j "$jobs"
./.harbour/target/debug/bin/hello --smp 1 --memory 256M --overprovisioned --reactor-backend=epoll
