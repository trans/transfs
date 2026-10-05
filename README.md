# transfs

transfs is an experimental content-addressed file archive. It stores file
bytes once by SHA-256 and keeps each document's name, tags, and version history
in an append-only claim log. SQLite is a rebuildable index, not the source of
truth. The original [Crystal transfs](https://github.com/trans/transfs.cr) is
retired; the Rust claim format is the only supported store format.

The library provides claims, the blob store, log replay, document mutations,
query-path parsing, and integrity checking. The native build adds a SQLite and
`libmagic` index, a CLI, and a read-only FUSE mount. The library compiles for
WASM with `--no-default-features`; browser storage and an API binding are still
needed to use it in a browser.

## Build

The native build requires SQLite, `libmagic`, and FUSE 3 development libraries.
To run the mount, the machine also needs `/dev/fuse` access.

```sh
cargo test --workspace
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

The mount is read-only. It presents
facets as directories; a lone `=` switches to matching documents. The FUSE API
supports writes, but transfs has not defined the commit behavior for edits
through a mount. The current document-bound format 2 build passed the live host
smoke test on 2026-10-02: facet listing, ordinary and forked content reads,
filesystem statistics, version modification time, and read-only rejection.
Concurrent name paths are covered by mount-view tests.

To check a live mount on a host with `/dev/fuse`, run these `just` tasks. The
example store stays under ignored `target/fuse-smoke-v2-forks/`.

```sh
just fuse-mount       # foreground; leave this terminal open
```

In a second terminal, from this repository:

```sh
just fuse-verify
just fuse-unmount
```

`just --list` also shows the individual `fuse-list`, `fuse-read`, `fuse-stat`,
`fuse-forks`, `fuse-time`, and `fuse-readonly` checks.

The store layout is:

```text
<store>/
  blobs/<hh>/<sha256>
  .transfs/docs/<hh>/<document-id>.log
  .transfs/index.db
  .transfs/writer.json
```

The database is disposable. Delete it or run `transfs reindex` to rebuild it
from the claim logs.

## Directory remote checkpoint

Check a directory remote, publish a checkpoint, then recover it into a new
working store on another device:

```sh
transfs remote-check /mnt/transfs-remote
transfs --store laptop/store publish /mnt/transfs-remote laptop
transfs --store restored/store recover /mnt/transfs-remote
transfs --store restored/store check
```

The optional `laptop` argument is a display label. Each working store mints
its own time-prefixed random writer ID; it cannot be chosen on the command
line. Keep `.transfs/writer.json` with the store when backing it up. Copying a
working store to a second live device also copies its writer ID, so publishing
from one copy will stop the other until it gets a new writer identity.
Run `transfs --store copied/store fork-writer` on the copy to give it a new
writer chain while retaining its local claims, then publish it again.
Stores published with the older manually supplied writer ID start a new
writer chain on their first publish with this version; the old refs remain
readable during recovery.

The recovery target must not exist. A remote stores immutable blobs and CHAMP
packs plus append-only writer refs; it does not contain the live claim logs or
SQLite database. Recovery unions all published writer roots and rebuilds the
index. `publish` and `recover` are the first cold-recovery commands, not yet an
incremental sync into an existing working store. See the
[directory remote format](docs/storage-protocol.md) for its layout and current
limits.

## Design notes

[Architecture](docs/architecture.md) describes the storage and query model;
the [user guide](docs/guide.md) walks through the CLI. These documents were
carried over from the Crystal project. Some design and proposal sections still
refer to that implementation; the Rust source in `src/` is the current code.
The [data-centric architecture whitepaper](docs/data_centric_architecture_architecture_whitepaper.md)
proposes a shared Pandora/transfs storage engine and a plan to test it.
The [causal claim model plan](docs/claim-model-plan.md) breaks the first
implementation gate into concrete changes and checks.
The [claim format 2 specification](docs/claim-format-v2.md) pins durable claim IDs.
