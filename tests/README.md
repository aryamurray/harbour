# Tests

`cargo test --test cli_integration` runs the CLI integration tests: one test crate
(`tests/cli_integration/main.rs`), one module per area, shared helpers in
`common.rs`. Every test runs `harbour` in a throwaway HOME and never touches the
network (see the header of `main.rs`).

Where a new test goes:

- It belongs to an existing area (`probes`, `generators`, `registry`, ...): add it
  to that module, next to its neighbours.
- It is a **new area**: add a **new module** (`tests/cli_integration/<area>.rs`
  plus one `mod <area>;` line in `main.rs`). Do not append it to the tail of an
  unrelated module.
- A helper used by one area stays in that area's module; the first time a second
  area needs it, move it to `common.rs` and mark it `pub(crate)`.

Why: when the whole suite was one 13,000-line file, nearly every PR appended its
tests to the end of it, so each merge to `main` made the next PR conflict at the
same spot. Separate modules keep unrelated PRs from touching the same lines.

## Hermetic by default

A test's result must not depend on the machine it runs on.

- Tests that run the binary build their command through `harbour(&home)`, which
  calls `common::isolate` (in `tests/common/mod.rs`): the one place that lists
  what is redirected (`HOME`, the XDG directories) and cleared (vcpkg, flags, the
  test registry override). Do not copy that block into a new file.
- Unit tests must not build a `GlobalContext` with `new()` or `with_cwd()`: those
  read the developer's real global config. Use `with_dirs(home, cwd)` or
  `with_cwd_isolated(cwd)`. A test in `src/util/context.rs` fails if you do.
