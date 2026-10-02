# Decoupled Data-Centric Architecture

> **Status: proposal, revised 2026-10-02.** Per-field causal claims and
> advisory edit announcements are agreed directions but are not implemented.
> The CHAMP-backed cloud store, RRB file tree, and Spatial N-D Chunk Tree below
> are design candidates. The
> [transfs architecture](architecture.md) describes the existing model, and the
> [substrate proposal](proposal-substrate.md) describes its proposed cloud role.

## Executive Summary

Modern software architectures frequently suffer from "application silos":
user data is trapped behind rigid application logic, isolated databases, and
vendor-specific APIs.

This whitepaper proposes a **Data-Centric, Local-First Architecture** for
Pandora and transfs. A **Merkle CHAMP (Compressed Hash-Array Mapped Trie)**
indexes immutable causal claims, content versions, and typed roots;
**Content-Addressable Storage (CAS)** holds their nodes and bytes locally and
on Cloudflare R2. Structural
sharing makes snapshots cheap. Concurrent changes to different fields can
coexist; competing changes to one field remain distinct until a field-specific
merge rule or an explicit authority decision resolves them.
The storage and sync properties must be validated before they are promises.

---

## 1. Paradigm Shift: From App-Centric to Data-Centric

### The Legacy Model
In traditional computing, applications own the schema and the storage:
$$\text{Open Specific App} \longrightarrow \text{Mutate Data in Silo} \longrightarrow \text{Export/Lock Output}$$

This pattern leads to duplicate entity tracking (e.g., contacts split across email, CRM, calendar, and chat applications) and forces users to rely on complex sync integrations.

### The Unified Data-Centric Model
Data is decoupled from tools and applications. Standardized **Data Kinds** (Entities, Events, Communications, Documents, Tensors) exist as first-class primitives:
$$\text{Universal Data Kinds} \longrightarrow \text{Dynamic Views \& Pluggable Tools (Calendar, CRM, Inbox)}$$

* **Universal Projections:** Any data kind containing a temporal timestamp property is automatically queryable and renderable by a global Calendar view.
* **Unified Communications:** Messages across protocols (Email, SMS, Forum, Chat) map to a singular `Communication` primitive, eliminating fragmented inboxes.
* **Pluggable Operators:** Third-party tools and AI agents operate directly on standardized data kinds without requiring app-specific API pipelines.

---

## 2. Storage & Memory Architecture: Merkle CHAMP over CAS

To support local-first offline usage and temporal versioning, each writer has
an immutable **Merkle CHAMP** root persisted through CAS. The ledger maps each
document ID to its set of durable claims, including content-version claims.
Current names, tags, and version heads are **frontiers derived from those
claims**. A checkpoint may cache frontiers, but it must retain the claims or
equivalent causal context needed to distinguish replacement from concurrency.
Updating a key copies the changed path and shares untouched nodes; a new root
is a snapshot of that writer's state. Branching is cheap, though nodes,
indexes, and referenced content still consume storage.

```
                          ┌───────────────────────────┐
                          │     Root Node (Hash)      │
                          └─────────────┬─────────────┘
                                        │
                       ┌────────────────┴────────────────┐
                       ▼                                 ▼
         ┌───────────────────────────┐     ┌───────────────────────────┐
         │     Branch Node (Hash)    │     │     Branch Node (Hash)    │
         └─────────────┬─────────────┘     └─────────────┬─────────────┘
                       │                                 │
        ┌──────────────┴──────────────┐           ┌──────┴──────┐
        ▼                             ▼           ▼             ▼
┌───────────────┐             ┌───────────────┐ ┌───┐         ┌───┐
│ Leaf Value A  │             │ Leaf Value B  │ │ C │         │ D │
└───────────────┘             └───────────────┘ └───┘         └───┘
```

### Key Properties

