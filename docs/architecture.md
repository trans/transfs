# transfs — Architecture & Design

> Status: **design**, pre-implementation. This document is the source of intent
> for transfs's storage model, claim format, index, and UX. It is the reference
> the code should conform to (and be corrected against). Last substantial
> revision: 2026-06-12 (analysis pass: trimmed `version` to {hash,parent,ts};
> composites unified as `tree`/`collection` manifests, membership-via-versioning;
> tags-vs-collections rule).
>
> Note: this hand-written design doc lives at `docs/architecture.md` and is
> tracked. If you later run `crystal docs`, send generated API HTML to a
> separate, ignored path (e.g. `docs/api/`) so it never collides with this.

---

## 1. What transfs is

A content-addressable (CAS / CID-based) file store for end users — a
**read-mostly archive** where:

- file **content** is stored once, addressed by its hash (dedup + integrity);
- **logical documents** (a thing with a name, tags, and a version history) are
  separate from the content they point at;
- you find things by **what they are** (tags, type, queries), not by where you
  filed them (there is no global directory tree);
- the store is designed to **merge across machines** eventually.

It is exposed to the OS through a FUSE mount (via the sibling project
**crystalfuse**), and driven by a CLI that is also the API a future GUI builds on.

### Non-negotiable principles (the spine)

1. **On-disk state is the source of truth; the database is a rebuildable
   index.** Lose the DB, rebuild it by replaying what's on disk. (Like git's
   object store vs. `.git/index`; Borg/restic segments vs. cache.)
2. **Truth lives in the content-addressed unit, never in the layout.** Not in a
   filename, a directory name, or a path. Anything derivable from truth is
   *derived*, not stored as a second authority. (This principle killed, in turn:
   `<ext>` in blob paths, a `versions/` directory, id-in-the-blob-path, and a
   per-claim `doc` field — all the same mistake in different costumes.)
3. **Blob ≠ document.** A blob is anonymous content. A *version claim* is the
   edge from a document to a blob. The mapping is **many-to-many** both ways.
   Identical content must **not** merge identity.
4. **Two layers, opposite disciplines.** The truth layer (logs/claims) is
   minimal and non-redundant. The index layer (SQLite) is maximally
   denormalized and redundant — which is *allowed precisely because it is
   rebuildable*.
5. **Humans never produce the machine identifier.** The opaque document id is
   for machines. People navigate by recognition (queries, computed
   descriptions), never by recalling a hash.

---

## 2. On-disk layout

```
<root>/
  blobs/<2hex>/<full-sha256>          content blobs — PURE CAS, hash only
  .transfs/docs/<2id>/<id>.log        one append-only claim log per document
  files.db                            the rebuildable SQLite index (disposable)
```

- **Blobs** are keyed by content hash alone. No extension, no name, no document
  id in the path — those would make the location depend on metadata, breaking
  dedup (same bytes → one file) and breaking the property that a CID
  *deterministically computes* its path on every machine.
- **Logs** are content-addressed too: the document id is the hash of its
  `create` claim, and the log is named by that id, fanned out by the first 2 hex
  for the same directory-bloat avoidance as the blob tree.
- **No other per-document on-disk artifact exists.** Version set, current head,
  tag set, name — all are *folds over the log*, materialized only into the
  disposable index.

---

## 3. The claim model

A **document** is its log: an append-only sequence of **claims** (timestamped
mutation records). Current state = fold the claims in timestamp order. The word
"claim" follows Perkeep (it connotes an *assertion by a party at a time*, which
is the right mental model for a mergeable, eventually-signed system).

### Encoding

The record format is behind a clean line: nothing in the model depends on it, so
long as records are append-only, one-per-record, and a torn trailing write is
detectable and skippable on replay.

