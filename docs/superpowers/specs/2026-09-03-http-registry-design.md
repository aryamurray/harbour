# Design: HTTP registry over R2 + CDN, alongside git

**Date:** 2026-09-03
**Status:** Draft — awaiting review

---

## Goal

Serve the public Harbour registry over Cloudflare R2 fronted by a CDN, for fast
and robust distribution. Keep the existing git transport as a first-class option.

The two serve different purposes rather than competing:

- **R2 + CDN** is the actual public registry -- the cloud is simply where a
  registry belongs for distribution.
- **Git** is the development path and the private path: local iteration on
  shims, and private or enterprise registries holding proprietary software.
  Nobody resolves over HTTP while developing.

**Both transports, one index format.** Two formats would mean two parsers and two
resolution paths that can disagree — the failure mode behind several of this
codebase's worst bugs (`linkplan` computing a correct link order the build path
ignored; fresh and locked resolution producing different package identities).

The dev/production split makes this argument stronger rather than weaker. If git
is what you develop against and R2 is what ships, the two must give **identical
answers** — otherwise development stops predicting production, and a package
resolves locally but fails for users. Sharing the format is what buys that
parity.

To be fair to the alternative: the git transport *could* keep its directory scan
and work correctly. What it would cost is precisely the parity above, not
correctness. This is a deliberate choice, not a forced one.

---

## Why the current format cannot work over HTTP

Two independent blockers, both verified in the code rather than assumed.

### 1. Version discovery depends on directory listing

`RegistrySource::query` (`src/sources/registry/mod.rs:664`) handles a version
*range* by calling `list_available_versions(name)`, which scans the filesystem.
That works because the git index is cloned locally. Over HTTP there is no
directory listing, and the layout is one file per version
(`z/zlib/1.3.1.toml`, from `shim_path`), so a client would have to guess version
numbers.

### 2. Resolution downloads sources to learn dependencies (a problem for git too)

The same `query` calls `fetch_package_source()` and then
`load_package_from_source()` to read dependencies out of the package's own
manifest — for *every candidate version*, including ones the solver rejects.
Over a CDN that is a source archive downloaded per candidate.

This second point is **not an HTTP-shaped problem** and must be fixed regardless
of transport: over git, resolving a version range still means a fetch per
candidate version. Moving dependencies into the index is something the git path
wants on its own merits; HTTP merely makes it unavoidable rather than optional.

It is also why the eager-versus-lazy question for transitive git and registry
dependencies was largely an artifact of the current format. That question has
since been settled independently in favour of lazy, on-demand discovery, which
this design leaves intact -- the resolver asks for candidates when it needs them
and does not care whether the answer came from a clone or a CDN.

---

## Index format

### Tier 1: package index — one file per package

Path: `index/<shard>/<name>`, sharded to avoid huge directories.

One record per version, carrying everything needed to **resolve** and nothing
more:

- `name`, `version`, `yanked`
- `deps`: name, version requirement, optional/default-features, kind
- `checksum`: sha256 of the artifact
- `shim`: path to the tier-2 record

Append-only in practice: publishing a new version adds a record. That makes the
file diff-friendly in git and cheap to revalidate over HTTP.

### Tier 2: shim — one file per version

The existing shim content: source location, `surface_override`, `patches`,
`prebuild`, conditional sources, `metadata`. Everything needed to **build** and
nothing needed to resolve.

Fetched only for versions the solver actually selects. A resolution that
considers twenty candidate versions of a package reads one tier-1 file and zero
tier-2 files.

### Why split the tiers

Resolution is metadata-only, which is what makes an HTTP registry viable. It also
means the hot path is small, highly cacheable files rather than archives.

---

## Transports

A transport answers two questions: fetch an index path, and fetch an artifact.

| Transport | Scheme | Mechanism |
|---|---|---|
| Git | `git+https://` | clone or pull, read from the working tree |
| Sparse HTTP | `sparse+https://` | `GET`, with ETag or `If-Modified-Since` revalidation |

Scheme-prefixed URLs follow Cargo's convention, which makes a manifest
self-documenting about which protocol a registry speaks.

The git transport keeps working exactly as it does today from a user's
perspective. Internally it gets simpler: the directory scan in
`list_available_versions` is replaced by reading a tier-1 file, shared with the
HTTP path. No sparse fetching, no cache headers, no revalidation -- it reads the
same file out of the clone.

### The tier-1 index in a git registry is committed