1. **Memory-to-Cloud Mirroring (Pointer Swizzling)**
   * **In RAM:** Nodes utilize raw native memory pointers (`*Node`) for sub-microsecond traversal.
   * **In CAS/Cloud (R2):** Pointers serialize into 32-byte cryptographic Content Hashes (`Node_Hash`).
   * **Lazy Paging:** Traversing a tree path dynamically fetches child nodes from local cache or cloud storage on demand (similar to OS virtual memory page faults).

2. **Immutable Structural Sharing**
   * Modifying a key-value entry does not mutate existing data. It creates a path-copy of nodes leading to a new **Root Hash**.
   * Unchanged subtrees are shared across versions. The shareable fraction depends on the workload and is not a fixed percentage.

3. **Typed Persistent Values**
   * The CHAMP indexes values by logical ID; it does not have to index every file chunk or tensor tile itself.
   * A proposed **Spatial N-D Chunk Tree** partitions dense or sparse tensors into coordinate-addressed regions. Its root is a CHAMP value.
   * A proposed **Relaxed Radix Balanced (RRB) tree** stores a file as an ordered sequence of immutable byte chunks. Nodes carry subtree byte lengths for range reads and variable-length chunks. Splitting and joining can preserve unchanged suffixes after an insertion or deletion.
   * A full **blob** is another representation for file bytes. Small files and read-mostly content versions may favor it; large files can still favor chunks for partial reads or sharing. The choice is measured, not determined solely by a read-only flag.

An RRB tree can preserve unchanged chunks when an edit arrives as a range
overwrite or splice. An external application may instead hand transfs an
entire replacement file. In that case transfs must compare it with the old
content, use content-defined chunking, or accept limited sharing; the tree
cannot infer the original edit merely from two byte streams.

### Identity, Causal Claims, and Physical Representation

A logical document keeps a stable ID. Each durable change is a small claim
identified by the hash of its canonical form. A name claim identifies the
name claims it supersedes; tag adds and removals record the causal claims
they affect, and `set` replaces
the observed claims under its key. Changes to different fields do not fork
the whole document. Two competing changes to one field remain visible together.
A content-version claim names its parent **version ID(s)**, not the parent's byte
hash, so a revert to identical bytes remains a new version. It also names the
SHA-256 hash of the **complete resulting bytes**, independent of whether those
bytes are stored as one blob, an RRB tree of chunks, or a delta. Metadata-only
claims store no new file bytes. A served URL is a key in a separately versioned
published namespace; it need not equal the document's descriptive name.

Converting a version's bytes from an RRB tree to a blob, or repacking its
chunks, must not change the version-claim ID or complete-content hash. The new
representation is written and verified before it is made discoverable; the old
one remains reachable until safe garbage collection. The durable mapping from
content hash to available representations is part of the storage design, not
just a disposable local cache. One proposed layout uses `blobs/<sha256>` for a
whole-file representation and write-once `reps/<sha256>/<rep-id>` records for
others. Each record names an RRB root or delta base and patch. A reader can
discover records by prefix; writers can add a representation without changing
any document claim or writer ref. Before publishing a record, its writer
reconstructs and hashes the complete bytes. The record carries verification
provenance; an untrusted reader may verify the reconstructed hash again.

---

## 3. Conflict Resolution & Sync Strategy

Each writer publishes its own root. A reader unions the claim sets reachable
from the roots it trusts, then derives the current frontier for each field.
Union preserves the facts; a policy still decides how to present competing
values. Merging only precomputed states, without their causal context, cannot
tell whether one value replaced another or arose concurrently.

```
                   name claim N0: report
                       /             \
       device A: N1 final             device B: N2 draft
       supersedes N0                  supersedes N0
                       \             /
                  two current names, both retained
                              |
            N3 supersedes N1 and N2 after resolution
```

Ordering N1 and N2 by wall-clock timestamp would hide a concurrent rename,
especially when clocks disagree. Causal references show that neither replaced
the other. The store can expose both names, or an authorized writer can select
an official display name while preserving the other claim. A rename and a tag
change made concurrently do not conflict. Content versions have their own
parent-version graph: two versions with the same parent are two heads. The
authority policy for showing unresolved names and content heads remains an
open product decision.