- **Chosen: C0DATA** (`github:trans/c0data`) as the truth-layer encoding —
  claim logs *and* manifests. It is a strong fit: its compact form is
  **canonical by construction** (which is exactly what content-addressed
  manifest hashes and the create-claim id need — no JSON key-order/whitespace
  hazard), its record/field separators are the log's native shape, and its
  scanner is fast (the fold over logs to rebuild the index is scan-bound).
  Pending **two C0 spec additions** that are correctness-blocking for us
  (tracked in `~/Projects/c0/transfs-requirements.md`): (1) an **append /
  record-stream framing** with torn-tail detection (C0 records are
  start-delimited, so a crash-truncated final record is currently
  indistinguishable from a complete one); (2) a **canonical-encoding contract**
  strong enough to hash (minimal-DLE-escaping + defined empty/trailing-field
  rules — a true logical↔bytes bijection). A strong-but-optional third item is
  **binary field values** (SO/SI) so 32-byte hashes need not be hex-doubled.
- **Until those land**, use JSON, one object per line (newline-framed; an
  unparseable trailing line is skipped on replay). The migration to C0 is a
  format swap with no model change.
- **Identity is hashed from canonical VALUES, never from the serialized line.**
  The document id = `sha256(canonical bytes of (ts, nonce))`, not
  `sha256("{...json...}")`. This keeps identity stable across encoding changes.

### Claim catalog

| op | fields | notes |
|----|--------|-------|
| `create` | `nonce` (16 random bytes), `ts` (ns ISO-8601) | The id-less root. `sha256(ts,nonce)` **is** the document id. Content-free on purpose, so identity is independent of any content that ever flows through it. The **signer** of this claim is the document **owner**. |
| `version` | `hash`, `parent` (hash or null), `ts` | **Minimal.** `parent` = the content hash this derived from (explicit, makes versions a fork-detectable DAG; never inferred from log order, which breaks on merge interleave). `size` and `type` are **deliberately NOT stored** — both are pure functions of the blob (size = stat/length; type = sniff the bytes), i.e. *derivable from content*, which is truth. Putting a derived fact in the truth layer is the layering mistake (principle 2), and a stored copy could even *contradict* the authoritative blob. The index materializes size/type by deriving them on fold. (See §9: lazy/partial sync may reintroduce size/type at the *transfer* layer — not the at-rest claim.) |
| `name` | `name`, `ts` | A flat, blessed **label** (not a path). Current name = latest `name` claim. The first name is just the first `name` claim — never carried on `create`. |
| `tag` | `add: [..]`, `del: [..]`, `ts` | Both keys allowed; same tag in both is a no-op caught at the CLI. Tag values are **opaque strings**. Conventions like `stars=4` are parsed *at index time*, never structured in the claim. |
| *(stub)* `supersede` | … | reserved |
| *(stub)* `derived-from` | `ref: <doc-id>` | cross-document provenance; a claim on one doc *referencing* another's id (never shared ownership) |

There is **no** `add-member`/`del-member` claim. Composite membership is **content** (a manifest blob), changed by appending a new `version` — see §5. This keeps one mechanism (a document + typed content + `version` claims) for files, trees, and collections alike, rather than a parallel member-claim subsystem only collections would use.

A claim carries **no `doc` field**. Identity is established once by the `create`
line (its hash is the id); every later claim belongs to it by position in the
file. (A future packed multi-document stream would re-introduce identity at a
group header, not per line — do not design the at-rest format around that.)

### Name / type / owner — why each lives where it does

- **name** is mutable metadata (rename changes no content) → a document-level
  claim, *not* a version.
