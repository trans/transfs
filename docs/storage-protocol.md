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
Working stores mint a 26-character Crockford base32 ID with a 48-bit
millisecond timestamp and 80 OS-random bits. The optional human label is kept
separately in `.transfs/writer.json`. A recovered store gets a new ID.

`remote-check` probes exclusive create, hard links, rename replacement, and
directory sync on the actual remote filesystem. Publication probes once per
remote handle as well.
With hard links, the directory backend writes and syncs a temporary file, then
links it into place only if absent. With no hard links, it reserves the final
ref name with exclusive creation (`O_EXCL`), writes and syncs the intended
ref hash into the reservation, syncs a temporary file, then renames it over
that reservation. An exact retry can finish a marked reservation. Refs in
that mode have a framing header, length, and SHA-256 checksum around the JSON:

```text
reservation: 0x00 + ASCII "TRANSFS-PENDING-1\n" + 64 lowercase SHA-256 hex digits

complete ref:
15 bytes  0x00 followed by ASCII "TRANSFS-REF-1\n"
8 bytes   JSON length, unsigned little-endian
N bytes   JSON ref
32 bytes  raw SHA-256 of the JSON ref
```

The ref chain hashes the JSON bytes, independent of this filesystem envelope.
Readers ignore any incomplete or invalid tip, but reject a damaged earlier
ref. A crash before the reservation marker is durable can leave an empty or
torn reservation that cannot establish ownership; the writer owner must
inspect it before removal. Blobs and packs have no reservation: every writer
of a hash has identical bytes, so a synced temporary file can atomically
replace a damaged copy. A filesystem without exclusive create, rename
replacement, or directory sync is refused for publication. The probe
cannot prove atomic behavior across every NAS or removable device; test that
device directly before relying on it. Readers ignore temporary files.

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
format lives in the separate `merkle-champ-pack` crate in the
`merkle-champ` workspace, so other projects can reuse it without taking on
transfs's document model.

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

Logical ref bytes are JSON with fields `format` (currently `1`), `writer`, `sequence`,
`previous` (the SHA-256 of the prior ref bytes, or null), `root` (CHAMP node
hash), and `packs` (ordered pack hashes). Sequences begin at 1. A new ref
extends the prior pack list. The previous-ref hash and gap checks detect
broken chains. A writer ID belongs to one working store. Its `writer.json`
records the hash of the last ref it published. Before publishing, it saves
and syncs the exact pending ref bytes, sequence, and hash. On restart, a
matching remote tip is adopted, or an unpublished pending ref is retried.
Publish holds the local store lock through the remote publication and local
state update, and refuses to continue if the remote tip differs from both
the last ref and exact pending ref. This catches a copied working store after
either copy advances. If two
copied stores race from the same recorded tip, conditional create chooses one;
the loser must not reread and braid the two devices into one writer chain.
`fork-writer` gives the losing copy a fresh ID and keeps its local claims.
The current state tracks one remote lineage per working store.
Newly minted writer IDs are uppercase Crockford base32; legacy manually named
writer directories remain readable. Unknown OS metadata files under `refs/`
are ignored, while two writer directories differing only by case are rejected.

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
still need direct tests. Directory sync may be unsupported on some NAS shares;
the current implementation refuses publication there until tested and a safe
durability rule is established. FAT32's 4 GiB file limit prevents storing
larger blobs or packs on it. Remotes provide no authentication or writer roster yet.
