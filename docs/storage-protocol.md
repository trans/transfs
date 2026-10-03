# Directory remote checkpoint format

> **Status:** first cold-recovery slice. The directory backend is implemented;
> R2 and transfs-service adapters, incremental pull into an existing working
> store, lazy pack reads, and garbage collection are not implemented.

Each device keeps its own private working store. `publish` reads its claim logs
under the local store lock, constructs a CHAMP ledger, and sends immutable
objects through the `RemoteStore` interface. `recover` unions the latest roots
from every writer and creates a new working store. The local SQLite index is
rebuilt afterward and is absent from the remote. The implemented interface is
synchronous; a browser adapter will need an asynchronous boundary.

## Remote keys

```text
blobs/<hh>/<sha256>             complete file bytes; hh is the first two hex digits
packs/<sha256>                  immutable indexed pack of CHAMP nodes
refs/<writer>/<sequence>       immutable writer ref; sequence is 20 decimal digits
```

`reps/` is reserved for alternative byte representations. Hashes are lowercase
SHA-256 hex. Writer IDs contain only ASCII letters, digits, `_`, and `-`.
The directory backend uses a synced temporary file and an atomic hard link to
publish a key only if absent. The hard-link and directory-sync behavior must
be checked on each NAS or removable filesystem before using it as a writable
remote. Readers ignore temporary files.

## Ledger and pack

The root is a merkle-champ 0.2 map:

```text
document ID -> (claim ID -> normalized format-2 JSON claim bytes)
```

The inner map retains every causal claim, including superseded names and old
content versions. Its entries can be unioned by claim ID across writers; the
ordinary document fold then derives current names, tags, and version heads.
The bytes of each CHAMP node are its identity preimage. transfs pins the
unreleased merkle-champ 0.2 commit that supplies node save/load. The pack
format lives in the separate `crates/merkle-champ-pack` layer so other projects
can reuse it without taking on transfs's document model.

Pack format `MCHPACK1`:

```text
8 bytes   ASCII magic "MCHPACK1"
4 bytes   object count, unsigned big-endian
8 bytes   index offset, unsigned big-endian
N bytes   concatenated CHAMP node bytes
48 bytes per object, sorted by identity:
           32-byte SHA-256 identity, 8-byte data offset, 8-byte length
```

The pack key hashes the complete pack. Readers check that hash, the bounds and
order of every index entry, and each node hash before passing nodes to
merkle-champ's canonical decoder. Each changed checkpoint writes a pack of
nodes absent from that writer's previous published pack set. The writer ref
retains the IDs of all packs needed to load its root.

## Writer refs and publication

Ref bytes are JSON with fields `format` (currently `1`), `writer`, `sequence`,
`previous` (the SHA-256 of the prior ref bytes, or null), `root` (CHAMP node
hash), and `packs` (ordered pack hashes). Sequences begin at 1. A new ref
extends the prior pack list. The previous-ref hash and gap checks detect
broken chains. A writer ID must have one authorized owner; if two processes
try to publish the same next sequence, exactly one conditional create wins.
The losing process must reread the ref and reconcile before publishing again.

Publication order is: capture and validate the local claim set, upload every
referenced blob, upload the new CHAMP pack, then publish the ref. A crash before
the ref may leave unreachable objects but cannot make a ref point to missing
ones. A no-change publication leaves the ref sequence unchanged.

Recovery reads the latest ref for every writer, verifies their chains and
packs, unions claims by ID, validates every document, fetches and verifies
every referenced blob, then builds a new working store in a staging directory.
It renames the completed store into the requested, absent path. The CLI rebuilds
SQLite after this step. Existing stores are never replaced by `recover`.

## Current limits

Checkpoint creation and recovery eagerly load all referenced CHAMP nodes into
memory. Ref pack lists grow with each checkpoint, and whole-file blobs are
read into memory. These are scaling limits, not format requirements: pack
indexes allow later range reads, and an immutable pack-list object can replace
large inline lists. No remote garbage collection runs yet. The directory
backend has only been verified on the local filesystem; NAS and USB semantics
still need direct tests. The working store has a local lock for appends and
checkpoint capture, but remotes provide no authentication or writer roster yet.