- Because names are flat labels that may carry **no extension**, the name can't
  supply the type. **Type is sniffed from the content bytes** (a true content
  fact) — but it is *derived in the index*, not stored on the version claim
  (it's a function of the blob; see the `version` row above). The mount can then
  *synthesize* a display extension (`label` + `type` → `label.pdf`) as a
  rendering detail.
- **owner** can't be derived from anything else, so it rides the **signature**:
  the owner is whoever signed `create`. Single-user now → owner is local, the
  signature slot empty. Multi-user later → owner is the verified signer; the
  deferred signing machinery is *first consumed* here. (Ownership is
  semantically "who asserted this," which is exactly what a signature is — so we
  do not invent a parallel ownership claim.)

---

## 4. Identity & the three handle layers

| layer | handle | who uses it | stability |
|-------|--------|-------------|-----------|
| durable | opaque id `d0c1f3a7…` (hash of `create`) | machine↔machine: provenance, membership, merge | forever; **never shown to humans** |
| navigable | a mount path = a rendered query | copy/paste, tab-complete, GUI clicks | stable enough; recognition-aided |
| ephemeral | a fuzzy description resolved live | the human, in the moment | resolved now, never stored |

These are the same three layers as name / id / content, one level up.

---

## 5. Composites (`tree` and `collection`)

**One mechanism, not three.** There is no separate machinery for files, trees,
and collections — there is a **document** whose **content** is one of three
kinds of blob, pointed at by ordinary `version` claims and described by the same
`name`/`tag`/owner claims:

| document content is… | it's a… | manifest entries point at |
|---|---|---|
| a plain blob | **file** | (the bytes) |
| a **blob-ref** manifest | **`tree`** (frozen snapshot) | blob hashes |
| a **doc-ref** manifest | **`collection`** (living group) | document ids |

A **manifest is just a typed blob** with its own CAS entry by its own hash (git
tree / IPFS dir / Perkeep static-set). Composites recurse for free (an entry may
point at another manifest). A document is rendered as a **file** if its head
version's type is ordinary, or as a **directory** if that type is a manifest
type — directories are not bolted on, they are documents whose content is a
manifest.

The discriminator (blob-refs vs. doc-refs) **is** the frozen-vs-living line:

- **`tree`** — entries are `(name → blob hash)`. A frozen content snapshot; the
  manifest hash *is* the version. The static-website case (`<link
  href="css/style.css">` resolves because the manifest maps that path).
  This is **where hierarchy lives**: we removed the global directory tree
  (flat labels, §3), so intrinsic content structure needs a home, and the
  `tree` is it — an opt-in, content-scoped path namespace. Names **must** be
  stored in the manifest: a blob is anonymous, names live nowhere else, and
  blob→document is many-to-many so no reverse lookup can recover a name. The
  whole tree versions as a unit; unchanged subtrees keep their hash and dedup.
  Tags can't do this job (a relative href can't resolve against a tag set), so
  `tree` is load-bearing and substitute-less.
- **`collection`** — entries point at **document ids**; members live and version
  independently. A doc-ref entry of `(id)` resolves to the member's current
  **head** (live/tracking); `(id, version-hash)` **pins** a snapshot (live vs.
  pin ≈ git branch vs. submodule) — one optional field, no extra mechanism.
  Names can come from the index (id → document → name).

**Membership is content, changed by versioning — not by claims.** Adding or
removing a member produces a **new `version`** pointing at a new manifest blob.
There is no `add-member`/`del-member` claim type. Rationale (decided 2026-06-12):

- *Symmetry.* `tree` was already "a version pointing at a manifest." Member
  claims would make `collection` a parallel subsystem doing the same job a
  different way — the inconsistency we keep removing. Manifests unify them.
- *The "free claim-union merge" we'd have gained is not actually free.* It is
  only free for commutative idempotent ops; a concurrent `add(X)` vs. `del(X)`
  still needs a tiebreak — so claims *diffuse* the merge problem across every
  claim (always, implicitly) instead of *localizing* it to one set-merge
  (rarely, explicitly). And a true concurrent edit of the same collection on two
  offline machines is a corner of a corner — never on one machine.
- *Snapshots for free.* Storing set-state as content means every past
  membership is a content-addressed, dedupable, shareable object; "the project
  as of last week" is just an old manifest hash, not a log replay-to-timestamp.

Trade-off, noted not feared: a churny collection (many adds/day) produces many
manifest versions; dedup keeps it cheap and log-compaction GC (§8) handles it.
Irrelevant at human pace.

### Tags vs. collections — the dividing rule

> A **tag** answers *"what is this document like?"* (an intrinsic property). A
> **collection** answers *"what does this group contain?"* (an extrinsic
> relationship).

`project=transfs` as a *tag* is a relationship masquerading as a property — the
arrow points the wrong way (the project contains the doc, the doc isn't
"transfs-like"), which is why it collides (the value is a name, not an identity)
and strains at multi-value (`project=[transfs,crystalfuse]` is two containers,
not one two-valued attribute). A collection fixes all three: the container owns
the membership, it's a document with a stable id (no name collision), and
many-to-many is native. Properties → tags. Relationships → collections.

