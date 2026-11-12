# Design docs index

Status was taken from each doc's own status line and then checked against the
code on `main` at the time of writing. Docs are point-in-time records: a doc
whose own status line says "Draft" or "Nothing here is implemented" may have
been overtaken by the code, and the table says so.

| Date | Doc | Status | Evidence |
|------|-----|--------|----------|
| 2026-09-02 | [Unify the Target Model](2026-09-02-unify-target-model-design.md) | Implemented (spot-checked, not audited line by line) | Doc says "Draft". One `TargetTriple` now lives in `core::target`, the shim `intent.rs` imports it, and `detect_toolchain` takes `Option<&TargetTriple>`. |
| 2026-09-03 | [HTTP registry over R2 + CDN](2026-09-03-http-registry-design.md) | Not implemented | Doc says "Draft". `GitTransport` is the only `RegistryTransport` (see `src/sources/registry/transport.rs`); `sparse+https://` appears only in comments. Only the transport abstraction it called for landed. |
| 2026-09-07 | [Crate and module layout review](2026-09-07-crate-layout-review.md) | Implemented (recommendation followed) | Verdict is to keep the single crate; there is still one crate and no workspace split. |
| 2026-09-07 | [Extensibility audit](2026-09-07-extensibility-audit.md) | Implemented (audit, nothing to build) | A measurement of shimming cJSON, libuv and zstd; all three now have canaries under `ci/canary/`. |
| 2026-09-07 | [Manifest schema audit](2026-09-07-manifest-schema-audit.md) | Unverified | An audit whose findings are fixes of many sizes. Not checked finding by finding. |
| 2026-09-11 | [Native configure-style probes](2026-09-11-native-probes-design.md) | Implemented | `src/core/probe.rs` and `src/builder/probe.rs` exist; the `curl-config` and `cpython-config` canaries answer hundreds of configure questions through them. |
| 2026-09-12 | [Native probes under MSVC (spike)](2026-09-12-msvc-probes-spike.md) | Partly implemented | Doc says "no production code changes". Its main fix landed: `msvc.rs` maps Unix-only `libs` such as `m` to nothing (`msvc_library_arg`). The other plan items (surfacing the linker's reason, lifting the probe tests off `cfg(not(windows))`) were not checked, and `tests/cli_integration/probes.rs` still has `cfg(not(windows))` tests. |
| 2026-09-12 | [openssl: generate, don't vendor](2026-09-12-openssl-generated-sources.md) | Implemented | Doc says "Decision + canary"; `ci/canary/openssl/` exists and a later commit publishes OpenSSL as a registry entry. |
| 2026-10-24 | [Host tools](2026-10-24-host-tools-design.md) | Implemented (its status line is out of date) | Doc says "Nothing here is implemented". `Target::host`, `validate_host_tools` and sibling-target dependencies are in the code; commits #167, #172 and #177 cover phases 1-3. The cross-compiling section was not checked. |
| 2026-10-24 | [Registry entries from shims](2026-10-24-registry-entries-from-shims.md) | Partly implemented | Findings and proposals. G1-G3, G8 (`[package] build_tools`) and G10 are marked fixed; G4-G6, G11, G13 and G14 are marked not fixed. |
