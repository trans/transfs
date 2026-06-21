# transfs

transfs is an experimental content-addressed file archive for end users.

It stores file bytes once by SHA-256, keeps document identity and metadata in
append-only claim logs, and materializes a rebuildable SQLite index for listing,
search, and the read-only FUSE mount. The working model is: blobs are anonymous
content; documents are named, tagged, versioned handles that point at blobs.

See [docs/architecture.md](docs/architecture.md) for the full design.

## Status

This is early software. The current implementation supports:

- content-addressed blob storage with deduplication
- append-only per-document claim logs
- document names, tags, and version history
- content-derived MIME type detection through `libmagic`
- rebuildable SQLite index
- `transfs check` integrity reporting
- a read-only query-path FUSE mount

Still deferred: manifest-backed trees/collections, checkout/checkin editing,
export/import, garbage collection, log compaction, signing, and cross-machine
merge tooling.

## Requirements

- Crystal `>= 1.16.0`
- SQLite development libraries
- `libmagic` development libraries
- FUSE 3 if you want to use the mount

Examples:

```sh
# Debian/Ubuntu
sudo apt install crystal sqlite3 libsqlite3-dev libmagic-dev fuse3

# Fedora
sudo dnf install crystal sqlite-devel file-devel fuse3

# Arch
sudo pacman -S crystal sqlite file fuse3
```

## Build

```sh
shards install
crystal build src/cli.cr -o bin/transfs
```

If your Crystal cache is not writable, set it explicitly:

```sh
CRYSTAL_CACHE_DIR=/tmp/crystal-cache crystal build src/cli.cr -o bin/transfs
```

## Store Location

By default, the CLI uses `test/store` under the current directory. Override it
with either `--store` or `TRANSFS_STORE`:

```sh
bin/transfs --store demo/store list
TRANSFS_STORE=demo/store bin/transfs list
```

The on-disk layout is:

```text
<store>/
  blobs/<hh>/<sha256>
  .transfs/docs/<hh>/<doc-id>.log
  .transfs/index.db
```

The database is a cache. Delete it and run `transfs reindex` to rebuild it from
the logs.

## Basic Usage

Add a file:

```sh
bin/transfs --store demo/store add ./paper.pdf "paper.pdf"
```

The command prints a short document id prefix. Use that prefix with later
commands:

```sh
bin/transfs --store demo/store tag <id> -- finance year=2024 stars=4
bin/transfs --store demo/store list
bin/transfs --store demo/store find tag:finance
bin/transfs --store demo/store find type:application/pdf
bin/transfs --store demo/store show <id>
bin/transfs --store demo/store cat <id> > copy.pdf
```

Add a new version:

```sh
bin/transfs --store demo/store addversion <id> ./paper-v2.pdf
bin/transfs --store demo/store versions <id>
```

Rename and untag:

```sh
bin/transfs --store demo/store rename <id> "final-report"
bin/transfs --store demo/store untag <id> -- stars=4
```

Tags are opaque strings in the claim log. The index treats `=` as a hierarchy
separator, so `year=2024` becomes the facet path `year/2024`. Boolean tags are
bucketed under `tag/`, so `finance` becomes `tag/finance`.

## Integrity Checks

Run:

```sh
bin/transfs --store demo/store check
```

`check` reports:

- malformed middle log records as errors
- malformed trailing records as recoverable torn-tail warnings
- document id/create-claim mismatches
- missing version blobs
- blob hash mismatches
- orphan blobs as warnings

Warnings exit successfully. Integrity errors exit nonzero.

Rebuild the index:

```sh
bin/transfs --store demo/store reindex
```

If a corrupt document log is encountered, `reindex` reports it, skips that
document, indexes the rest, and exits nonzero.

## Query-Path Mount

The mount is read-only. Mutations go through the CLI.

```sh
mkdir -p demo/mnt
bin/transfs --store demo/store mount demo/mnt
```

In another shell:

```sh
ls demo/mnt/                  # facet menu: type/ year/ stars/ tag/ ...
ls demo/mnt/=/                # recent documents
ls demo/mnt/tag/finance/=/    # documents tagged finance
ls demo/mnt/year/2024/        # drill into a facet hierarchy
cat demo/mnt/tag/finance/=/paper.pdf
```

Unmount when done:

```sh
fusermount3 -u demo/mnt
```

Path rules:

- facet view is the default
- a lone `=` toggles to document view
- `key=value` is the same as `key/value`
- path components narrow by AND
- documents and facets are not co-listed

## Demo

Generate a throwaway demo store:

```sh
demo/seed.sh
```

The script builds `bin/transfs` if needed, creates `demo/store`, adds sample
documents, tags them, and prints mount commands to try.

## Development

Run specs:

```sh
CRYSTAL_CACHE_DIR=/tmp/crystal-cache crystal spec
```

Build the CLI:

```sh
CRYSTAL_CACHE_DIR=/tmp/crystal-cache crystal build src/cli.cr -o bin/transfs
```

Format edited Crystal files:

```sh
crystal tool format src spec
```

## License

MIT. See [LICENSE](LICENSE).