`add ./dir/` walks bottom-up: each file → blob, each subdir → manifest blob,
then **one** document whose head version is the root manifest. Re-archiving an
edit produces one new version on the same document; unchanged subtrees dedup.

Open (decide at implementation): the manifest entry discriminator (`kind` field
vs. blob-`hash` vs. doc-`id` target); whether a manifest may mix entry kinds;
whether to keep a minimal unlabeled-bag form. Deferred manifest extras: file
mode / exec bit, symlink entries.

---

## 6. The index (a materialized facet view)

The SQLite index is a **fold over the logs and manifests**, rebuildable at any
time, and deliberately **denormalized** so facet queries and the
collision-neighborhood lookup (see §7) are instant.

```sql
-- one materialized facet row per document
CREATE TABLE documents (
  id            BLOB PRIMARY KEY,     -- 32-byte create-claim hash
  head_hash     BLOB,                 -- current version's content hash (NULL = no content yet)
  name          TEXT,                 -- current blessed label (may be NULL)
  type          TEXT,                 -- head version's sniffed content type
  size          INTEGER,              -- head version's size
  is_collection INTEGER NOT NULL DEFAULT 0,   -- head type is a manifest type
  owner         TEXT NOT NULL DEFAULT 'local',-- create-claim signer (local for now)
  source        TEXT,                 -- origin hint (inbox, add path, import…)
  date_added    TEXT NOT NULL,        -- create ts
  date_content  TEXT,                 -- head version ts
  version_count INTEGER NOT NULL DEFAULT 0
);

-- full version history (the content DAG)
CREATE TABLE versions (
  doc_id  BLOB NOT NULL REFERENCES documents(id),
  hash    BLOB NOT NULL,              -- content/blob hash
  parent  BLOB,                       -- parent content hash (NULL = first)
  seq     INTEGER NOT NULL,           -- fold order within the document
  ts      TEXT NOT NULL,
  size    INTEGER,
  type    TEXT,
  PRIMARY KEY (doc_id, seq)
);

-- tags, with the key=value convention split out at index time
CREATE TABLE doc_tags (
  doc_id BLOB NOT NULL REFERENCES documents(id),
  key    TEXT NOT NULL,               -- "finance", "important", "stars"
  value  TEXT,                        -- NULL for boolean tags; "4" for stars=4
  PRIMARY KEY (doc_id, key, value)
);

-- composite membership (a fold of manifest entries / member claims)
CREATE TABLE membership (
  coll_id    BLOB NOT NULL REFERENCES documents(id),
  member_ref BLOB NOT NULL,           -- a blob hash or a document id
  name       TEXT,                    -- entry name (for dir composites)
  kind       TEXT NOT NULL            -- 'file' | 'dir' | 'doc'
);

-- reverse reachability map, built during fold; powers GC and "what uses this blob"
CREATE TABLE blob_refs (
  blob_hash BLOB NOT NULL,
  referrer  BLOB NOT NULL,            -- a version's doc_id, or a manifest hash
  PRIMARY KEY (blob_hash, referrer)
);

CREATE INDEX idx_documents_name_type ON documents(name, type);  -- collision neighborhood
CREATE INDEX idx_doc_tags_key_value  ON doc_tags(key, value);
CREATE INDEX idx_versions_hash       ON versions(hash);
```

**Discipline reminder:** redundancy here is intentional and safe because every
row is reconstructable from the logs + manifests on disk.

---

## 7. UX — the part that decides whether this lives

The hard truth: CAS/tag stores die on the **daily loop**, not the feature list.
Dead systems make humans *produce* an identifier (a CID, a permanode ref, a
tag-path) — that is **recall**. Survivors (Gmail, Apple Photos, git) trade on
**recognition** and keep the machine id hidden.

### Two innovations

**1. The query is the handle.** A mount path
`/by-tag/finance/2026/report.pdf` *is* a conjunctive query; the CLI speaks the
same language: `transfs checkout finance 2026 report` narrows by query (one
match → act; several → a recognition list; zero → say so). A git-style
short-id prefix remains as a power/script escape hatch.

**2. A document's displayed name is the shortest description that
distinguishes it in context — computed, not assigned.** Like "the John in
accounting." This works on a **zero-metadata** store because every facet is
free (date, type, size, source, owner, collection, version count, plus tags if
any).

