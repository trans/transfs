# Chunked storage

> **Status:** proposal for review, 2026-10-06. Nothing here is built. It turns
> the [chunk study](chunk-study.md) and the [file-type table](file-types.md)
> into a storage design. Two pieces need merkle-champ work first
> ([what it needs from merkle-champ](#what-it-needs-from-merkle-champ)).

Today transfs stores every version of a file as one complete blob. Saving a
3.5 MB GIMP master after a small brush stroke stores another 3.5 MB. With
chunked storage, transfs cuts the file into chunks, stores only the chunks it
has not seen before, compressed, and records the new version as a list of
chunks. The same save costs about 90 KB.

Nothing about documents changes. A version claim still records the SHA-256 of
the complete file, so `cat`, `versions`, forks and every claim work as before.
Chunking changes only how the bytes behind that hash are kept.

## The parts

A chunked file version is made of three kinds of thing:

- **Chunks.** Pieces of the file, cut by FastCDC (about 8 KiB) or, for SQLite,
  one database page each. Each chunk is stored once under its own identity and
  compressed with zstd when that shrinks it.
- **A chunk list.** The version's chunks in order, with each chunk's length.
  It is a merkle-champ `Sequence`, a tree whose shape depends only on its
  contents, so an edit rewrites only the few list nodes near it, and finding
  the chunk that holds byte *n* is one walk down the tree.
- **A representation record.** A small record saying "the content with hash
  *H* is the chunk list with root *R*", plus the chunking settings and where the
  list and chunks are stored. It is how a reader gets from a version claim's
  hash to the bytes.

```text
version claim ── hash H ──▶ representation record (H → list root R, packs)
                                   │
                                   ▼
                       chunk list (Sequence nodes)
                          │      │      │
                          ▼      ▼      ▼
                       chunk  chunk  chunk ...   (shared with other versions)
```

Small files and formats that chunking cannot help stay **whole blobs**, as
today (see [which files are chunked](#which-files-are-chunked)). A whole blob
needs no representation record: `blobs/<hh>/<H>` existing is enough.

## Which files are chunked

The [file-type table](file-types.md) decides, from the file's first bytes and
its size:

| file | stored as |
|---|---|
| SQLite database | chunked: fixed chunks of its page size |
| GIMP image, PNG, anything else of 32 KiB or more | chunked: FastCDC, 2/8/32 KiB |
| WebP, JPEG and other already-compressed media | whole blob |
| anything under 32 KiB | whole blob |

Already-compressed media stay whole because chunking saves them nothing (the
WebP edits cost 100% either way), and a whole blob is a real file. That keeps
DataDungeon's `resolve(key) → path` and Infocomic's `File.open(path)` working
for the images they serve, with no change to either.

## On disk

```text
<store>/
  blobs/<hh>/<H>                       whole blobs, as today
  .transfs/docs/<hh>/<doc-id>.log      claim logs, as today
  .transfs/packs/<pack-hash>           chunks and chunk-list nodes
  .transfs/reps/<hh>/<H>/<rep-id>      representation records
  .transfs/index.db                    index, now also: object → pack, offset
```

- **Chunks and list nodes live in packs**, not one file per chunk. A terabyte
  at 8 KiB is over 100 million chunks, far too many files. Each `add` or
  `addversion` that stores new chunks writes **one pack**: the version's new
  list nodes and new chunks, rooted at the list root. Chunks the store already
  has are not written again, wherever they are.
- **Representation records** are small, write-once files, named by the
  content hash they describe, as the [whitepaper](data_centric_architecture_architecture_whitepaper.md)
  proposes for remotes (`reps/<H>/<rep-id>`). A content hash may have more than
  one record, for example a chunked one and, later, a repacked one.
- **The index gains a table** from object identity to pack, offset and length.
  Like the rest of `index.db` it is a cache: rebuilding it reads each pack's
  index, which sits at the start of the pack.

### The representation record

```json
{
  "format": 1,
  "content": "<H: SHA-256 of the complete file>",
  "length": 3670016,
  "chunking": "fastcdc-2020/2048/8192/32768",
  "root": "<chunk-list root identity>",
  "packs": ["<pack hash>", "..."]
}
```

`chunking` is either the FastCDC parameters or `fixed/<page size>`. `packs`
lists every pack holding this version's list nodes or chunks, so a reader that
has only the record (a remote, or a store being recovered) knows what to fetch.
A later field can name a compression dictionary. The record's id is the SHA-256
of its bytes.

## Writing a version

`add` and `addversion` keep their current shape and the store lock. For a file
that is chunked:

1. Read the file in a stream, cutting chunks and computing the whole-file hash
   *H* as it goes, so files larger than memory work.
2. If a representation of *H* already exists, the bytes are already stored.
   Skip to step 6.
3. For each chunk, look up its identity in the index. Compress the new ones
   with zstd level 3, keeping the raw bytes when compression does not shrink
   them.
4. Build the chunk list and save its nodes. Write one pack holding the new
   nodes and new chunks, fsync it, and add its objects to the index.
5. Write the representation record and fsync it.
6. Append the version claim, as today.

The order is the existing rule, content before the claim that names it: a crash
before step 6 leaves only unreferenced packs and records, which a later sweep
can remove, never a claim pointing at missing bytes. Before step 5 the writer
may rebuild the file from the stored chunks and check *H*, as the whitepaper
asks of anyone publishing a record; it costs one read of the file.

## Reading

- **A whole read** (`cat`, `read_version`): find a representation record for
  *H*, load the chunk list, fetch each chunk through the index, decompress it,
  check its identity, and concatenate. A whole blob is read directly, as now.
- **A ranged read** (the mount reading *n* bytes at offset *o*): find the chunk
  holding byte *o* with one walk down the list, then read chunks from there
  until *n* bytes are served. Recently read chunks are kept decompressed in a
  small cache, since programs read files in pieces. The file's size comes from
  the list's total length.
- **Integrity:** a chunk's identity comes from its uncompressed bytes, so
  checking it means decompressing and hashing it: about 500 MB/s per core,
  against about 850 MB/s for decompression alone. Instead, every compressed
  chunk carries zstd's own 4-byte checksum, checked as it decompresses; on the
  study's data it cost nothing measurable. That catches damage on every local
  read. The full identity check (SHA-256 of the decompressed bytes) is made
  where trust changes: on objects fetched from a remote or another store, and
  in `check`. Moving whole packs needs neither, because a pack is named by the
  SHA-256 of its bytes. Today transfs does not check blobs on read at all, so
  either way chunked reads are checked more than whole blobs are.

## The index and `check`

- **Size** comes from the representation's `length`, and **type** from the
  first chunk's bytes (libmagic needs only the start of a file).
- **`check`** verifies each pack (every object's identity), that every
  representation's list and chunks are present, and that every version's hash
  has a whole blob or a representation. A slower `check --deep` rebuilds each
  chunked version and compares its hash with *H*.

## Publishing and recovering

`publish` uploads blobs today; for a chunked version it uploads its
representation record and any of its packs the remote lacks. Packs are
immutable and named by their hash, so the local and remote packs are the same
files, and nothing is repacked to publish. On the remote, records go under
`reps/<H>/<rep-id>`, beside `blobs/` and `packs/`. `recover` fetches each
version's records by listing `reps/<H>/`, then the packs they name.

The whitepaper's serving rule still applies: a version that is published for
serving keeps a whole blob on the remote, so a server never has to rebuild it
from chunks.

## Dictionaries

A dictionary helped only when trained on the file's own content: a large SQLite
database gets its own, about 110 KiB, trained on its first stored version and
reused for its later versions. This proposal leaves dictionaries to a later
step. The first version compresses without them, which already captures most
of the saving (Codex's logs: 7.1% of a whole-file copy per version without a
dictionary, 4.9% with), and a dictionary can be added later without changing
any identity, because chunks are identified by their uncompressed bytes.

## Format parameters

These must be fixed and written down, because writers share chunks only if
they cut and identify them the same way:

- FastCDC in its 2020 form, with 2 KiB minimum, 8 KiB average, 32 KiB
  maximum, and a fixed normalization level; its gear table is part of the
  format and is copied into the spec, not left to a library version.
- SQLite: fixed chunks of the page size in bytes 16–17 of the header (a stored
  value of 1 means 65,536).
- Chunk identity (see below), and the chunk list's element encoding.

## What it needs from merkle-champ

Two pieces are merkle-champ's to provide; they are @march-claude's call, and
this proposal only states the need.

1. **Loading a stored `Sequence`.** merkle-champ saves a sequence's nodes but
   does not yet load them (its `FORMAT.md`, section 10.8). Reading a chunk list
   needs it. For very large files, loading only the nodes along one path
   (which `PERSISTENCE.md` already sketches) would keep a ranged read from
   loading a whole list.
2. **Compressed chunks in packs.** A merkle-champ blob's identity already
   comes from its uncompressed content, `SHA-256("merkle-champ/blob/v1" ||
   content)`, which is what chunks need: the same chunk has the same identity
   however it is stored. The gap is storage. An MCHPACK2 pack stores each
   object as exactly the bytes its identity hashes, and checks an object by
   hashing what is stored, so a pack can hold a chunk only uncompressed. Packs
   need to separate what an object is (its identity) from how it is stored
   (its bytes in the pack). Two ways to get there:
   - **Recommended: MCHPACK2 learns compressed members.** A pack member may be
     stored as a zstd frame; a reader decompresses it and checks the identity
     against the decompressed bytes, so identities stay exactly as they are.
     The header's reserved flags, or an encoding byte per index entry, could
     say which members are compressed. Pandora's SQLite snapshots need the
     same thing, so one format would serve both, and a version's chunks would
     sit in the same pack as its list nodes, right after the leaf that names
     them, which is good for sequential reads.
   - **Otherwise: transfs keeps its own chunk packs.** A small format of
     compressed chunks with an index, beside MCHPACK2 packs for the list nodes.
     It needs nothing from merkle-champ, but it is a second pack format, and
     Pandora would not share it.

**Chunk identity** follows from that choice. If chunks are merkle-champ blobs,
a chunk's identity is `SHA-256("merkle-champ/blob/v1" || chunk)`; otherwise it
could be the plain SHA-256 of the chunk. Either works. The domain-prefixed form
is what merkle-champ packs already use, and it keeps a chunk from ever sharing
an identity with a list node. The whole-file hash *H* in version claims stays
the plain SHA-256 of the file either way.

## Building it

In slices, each usable on its own:

1. **merkle-champ:** loading a stored `Sequence`, and compressed pack members
   (or the decision to keep transfs's own chunk packs).
2. **Write and read:** the chunkers, representation records, local packs and
   the object index; `add`, `addversion`, `cat` and `read_version` on chunked
   files; size and type in the index; `check`. Files under the threshold and
   already-compressed media keep going to whole blobs.
3. **The mount:** ranged reads through the chunk list, with a chunk cache.
4. **Publish and recover:** records and packs to and from a remote.
5. **Dictionaries** for large SQLite databases.
6. **Speed:** compressing chunks on several cores, and packing small packs
   together.

## Open questions

- **Compressed pack members, or transfs's own chunk packs** (above), and with
  it the form of a chunk's identity.
- **The small-file threshold.** 32 KiB is a guess; it should be where a chunk
  list and pack entries cost about what chunking saves.
- **Compress when writing, or later in the background.** Writing compressed is
  simpler and is what this proposal does; storing raw and compressing later
  would keep `add` at chunking speed.
- **Whether to check *H* by rebuilding the file on every write**, or only in
  `check --deep`.
- **Whether local reads should also make the full identity check**, at about
  half the read speed, or rely on zstd's checksum as proposed.
- **Many small packs.** One pack per version keeps writes simple, but a store
  edited often collects many small packs; packing them together needs the same
  reachability rules as garbage collection.

## Rejected alternatives

- **One file per chunk.** Over 100 million files per terabyte at 8 KiB.
- **Identifying a chunk by its compressed bytes.** The same chunk compressed at
  another level, or with a dictionary, would become a different chunk, and
  recompressing would change identities.
- **Recording the representation in the version claim.** A claim says what the
  content is; how it is stored can change (repacked, recompressed, kept whole
  for serving) without a new claim.
- **Choosing chunk size by file size.** A file growing past the threshold would
  be cut differently from its previous version, and the two would stop sharing.
- **An RRB tree or a positional vector for the chunk list.** An RRB tree's
  shape depends on its edit history, so equal lists could get different
  identities; a positional vector rewrites everything after an insertion.
  `Sequence` has neither problem.
- **One dictionary per kind of file per store.** Measured: dictionaries trained
  on other files gained nothing.
- **Compressing whole packs.** It would stop readers from fetching one object
  by range.