An optional pub/sub channel announces "editing X" and "changed X" so other
devices can show timely context. These are courtesy announcements, never
granted locks; missed messages do not affect correctness. A content edit
becomes a new current version only if the version it started from is still
current. Otherwise the edit is retained as a fork. The version graph enforces
this rule even when devices were offline or began editing simultaneously.

### Opaque Files and Structured Data

* An opaque file saved by an external editor produces a new content version.
  Concurrent saves produce separate heads. Byte differences alone cannot
  recover the editor's intent, so automatic semantic merging is not assumed.
* Pandora Data Kinds may define finer operations and CRDT merge rules for
  fields where those rules are meaningful. A CRDT can make replicas converge;
  it does not choose the human-preferred name when two people rename the same
  document differently.
* Small operation records can be batched into CAS objects and periodically
  snapshotted. Binary file deltas and RRB chunk sharing are separate physical
  storage techniques; a logical edit record is not automatically a byte delta.

### Checkpoints, Compaction, and Offline Peers

Three operations must remain distinct: checkpointing a writer's CHAMP root to
R2, compacting structured-data operation history, and repacking file bytes
into chunks or a full blob. A content compaction changes storage
representation, not logical claim or version identity. Structured-data
compaction may
discard old operations only if its snapshot retains the causal and tombstone
information needed when a long-offline peer returns. Garbage collection must
consider every trusted writer root and unresolved content-version head,
including the base objects needed by stored deltas.

Drop is a two-phase operation. A device declines a drop while it knows another
device is editing the document. A requested drop hides the document; it becomes
final only when every active writer's ref shows that writer saw the request.
An unseen concurrent edit cancels the request. Retiring a writer that never
returns is an authority action, not an automatic timeout. The local claim-log
retention policy after checkpointing remains open; no log should be trimmed
until its causal role is specified.

---

## 4. Cloudflare R2 Storage and Recovery

R2 holds immutable CAS objects and packs plus one moving ref per writer. A
writer uploads referenced blobs, chunks, and tree nodes before publishing the
ref that reaches them. A new machine must be able to start from a ref with no
local index or cache.

### 1. Packing Small Nodes

Fetching every small CHAMP or RRB node as a separate object can make traversal
request-heavy. Packs group nodes, and range reads fetch selected nodes. A local
SQLite index can cache `Node_Hash → (Pack_Hash, Byte_Offset)`, but it cannot be
the only copy of that directory: a cold client must discover packed nodes from
durable data reachable from the writer's ref. merkle-champ's `PERSISTENCE.md`
already proposes append-only packs and indexes, including path fetches. The
pack design and ownership should be settled with that project rather than
specified independently here; a writer ref can name a root and the packs
needed to reach it. Pack size and cache policy require measurement.

### 2. Publishing Roots

Each device normally updates only its own ref, so devices do not overwrite one
shared `head.json`. If several actors must update one official ref, its owner
needs conditional replacement or a coordinator. [R2 supports conditional
puts](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/);
a Durable Object is an option, not a prerequisite for every writer. Per-writer
refs preserve divergent states; version ancestry and an authority rule still
decide how readers present them.

### 3. Reachability

Deleting chunks, old representations, or tombstones requires a retention rule
that accounts for reachable content versions, all relevant writer refs, and
offline peers. A failed upload or a crash between writing objects and publishing a ref
may leave unreachable objects; it must never leave a published ref pointing at
missing objects.

Curio currently serves whole files from local `assets/`; it has no R2
integration. Its proposed substrate integration would serve published masters
by complete-content hash, with rendition generation from whole masters.
**Proposed serving invariant:** any version reachable from a published
namespace, or marked served, keeps a full blob at `blobs/<sha256>`. RRB and
other chunked representations may coexist for unpublished, large, or working
files. This makes the direct-serving path independent of chunk reconstruction
and leaves the storage savings to be measured on the remaining workload.

---

## 5. Potential Benefits