Mechanism:
- A **recognizability ranking** (v1, grounded in the `dir/name.type` + mtime set
  humans already know, plus owner):
  `type > name > collection > date > owner > source > size > version_count`.
  (Type beats date: "picture or document?" is the bigger cut.)
- **Greedily** add facets in rank order until the document is unique. Greedy,
  not optimal set-cover — cheaper and more human.
- Stability: disambiguate against the **collision neighborhood** (documents
  sharing your name/type — `SELECT … WHERE name=? AND type=?`), **not** the
  volatile search result. So the handle is the same everywhere, yet contextual.
- A **floor**: even when a document is already unique, show ≥1 most-recognizable
  facet (`Untitled PDF · added Tuesday`) so a zero-effort inbox stash is
  recognizable later.
- Rendering: **symmetric** within a candidate list (parallel facets, scannable);
  **asymmetric** for a standalone handle (its own most-salient facet). The
  ranking should eventually be tunable / learned from clicks; v1 is hardcoded.

Note how cheaply the architecture serves this: the collision neighborhood is one
indexed query, and the minimal description is a column-walk over a tiny set of
denormalized rows. The UX innovation imposes essentially **one** requirement on
the store — *materialize every facet as a queryable column, grouped cheaply by
name+type* — and bends the truth model not at all.

### Zero-effort discipline

The system must be **fully usable with no metadata** (this is why Photos won):
- **Stash** = one gesture, zero decisions. The **inbox** (a writable drop-zone)
  archives whatever is dropped, with no name/tag prompt. Naming and tagging are
  lazy, optional, suggested later. Recognition facets are free, so untagged ≠
  unfindable.
- **Retrieve** = describe fuzzily → recognize among auto-disambiguated
  candidates → act.

Judge every UX decision by the **2-second loop**: stash and retrieve must each
beat dragging-into-a-folder and hunting-a-tree.

### The mount is a read surface, honestly

