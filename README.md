# transfs

transfs is an experimental content-addressed file archive. It stores file
bytes once by SHA-256 and keeps each document's name, tags, and version history
in an append-only claim log. SQLite is a rebuildable index, not the source of
truth. This Rust implementation can read stores written by the original
[Crystal transfs](https://github.com/trans/transfs.cr), and Crystal can read its
stores.

The library provides claims, the blob store, log replay, document mutations,
query-path parsing, and integrity checking. The native build adds a SQLite and
`libmagic` index, a CLI, and a read-only FUSE mount. The library compiles for
WASM with `--no-default-features`; browser storage and an API binding are still
needed to use it in a browser.

## Build

The native build requires SQLite, `libmagic`, and FUSE 3 development libraries.
To run the mount, the machine also needs `/dev/fuse` access.

```sh
cargo test
cargo test --no-default-features
cargo build --bin transfs
```

If the `wasm32-unknown-unknown` target is installed, the portable library can
be checked with:

```sh
cargo check --no-default-features --target wasm32-unknown-unknown --lib
```

## Use

The default store is `test/store` in the current directory. Set
`TRANSFS_STORE` or put `--store DIR` before the command to choose another.

```sh
cargo run -- --store demo/store add ./paper.pdf paper.pdf
cargo run -- --store demo/store list
cargo run -- --store demo/store find tag:finance
cargo run -- --store demo/store check
mkdir -p demo/mnt
cargo run -- --store demo/store mount demo/mnt
```

The mount is read-only, matching the current Crystal implementation. It presents
facets as directories; a lone `=` switches to matching documents. The FUSE API
supports writes, but transfs has not defined the commit behavior for edits
through a mount. A live host smoke test on 2026-10-02 passed facet and document
listing, content reads, filesystem statistics, and read-only rejection.

To check a live mount on a host with `/dev/fuse`, run these `just` tasks. The
example store stays under ignored `target/fuse-smoke/`.

```sh
just fuse-device
just fuse-build
just fuse-prepare
just fuse-mount       # foreground; leave this terminal open
```

In a second terminal, from this repository:

```sh
just fuse-verify
just fuse-unmount
```

`just --list` also shows the individual `fuse-list`, `fuse-read`, `fuse-stat`,
and `fuse-readonly` checks.

The store layout is:

```text
<store>/
  blobs/<hh>/<sha256>
  .transfs/docs/<hh>/<document-id>.log
  .transfs/index.db
```

The database is disposable. Delete it or run `transfs reindex` to rebuild it
from the claim logs. The tests include Crystal-produced claim fixtures and
cross-language store compatibility checks.

## Design notes

[Architecture](docs/architecture.md) describes the storage and query model;
the [user guide](docs/guide.md) walks through the CLI. These documents were
carried over from the Crystal project. Some design and proposal sections still
refer to that implementation; the Rust source in `src/` is the current code.
The [data-centric architecture whitepaper](docs/data_centric_architecture_architecture_whitepaper.md)
proposes a shared Pandora/transfs storage engine and a plan to test it.
The [causal claim model plan](docs/claim-model-plan.md) breaks the first
implementation gate into concrete changes and checks.