| Feature | Separate Application Stores | Proposed Data-Centric Merkle Architecture |
| :--- | :--- | :--- |
| **Data Ownership** | Depends on each application's export and storage policy | User-owned, content-addressed, local-first |
| **Offline Capability** | Depends on each application | Local read/write with eventual synchronization |
| **Versioning** | Often application-specific | Causal field claims and content-version ancestry, with visible conflicts |
| **Storage Efficiency** | Depends on each application | Shared nodes and content, subject to measured workload and storage overhead |
| **AI Integration** | Application-specific interfaces | Operators over authorized shared Data Kinds |

---

## 6. Validation Plan for Pandora and transfs

The [causal claim model plan](claim-model-plan.md) details the first gate for
the Rust repository, including on-disk format and read behavior at unresolved
forks.

The first implementation changes the claim model in Rust, while no real
transfs stores need migration. It needs no R2 bucket or object-store mock.
Work through these gates in order:

1. **Prove causal semantics locally.** Give every claim a canonical ID and
   field-scoped causal references; give each content version an ID and parent
   version ID(s). Merge claim sets in either order and derive field frontiers
   and content heads from the graph. Required cases: two concurrent renames
   remain visible until a later rename supersedes both; rename plus tag do
   not conflict; concurrent tag add/remove and two `set`s obey their chosen
   field rules; content A → B → A's exact bytes stays acyclic; two edits from
   one base make two heads. The SQLite index, CLI, and FUSE mount must show
   forks rather than hiding them behind log order.
2. **Measure chunk sharing before building an RRB tree.** Run a read-only
   content-defined chunking experiment over representative whole-file saves:
   the 171 named `.xcf` files in Silicon Circus (about 371 MB here), their archived history where
   available, other image/audio revisions, and a sample transfs archive.
   Pair successive versions when possible and also report sharing across the
   whole corpus. Compare full-file CAS bytes with unique chunk bytes plus
   manifest/index overhead, recording chunking CPU and plausible R2 object
   counts. Include XCF masters because they may preserve unchanged internal
   regions even when PNG/WebP re-encodes do not. This measurement changes no
   live files. [Restic's design](https://github.com/restic/restic/blob/master/doc/design.rst)
   illustrates content-defined file chunks.
3. **Build the RRB prototype only if there is a workload for it.** If the
   measurement shows useful sharing, or a Pandora editor needs explicit
   splices, prototype variable-length byte chunks with range read, overwrite,
   append, truncate, split, and join. Compare bytes and nodes rewritten,
   range-read amplification, CPU, and memory against full blobs and content-
   defined chunks. An insertion near the front must preserve the unchanged
   suffix. [RRB trees](https://hypirion.com/pdf/RMTrees.pdf) provide the
   split/concatenate basis. Keep the Spatial N-D Chunk Tree on Pandora's
   tensor path; transfs has no identified N-D file workload.
4. **Prove cold recovery after the pack layer exists.** From an empty cache,
   fetch a writer ref, locate packed nodes, reconstruct a file range, and
   verify the complete-content hash. Inject crashes before ref publication.
   Test representation conversion, published full-blob availability, stale
   writer refs, and garbage collection without losing unresolved heads.
   Specify the active-writer roster and two-phase drop here: test missed
   pub/sub announcements, an offline concurrent edit canceling a pending
   drop, all-writer observation, and authority retirement of a writer that
   never returns. A sole active writer can finalize its own drop immediately.

Correct causal state and recoverability are mandatory. The chunk measurement
decides whether RRB work is justified now or should wait for a Pandora editor.
The same CAS and writer-ref machinery can serve transfs files and Pandora Data
Kinds without forcing one value structure on every workload.

---

## Conclusion
Combining Data Kinds with immutable causal state and CAS may give Pandora
and transfs a common local-first foundation. The proposed CHAMP, RRB, Spatial
N-D Chunk Tree, and blob representations have different jobs. The validation
plan above must establish their sync semantics, recovery behavior, and actual
storage and read costs before choosing the production layout.