The mount is a superb **read** surface and a treacherous **write** surface.
Making it a normal read-write folder fights the architecture (drag-in → which
doc/name/tags? save → a silent new version the user didn't model; two files
named `report.md` can't coexist in a folder but can in the store). So:

- **Read** (browse, open, copy out): rich synthetic views — `/by-tag/`,
  `/by-type/`, collections, `/versions/` — the same document visible under many
  without duplication.
- **Add**: one obvious safe spot, the **inbox**, where "drop = archive a new
  document" is unambiguous. Elsewhere the mount is read-only and *honest* about
  it (you discover you can't save-in-place early and clearly, not late and
  confusingly).
- **Edit**: a deliberate **checkout → edit → checkin** round-trip that makes the
  copy-on-write reality visible and intentional ("git for files"). A future
  one-shot `edit <doc> <cmd>` (checkout → run → checkin) is the seam the GUI
  leans on to hide the round-trip.

### CLI = the API

UX direction is **"both, technical first"**: build the CLI + honest read-only
mount + inbox now; a friendly GUI sits on the **same core** later. **Hard rule:
every GUI action equals a CLI/core operation — no GUI-only magic.** So the CLI
is the complete operational vocabulary of transfs.

**Chosen CLI library: Jargon** (`github:trans/jargon`) — it defines each command
as a **JSON Schema**, which makes the "CLI is the API" rule *structural* rather
than aspirational: the schema is one machine-readable contract that both the CLI
and the future GUI consume (the GUI renders forms from the same schemas it
validates against, and can even drive the core by piping JSON to `-`). It brings
subcommands-with-independent-schemas, variadic positionals (our `<q>`),
`format: path`, shell completions, and "did you mean?" for free. Division of
labor: **Jargon owns syntax** (shape, types, required, enums); **transfs owns
semantic resolution** (`<q>` → document, with the recognition list on
ambiguity — Innovation 2 — which is app logic downstream of parsing). A short
**Jargon punch-list** transfs surfaces (GUI UI-hint annotations, live
store-driven completions, an interactive disambiguation seam) is tracked in
`~/Projects/jargon/transfs-requirements.md`.

Verb catalog (the operational API):

- **in:** `add <file|dir> [--tag] [--name]`; inbox drop
- **find:** `find [--tag][--type][--name]`; `tags`; browse the mount
- **read out:** `cat <q>`; `get <q> <dest>`
- **change:** `checkout <q> [dest]` → `checkin <path>`; `status`
- **history:** `versions <q>`; `log <q>`; `revert <q> <version>` (just a new
  version pointing at old content)
- **move between stores:** `export <q> <bundle>` / `import <bundle>` (document +
  reachable blobs, deduped — the porcelain that replaces `cp`)

Here `<q>` is a query/recognition handle, never a raw id.

---

## 8. Garbage collection & reachability

A blob need **not** belong to a document. Manifest and sub-manifest blobs are
referenced by other *blobs*, not by any document's version; there are also
transient write-window blobs and orphans. The correct frame is therefore
**reachability from roots**, not ownership:

- **Documents are the only roots.** Blobs and manifests are the interior graph
  (cf. git: commits are refs; trees and blobs are interior nodes).
- **Invariant:** every blob must be reachable from ≥1 document root, *or it is
  garbage*. "Unreachable from any root" is the definition of collectable.
- **GC = mark-and-sweep from roots**, chasing manifests recursively. The reverse
  map (`blob_refs`) is built during the sweep; blobs do not carry owners. Do not
  "give every manifest an owner" by making it a document — that is the
  blob = document conflation again.

Log GC is **compaction**: replay a log, drop superseded/dead claims per the
retention policy, rewrite. Both are deferred features, but the model defines
them cleanly.

---

## 9. Deferred (decided to defer, not undecided)

- **Signing** — leave a `signer`/`sig` slot in the claim format, unused for now;
  first consumer is the `owner` facet; full crypto only when multi-user arrives.
- **Cross-machine merge mechanics** — union claim lines per document, sort by
  ts, dedup identical lines; per-document logs merge independently. Head/fork
  and tag-tie resolution default to last-writer-wins, tie-broken by claim hash.
  Collection-membership merge is a 3-way set merge of manifests against their
  common ancestor (git-tree style) — needed rarely, never on one machine.
- **Lazy/partial sync** — receiving logs before blobs. If a peer must show
  `size`/`type` before fetching content, carry them in the *transfer envelope*
  (a sync manifest), not in the at-rest `version` claim.
- **GC** (blob mark-sweep; log compaction).
- **C0DATA** record encoding — *chosen* (see §3 Encoding), pending two C0 spec
  additions (append/torn-tail framing; canonical-hashing contract). Start on JSON
  one-object-per-line; swap to C0 with no model change. Hand-off:
  `~/Projects/c0/transfs-requirements.md`.
- **btrfs/ZFS backend experiment** — put the blob store on a btrfs subvolume and
  measure whether native block dedup + checksums + snapshots let us simplify our
  own blob layer. (Dev machine is currently **XFS** — `/dev/nvme0n1p2` — which
  has reflinks but not block dedup/snapshots, so this needs a btrfs volume
  created, not the current mount.) The document/claim/merge model lives above
  any FS regardless; this is a backend optimization only.
- **Manifest extras** — file mode / exec bit, symlink entries.

## 10. Settled rejections (do not relitigate)

- `<ext>` (or any metadata) in the blob path — breaks dedup and CID→path.
- A `versions/` directory — redundant materialization of a log fold.
- id-in-the-blob-path — same as the `<ext>` mistake.
- per-claim `doc` field — `create` roots the doc; position inherits.
- `size`/`type` on the `version` claim — derivable from the blob (truth); the
  index derives them. (Only lazy/partial sync would want them, and then at the
  *transfer* layer, not at rest.)
- `add-member`/`del-member` claims — membership is content (a manifest blob)
  changed by versioning; member claims would be a parallel subsystem, and their
  "free union merge" isn't actually free.
- "a blob is a document / one version" — breaks dedup; identical content must
  not merge identity.
- an in-kernel filesystem — abandons Crystal, takes on block management we
  correctly delegate, makes bugs into panics, kills portability. Userspace model
  + FUSE over a normal backing FS is correct (as Perkeep, git-annex, IPFS do).
```
