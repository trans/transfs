# Causal claim model: implementation plan

> **Status:** the first local causal-claim gate is implemented in Rust. An
> earlier format 2 build passed live FUSE checks, including content forks;
> the document-bound revision needs a new host check. Later storage/replication
> gates remain. This is the working
> plan for the first gate of the [shared storage proposal](data_centric_architecture_architecture_whitepaper.md#6-validation-plan-for-pandora-and-transfs).
> [Architecture](architecture.md) retains some historical design context;
> [the guide](guide.md) describes the current user behavior.

## Goal and boundary

Make local transfs claims safe to union after independent edits. A document
keeps one ID, but names, tags, and content versions have separate causal
frontiers. An edit made from an old content head stays as a fork. Every head and
same-field conflict remains inspectable. This gate needs no R2 bucket, CHAMP
store, pub/sub transport, or new file-byte representation.

The former Rust code folded claims in timestamp order, selected one name and
last version, and used the previous **blob hash** as a version parent. The
current implementation folds causal claim IDs, indexes all current names and
heads, and presents each name/head pair in the FUSE mount.

## Invariants

1. A claim ID is stable across serialization, replay, and replicas. Two
   independently minted, otherwise identical edits have different IDs. New
   claim IDs use merkle-champ's `Identify` value encoding with a distinct
   transfs claim domain, type tag, and normalized causal-ID sets. The v2 create
   claim's ID is the document ID; every edit claim includes and hashes that ID.
   C0DATA remains a separate possible log
   serialization; CHAMP and C0DATA are not competing structures.
2. A content version has its own claim ID, a complete-content SHA-256 hash,
   and zero or more **parent version IDs**. Its identity is independent of the
   representation of the bytes. A → B → A in byte content is three distinct,
   acyclic version nodes. A head has no child in the merged claim set; two
   children of one base are two heads. Timestamps are display metadata, not
   causal authority.
3. A name claim names all observed name frontier IDs that it supersedes.
   Concurrent names survive together; a later rename that observed both can
   supersede both. A name and a tag edit commute.
4. Tag assertions have stable IDs. A remove targets the observed assertions
   for its normalized tag; a `set key value` replaces only the observed
   assertions beneath `key`, then asserts `key/value`. Concurrent adds or sets
   remain visible. Keep the existing `=` to `/` normalization and path-prefix
   rules. Preserve simultaneous ancestor and descendant assertions when neither
   causally replaces the other. The derived facet and display projection shows
   only the deeper path because it implies the ancestor; this alone is not a
   conflict. Flag competing `set` intentions under one key.
   A no-op optimization must never erase an observed causal target needed to
   resolve a concurrent value.
5. Replaying, duplicating, and unioning valid claim sets in any order produces
   the same field frontiers and version heads. Missing causal references,
   cycles, duplicate IDs with different payloads, and malformed claims are
   errors for a complete local claim set, never reasons to select a winner
   silently. Partial replication may later distinguish an unavailable ancestor
   covered by a checkpoint from a corrupt missing reference. Derived indexes
   may be discarded and rebuilt with the same result.
6. A byte read must identify a single version. `cat` without a version
   selector errors and lists the heads when content has forked; `cat` with an
   explicit version ID reads that version. FUSE remains browsable: a content
   fork appears once per head as `name~<version-id-prefix>.ext`, and a name
   conflict appears under every current name. If both occur, list each
   name/head pair. Grow suffixes until all leaves in a directory are unique.
   `show`, `versions`, and `check` expose every head and conflicting name.
   Folded truth contains every value;
   a future read/display policy may choose a default without changing it.

## On-disk format

V2 replaces the Crystal-compatible format. There are no real stores to migrate,
so no v1 reader, importer, or cross-language compatibility layer is needed.
The first JSON-lines record is a create claim with `format: 2`; unknown formats
fail clearly. Each edit claim carries a fresh random nonce, timestamp,
operation, payload, and causal IDs. Its ID hashes validated, normalized fields
through a custom `Identify` value that uses merkle-champ FORMAT.md v1's typed,
self-delimiting encodings. The same `Identify` value is used when the claim
later becomes a CHAMP ledger value. JSON key order never affects identity.
The exact byte contract is in [claim format 2](claim-format-v2.md).
Pin golden vectors for delimiter-bearing Unicode text, shuffled reference
input, and independent identical edits. Reject invalid hash/ID syntax and
references outside the document. Replace Crystal parity fixtures with v2
fixtures, and regenerate disposable demo stores.

## Work sequence

### 1. Specify and prove the pure model

- Replace v1 claim types and decoding with v2 claims, canonical `Identify`
  encoding, parsing, validation, and stable IDs.
- Fold a **set** of claims into name, tag, and version frontiers. Give callers
  access to all versions and the parent graph. Add a pure union-by-ID helper
  that rejects an ID collision with different payloads.
- Prove the invariant cases below without SQLite or FUSE. This is the first
  merge gate; do not use timestamp or input order as a tie breaker for state.

### 2. Make local writes causal

- Have `add`, `addversion`, `rename`, `tag`, `untag`, and `set` record the
  frontier observed by the caller. A stale document snapshot must create a
  fork or concurrent field value, not silently rebase. Store bytes before
  appending the claim and preserve the log's flush/sync and torn-tail behavior.
- Give `addversion` an explicit base version ID when a document has multiple
  heads. Provide explicit resolution operations for names, tags, and content;
  resolution names every head it observed and preserves the old claims.
- Reject old-format logs with a clear error instead of interpreting their
  blob-hash parents as version IDs.

### 3. Rebuild derived views and enforce safe reads

- Version the disposable SQLite schema and rebuild it from logs on upgrade.
  Store version IDs and parent IDs as well as blob hashes, all head IDs, name
  alternatives, and conflict flags. Index tags from the causal frontier.
- Update `show`, `versions`, `list`, `find`, `cat`, and `check` so conflicts are
  visible and ambiguous byte reads require a version ID. Do not make query
  order or timestamps choose a hidden winner.
- Keep the mount read-only. Use its existing disambiguated leaf naming to
  present each content head and current name. A fork in one document must not
  make the rest of the archive unbrowsable. Reindex must reproduce the same
  conflicts and projected tag paths.

### 4. Exercise replication-shaped cases locally

- Build two independent claim sets from one base, union them in both orders,
  deduplicate replayed claims, and compare the complete derived state.
- Test concurrent renames and later resolution; rename plus tag; add/remove;
  two `set`s under one key; stale local saves; same-parent content forks; and
  A → B → A bytes. Include clock skew, duplicate delivery, missing references,
  ID collisions, cycles, torn tails, and unsupported-format logs.
- Test SQLite rebuild and CLI conflict output against the pure fold. Test FUSE
  leaves for name and content forks. Run native and `--no-default-features`
  tests, the WASM library check, and the live FUSE smoke test on a host with
  `/dev/fuse`.

## Exit criteria and follow-on work

This gate passes when every test above is green, merged replicas derive the
same state regardless of delivery order, every unresolved value remains
inspectable, and the index, CLI, and FUSE expose forks without losing access
to unrelated documents. The README and guide must state that the v1 Crystal
store format is unsupported.

After that, specify two-phase drop with an authoritative active-writer roster
as part of the writer-ref gate. A single-writer store can finalize its own
drop immediately; offline writers require the two-phase rule. R2 writer refs
and CHAMP checkpoints are later work. Measure chunk sharing on real files
read-only before deciding whether to build the proposed RRB file tree; byte
representation does not alter logical version IDs.