Generated from the shims by CI, committed, and **checked for freshness by CI** --
the standard generated-file-checked-in pattern.

The alternative, having the client build the index by scanning a clone, is
tidier in diffs but reintroduces exactly the parity risk this design exists to
remove: development would compute its index differently from the way production
serves it. Committing it means what you resolve against locally is
byte-identical to what R2 serves.

The cost is a slightly noisier diff whenever a shim changes.

---

## Caching and integrity

**Artifacts are immutable and content-addressed.** Served with
`Cache-Control: public, max-age=31536000, immutable`, so the CDN never
revalidates them.

**Index files are mutable** — a new version appends a record — so they are served
with a short TTL and revalidated by ETag. This is the same split crates.io uses
between its index and its static artifact host.

**The CDN is not trusted.** Every artifact's sha256 lives in the tier-1 index and
is verified after download. The lockfile pins the checksum, so a rebuild
verifies without contacting the network and a substituted artifact fails loudly.

**Yanking, not deletion.** Immutability means a bad version cannot be withdrawn
by deleting it — existing lockfiles would break. A `yanked` flag keeps it
fetchable for anyone who already depends on it while excluding it from new
resolutions.

---

## Vendoring

Artifacts are vendored into R2 rather than shims pointing at upstream hosts.

A shim that points at `github.com/madler/zlib` at a tag makes every build depend
on a third party continuing to serve that tag unchanged. Tags get moved and
deleted, hosts rate-limit, and the build stops being reproducible. Vendoring also
gives content-addressed paths, which is what makes the immutable caching above
possible.

Cost is storage plus a fetch-and-repack step at publish time. R2 charges no
egress, which is the reason it is a good fit for this.

---

## Publishing

Deliberately **not settled here**. The index format is needed under any
publishing model, and settling authority is easier once there is something to
publish.

The likely shape, consistent with the curated-overlay governance already agreed:
shims are authored and reviewed as pull requests in a git repository, and CI
derives the tier-1 index and vendored artifacts and uploads them to R2. Git is
then the source of truth and R2 a rebuildable derivative. That is a decision to
make later, not an assumption of this design.

---

## What a registry package may depend on

**Registry packages only.** This follows Cargo, where `cargo publish` rejects
`path` and `git` dependencies outright, and matches where vcpkg and Conan
independently landed: ports depend on ports, recipes require recipes.

The reason is that a published package must be resolvable and reproducible
**from the index alone**. A `path` dependency is meaningless once the package
leaves the machine that published it. A `git` dependency is uncurated and not
guaranteed to remain available -- and although Harbour's shim format already
requires a full 40-character SHA, so such a dependency is content-pinned, the
objections that remain are availability and curation rather than immutability.

Both are **errors at index generation**, not warnings. A skipped dependency
produces an index that resolves cleanly and then fails at build time, which is
the wrong direction for that failure.

### vcpkg dependencies are not an exception

They looked like one, and the tier split already answers it: tier 1 carries what
the solver needs, and a vcpkg dependency is never resolved against the registry
-- the environment satisfies it. So it is not a tier-1 concern at all, and
belongs with the rest of the build recipe in tier 2. Index generation omits it
without complaint.

### The road not taken

Nix flakes permit fully heterogeneous inputs, and that is safe there because
every input is content-addressed and pinned in `flake.lock`. Heterogeneity is
safe only when immutability is enforced a layer below, which Harbour does for
registry artifacts but not uniformly. Go modules take the opposite route --
every dependency *is* a repository, made safe by a checksum database -- but that
requires upstream to ship a module manifest, which C upstreams will not do. That
is the same constraint that forced the curated-overlay model, so the option is
closed.

---

## Sequencing

1. **Index format and parser**, shared. Includes moving dependency metadata into
   tier 1 and dropping the download-to-resolve behaviour.
2. **Transport abstraction**, and port git onto it with no behavioural change.
3. **Sparse HTTP transport**, with ETag revalidation and an on-disk cache.
4. **Publish tooling**: derive index and vendored artifacts, upload with correct
   cache headers.
5. **Checksum verification and lockfile pinning** end to end.

1 and 2 are prerequisites. 3 and 4 can proceed in parallel afterwards.

---

## Migration

The format change costs almost nothing today and a great deal later: the registry
currently holds no real packages. Doing it before the registry is populated
avoids rewriting every published shim.

Existing per-version shim files remain the tier-2 records, so the change is
additive at that tier — what is new is the tier-1 index above them.
