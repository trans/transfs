# Causal claim model: implementation plan

> **Status:** approved direction, implementation pending. This is the working
> plan for the first gate of the [shared storage proposal](data_centric_architecture_architecture_whitepaper.md#6-validation-plan-for-pandora-and-transfs).
> [Architecture](architecture.md) and [the guide](guide.md) describe the
> current, single-writer behavior where they disagree with this plan.

## Goal and boundary

Make local transfs claims safe to union after independent edits. A document
keeps one ID, but names, tags, and content versions have separate causal
frontiers. An edit made from an old content head stays as a fork. Every head and
same-field conflict remains inspectable. This gate needs no R2 bucket, CHAMP
store, pub/sub transport, or new file-byte representation.

The current Rust code folds claims in timestamp order, selects one name and
last version, and uses the previous **blob hash** as a version parent. The
SQLite index and FUSE mount also assume one name and head. Those are all part
of this change; changing just the claim struct would leave hidden conflicts.

## Invariants

1. A claim ID is stable across serialization, replay, and replicas. Two
   independently minted, otherwise identical edits have different IDs. New
   claim IDs use an injective, versioned, domain-separated encoding with
   length-delimited fields; normalized sets of causal IDs have a defined order.
   Do not reuse the current separator/comma-based `canonical()` for new name
   or tag IDs, because user text can contain those delimiters. Keep the
   existing create-claim hash as the document ID so existing paths remain
   stable.
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
   causally replaces the other, even if both appear in the facet projection.
   A no-op optimization must never erase an observed causal target needed to
   resolve a concurrent value.
5. Replaying, duplicating, and unioning valid claim sets in any order produces
   the same field frontiers and version heads. Missing causal references,
   cycles, duplicate IDs with different payloads, and malformed claims are
   errors, never reasons to select a winner silently. Derived indexes may be
   discarded and rebuilt with the same result.
6. A byte read must identify a single version. Until a conflict-aware mount
   path is designed, `cat` without a version selector and FUSE mount startup
   fail with a clear conflict error when a reachable document has multiple
   content heads or names. `show`, `versions`, and `check` expose every head
   and conflicting name so the error is actionable. The CLI can read an
   explicit version ID even while a fork exists.

## On-disk format and existing stores

Use an explicit v2 format marker after the unchanged create record and distinct
v2 operation names in the existing per-document JSON-lines log. This lets a
reader classify the document without mistaking a v2 version parent for a v1
blob-hash parent. Each v2 edit claim carries a fresh random nonce,
timestamp, operation, payload, and its causal IDs. The canonical ID is
computed from validated, normalized fields, not JSON byte order. Keep deriving
the document ID from the create record exactly as today. Define frozen test
vectors for the v2 ID algorithm, including delimiter-bearing Unicode names/tags,
shuffled reference input, and identical edits with distinct nonces. Reject
invalid hash/ID syntax and references outside the document.

The current Crystal-compatible records are v1. Keep v1 logs readable with
their current fold semantics and keep the Crystal parity fixtures. A v2 writer
must refuse to append to a v1 document until an explicit import/migration path
exists; silently mixing v1's blob-hash parents and v2's version IDs would
invent causality. New v2 documents and migrated documents may share one store
because each document's log identifies its format. An importer should preserve
the create claim and document ID, translate records in original log order,
verify that the old current name/tags/content match, and flag ambiguous
blob-hash ancestry for review. Never rewrite a user's original log in place;
write a verified replacement and switch atomically. The current README's
two-way Crystal interoperability claim must be narrowed when v2 writing ships:
Crystal only understands v1.

## Work sequence

### 1. Specify and prove the pure model

- Add v2 claim types, canonical encoding, parsing, validation, and stable IDs.
  Keep v1 decoding isolated so old fixtures cannot accidentally use v2 rules.
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
- Keep per-document v1 reads and v2 writes separated. Add the importer before
  permitting edits to legacy documents.

### 3. Rebuild derived views and enforce safe reads

- Version the disposable SQLite schema and rebuild it from logs on upgrade.
  Store version IDs and parent IDs as well as blob hashes, all head IDs, name
  alternatives, and conflict flags. Index tags from the causal frontier.
- Update `show`, `versions`, `list`, `find`, `cat`, and `check` so conflicts are
  visible and ambiguous byte reads require a version ID. Do not make query
  order or timestamps choose a hidden winner.
- Keep the mount read-only. At startup, report affected document IDs and
  refuse a mount with unresolved name/content conflicts until a deliberate
  per-version path layout exists. Reindex must reproduce the same conflicts.

### 4. Exercise replication-shaped cases locally

- Build two independent claim sets from one base, union them in both orders,
  deduplicate replayed claims, and compare the complete derived state.
- Test concurrent renames and later resolution; rename plus tag; add/remove;
  two `set`s under one key; stale local saves; same-parent content forks; and
  A → B → A bytes. Include clock skew, duplicate delivery, missing references,
  ID collisions, cycles, torn tails, and legacy v1 fixtures.
- Test SQLite rebuild and CLI conflict output against the pure fold. Run native
  and `--no-default-features` tests, the WASM library check, and the live FUSE
  smoke test if mount behavior changes on a host with `/dev/fuse`.

### 5. Prove drop against offline writers

- Specify an active-writer roster with explicit authority for admitting and
  retiring writers. A drop request is a causal claim that hides a document
  while pending; it is final only after every active writer has acknowledged
  observing the request through a durable ref/claim. An edit concurrent
  with and unseen by the request cancels the drop. A missed pub/sub message
  cannot change the outcome.
- Implement the state machine and writer-ref simulation locally before adding
  a cloud transport. Test an offline writer returning with an edit, all-writer
  acknowledgement, and authority retirement of a writer that never returns.
  Rejoining a retired writer requires a new authorized roster entry. Defer
  garbage collection until the writer-root and retention rules are specified.

## Exit criteria and follow-on work

This gate passes when every test above is green, merged replicas derive the
same state regardless of delivery order, every unresolved value remains
inspectable, byte reads cannot silently choose a fork, and legacy logs are
still readable without modification. The drop state machine must also pass its
offline-writer cases. The rollout must document the v1/v2 interoperability
boundary and provide a verified migration path before v2 becomes the default
writer for existing stores.

After that, R2 writer refs and CHAMP checkpoints are a later gate. Measure
chunk sharing on real files read-only before deciding whether to build the
proposed RRB file tree; byte representation does not alter logical version IDs.
