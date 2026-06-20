# transfs — Architecture & Design

> Status: **design**, pre-implementation. This document is the source of intent
> for transfs's storage model, claim format, index, and UX. It is the reference
> the code should conform to (and be corrected against). Last substantial
> revision: 2026-06-16 (query-path mount design pass: settled the path grammar,
> the composite-boundary semantics, and the two transfer/packaging artifacts —
> see §7 "The query-path mount". Prior substantial revision 2026-06-12: trimmed
> `version` to {hash,parent,ts}; composites unified as `tree`/`collection`
> manifests, membership-via-versioning; tags-vs-collections rule.)
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
  blobs/<hh>/<hex-sha256>             content blobs — PURE CAS, hash only
  .transfs/docs/<hh>/<hex-id>.log     one append-only claim log per document
  files.db                            the rebuildable SQLite index (disposable)
```

**All on-disk names are the lowercase hex encoding** of the underlying 32-byte
SHA-256 (64 hex chars), and `<hh>` is the **first two hex characters** of that
same string used as a 256-way fan-out directory. Hex (not base64) so the name in
a claim *is* its path component with zero conversion — see §3. This applies
identically to blob hashes and to document ids (a document id is itself a
SHA-256 — the hash of its `create` claim — so it is hex-encoded and fanned out
exactly like a blob).

- **Blobs** are keyed by content hash alone. No extension, no name, no document
  id in the path — those would make the location depend on metadata, breaking
  dedup (same bytes → one file) and breaking the property that a CID
  *deterministically computes* its path on every machine.
- **Logs** are content-addressed too: the document id is the hex of its
  `create` claim's hash, and the log is named by that id, fanned out by the first
  two hex chars for the same directory-bloat avoidance as the blob tree.
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
  Status of the C0 spec items transfs needs (tracked in
  `~/Projects/c0/transfs-requirements.md`): (1) **append / record-stream framing**
  with torn-tail detection — **delivered** as ETB stream mode in **C0 0.9**;
  (2) a **canonical-encoding contract** strong enough to hash (minimal-DLE +
  defined empty/trailing-field rules — a true logical↔bytes bijection) — the one
  remaining blocker, a small spec tightening. Inline binary fields were
  considered and **dropped** (serious complications); transfs **hex-encodes**
  `hash`/`parent` (64 chars). Hex specifically (not base64) because hex is
  *already the spelling of the on-disk layout* — `blobs/<2hex>/<hash>` and
  `.transfs/docs/<2id>/<id>` — so the hash string in a claim **is** its path
  component with zero conversion, keeping the CAS "the hash is the address"
  property a pure string op. Cost is 2× on hash fields only. **base64 is parked**
  as a future log-size optimization *if* logs ever prove hash-heavy (url-safe
  `-_` alphabet, unpadded, since these strings appear in mount paths) — a clean
  encoding swap behind the same interface, not needed now.
- **Until those land**, use JSON, one object per line (newline-framed; an
  unparseable trailing line is skipped on replay). The migration to C0 is a
  format swap with no model change.
- **Identity is hashed from canonical VALUES, never from the serialized line.**
  The document id = `sha256(canonical bytes of (ts, nonce))`, not
  `sha256("{...json...}")`. This keeps identity stable across encoding changes.

### Durability

**The claim log is transfs's write-ahead log.** Do not build a journal in front
of it — that would be journaling the journal. A journal *is* "append-only writes
+ a commit marker + a recovery rule that discards anything past the last
commit," which is exactly what the log already is (C0's ETB stream mode supplies
the commit marker; the JSON interim uses the newline-framed last-line-skip).
ETB does **not** make writes atomic — POSIX has no atomic multi-byte append, and
no journal relies on one. It makes interrupted writes *recoverable*, and
recoverable + append-only + ordering is how every journal achieves *effective*
atomicity. Three invariants transfs must enforce on top, because no format can:

1. **fsync discipline.** Write the record + commit marker, `fsync`, and only
   *then* acknowledge or act on the claim. The marker makes a torn append
   detectable; fsync ordering decides when it can no longer be lost.
2. **Write ordering across files.** For "store a blob/manifest, then append a
   claim referencing it," write the **content first, the claim second**. A crash
   in between leaves an orphan blob — which in a content-addressed store is
   *harmless garbage the GC sweeps*, never corruption. This ordering rule is what
   **replaces any need for a cross-file transaction**: content-addressing turns a
   would-be multi-file transaction into "write leaves, then write the reference."
3. **Batch atomicity = N records, one commit marker.** When several claims must
   land together (or not at all), write them as one block closed by a single
   marker. Either the whole block replays or none of it does. The block boundary
   *is* the transaction boundary.

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

> **Implementation note (as built).** Two deliberate deviations from the DDL
> below, both justified by the index being a disposable cache rather than the
> truth layer: (1) ids/hashes are stored as **hex `TEXT`**, not `BLOB` —
> debuggable with the `sqlite3` CLI, no hex↔bytes conversion at boundaries, and
> the rest of the codebase already speaks hex; 32 bytes/row is irrelevant for a
> personal archive's metadata. (2) the index lives at
> **`<root>/.transfs/index.db`**, not `<root>/files.db` — the legacy SQL model
> still owns `files.db` during the transition. Opening a missing index rebuilds
> it from the logs; every mutating op writes its document's rows through, so the
> index stays fresh across separate CLI processes. `type`/`size` are derived on
> fold (size from the blob; type currently from the name's extension — real
> content-sniffing is a later refinement). `membership` is created but unpopulated
> until composites exist.

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

**1. The query is the handle.** A mount path `/finance/year/2026/report.pdf` *is* a
conjunctive query (each segment narrows; see "The query-path mount" below for the
grammar — `finance` a tag, `year/2026` a walk into the year key); the CLI speaks
the same language: `transfs checkout finance 2026 report` narrows by query (one
match → act; several → a recognition list; zero → say so). A git-style short-id
prefix remains as a power/script escape hatch.

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

- **Read** (browse, open, copy out): rich synthetic views — every path *is* a
  query (`/finance/2026/`), composites render as directories, and the same
  document is visible under many queries without duplication. See "The query-path
  mount" below for the grammar and the composite boundary.
- **Add**: one obvious safe spot, the **inbox**, where "drop = archive a new
  document" is unambiguous. Elsewhere the mount is read-only and *honest* about
  it (you discover you can't save-in-place early and clearly, not late and
  confusingly).
- **Edit**: a deliberate **checkout → edit → checkin** round-trip that makes the
  copy-on-write reality visible and intentional ("git for files"). A future
  one-shot `edit <doc> <cmd>` (checkout → run → checkin) is the seam the GUI
  leans on to hide the round-trip.

### The query-path mount

This is the concrete realization of Innovation 1 (the *navigable* handle layer,
§4). **Status: the navigation model below is settled (2026-06-18) — the two-view
toggle, the hierarchical tag-tree walk with value-activation, facet enumeration,
and the wildcard; version addressing and the export/transfer format remain open
(see "Open knots").** The current `/<mime-type>/<name>` mount is a stopgap this
replaces. *Implementation note:* the shipped first slice (`src/fusefs.cr`,
`src/query.cr`, `Index#match`) implements **documents-as-default flat filtering**
(bare values + `key=value`, friendly type via `LIKE`) — which this design now
**inverts** (facets are the default; documents are rendered with `=`). So the next
slice both adds the facet view + hierarchical tag-tree + value-activation *and*
flips the default; the shipped flat doc-default behavior is superseded.

**The path *is* the query, and tags are a hierarchy.** Each `/`-separated segment
narrows the set; descending = **AND**; segments **commute**. Tags form a *tree*: a
`key=value` tag is a parent→child edge (`year` → `1920`), a boolean tag is a
top-level leaf (`vacation`), and natural hierarchies nest freely —
`date/1920/08/10`, and **MIME types fall out for free** (`type/image/jpeg`, with
`/type/image/` = all images, retiring the `LIKE` hack). Navigation is a **walk
down this tree.**

| segment | meaning | example |
|---|---|---|
| `key` (a top-level node) | a boolean tag, **or** a key whose values then *activate* | `vacation` `year` `type` |
| `key/value` (≡ `key=value`) | walk a key and pick a child → **exactly** `key=value` | `year/1920` `date/1920/08/10` |
| lone `=` | the **view toggle** (documents ↔ facets) | `/=/` |
| `*` | wildcard — **any key**, one level | `/*/1920/` |

- **Value-activation makes pairing exact.** Entering `year` activates its children
  as the valid next steps, so `/year/1920/` reads as `year`=`1920` *by position* —
  not the looser "1920 somewhere." No footgun.
- **`=` is a cosmetic alias for `/`.** `year=1920` ≡ `year/1920` (and `=` is the
  tag-*creation* spelling on the CLI); a **lone** `=` can't be a separator, which
  is exactly why it's free to be the **view toggle**. So: lone `=` = toggle,
  `key=value` = a walk, `key=` (empty value) = **illegal** — `=` never means two
  things in one position (the dual-hat, finally dead). Because both `/` and `=`
  are separators, a tag key/value contains *neither* literally — a small,
  consistent reservation, like `/` in a filename.
- **No bare top-level values.** You reach a value through its key (`/year/1920/`),
  never bare (`/1920/`) — safer and unambiguous (a bare top segment is always a
  *top-level node*, never a free value). The explicit "value under any key" is the
  wildcard `/*/1920/`.
- **Direct access** is the same walk on identity facets: `doc/<id-or-prefix>` /
  `blob/<hash>`. A discriminator is needed because a document id and a blob hash
  are *both* 64-hex SHA-256.

**The two views — facets are the default; documents are *rendered*.** A path
resolves to facets *or* to documents, chosen by the lone `=` toggle, and **the
default is facets** — this is what makes the mount POSIX-consistent (see below):

- **facet view** (default; even count of lone `=`, including zero): the **facets
  you could narrow by**, as *directories* you walk. The root `/` is the **facet
  menu** — `ls /` lists the **keys** present, small and bounded
  (`type/ year/ stars/ tag/`), never a wall of values; boolean tags collapse under
  a synthetic **`tag/`** bucket so the menu stays tiny however many bare tags
  exist. Drilling a key shows its activated values (`ls /year/` → `1920/ 2020/`).
- **doc view** (rendered; odd count): the matching **documents**, as *files* (a
  composite is a *directory*, §5), **recency-windowed** — the N most-recent
  matches, streamed/paged, never the whole set. `/=/` is the window over
  everything: your **recents**. `/year/1920/=/` is the documents for that query.

**Why facets-default — the POSIX-consistency win.** The thing you `cd` into must be
the thing `ls` shows, or `find` / tab-completion / file managers (all `readdir`-
based) break. With facets as the *default listing*, the **navigation vocabulary is
the listing** at every level: `cd /year/` works *because* `year/` is listed in `/`,
and `cd /year/1920/` works because `1920/` is listed in `/year/`. Fully walkable by
ordinary tools. (Documents-as-default would invert this — you'd type tag terms that
*aren't* listed, a search box in a path costume, navigable-but-unlisted like
autofs.) The trade is the *only* thing given up: documents aren't at the root —
your recents are one render away at `/=/`. That is an ergonomic preference, not a
structural cost, and for a tag archive "open it → see how it's organized" is
arguably the right default anyway.

The toggle is **sticky** — drill freely between toggles, and return to narrowing
with `cd ..`, never by stacking markers, so there is **no oscillation**. Crucially
it **separates the two populations by view**: documents and facets are *never
co-listed*. That is what dissolves the name-collision problem wholesale — a
document named `year` and the facet `year` live in different views, so there is no
sigil, no dynamic disambiguation, no fence character beyond the lone `=` itself.
Net of facets-default + view-separation: **typed query-paths, collision-free, and
fully-listed navigation all hold at once** — the inversion buys all three.

**Appearance rule:** a key or value is offered **iff selecting it would actually
narrow the current set** (some-but-not-all of the current documents match) —
monotonic, no thresholds. An unsplittable point (one document, nothing to divide)
shows an empty facet listing. Rendering a dangling key (`/year/=/`) means "the
documents that have *any* `year` set." Inside a `collection` this whole apparatus
recurs on the membership set, for free, by the composite-boundary rule below.

**The wildcard, and the shell cooperating for free.** `*` = any key, one level, so
`/*/1920/` is the explicit loose "value 1920 under any key." Because facet view is
the *default* and lists keys as **real directory entries**, a shell's own globbing
expands `*` against exactly the right set and keeps only expansions that exist — so
`ls /*/1920/` works **unquoted**, the shell computing "any key with a 1920" for us
(and you get **tab-completion** the same way, for free). We also interpret a
literal `*` ourselves (for quoted / `nullglob` shells). `**` (any depth) is
deferred.

**Why this shape** (alternatives, rejected): an *empty-slot* enumerate marker
(`type=` lists values) brought back the dual-hat and a flat value-list that
returned *more entries than there were documents* — useless at the root. A
per-entry **sigil** (`@year/`) co-lists the populations, costing a reserved leading
character and still needing collision handling. A **`.`-prefix** collides with
archived dotfiles (a file store can't claim that namespace). **`//`** is collapsed
by POSIX before FUSE ever sees the empty component. The **toggle-plus-walk** model
beats them all: views separate the populations (no co-listing → no collision, no
sigil), a lone `=` is purely the toggle (no dual-hat), the hierarchy keeps the
facet menu small and the value-pairing exact, and the OS does the globbing and
completion. The mount is deliberately scoped to **browse, not full query**:
boolean OR/NOT and heavy range/predicate work live in the CLI and GUI (as Perkeep
keeps its query in a search box and its mount as structured views) — see "Open
knots".

**Implementation spine — tags as paths, navigation as prefix-walk.** The walk,
value-activation, facet enumeration, *and* arbitrary-depth hierarchy collapse into
one mechanism: store each tag as its full hierarchical **path string** (`stars/4`,
`vacation`, `type/image/jpeg`, `date/1920/08/10` — the `=` alias normalizes
`stars=4` → `stars/4` at creation), and the mount is **prefix navigation over the
set of tag-paths**. The index representation simplifies accordingly:
`doc_tags(doc_id, key, value)` → `doc_tags(doc_id, path)`.

Resolving a mount path is a **fold** over its segments carrying two pieces of state
— the accumulated document set **S** and a partial prefix **P** (reset at tag
boundaries):

```
for each segment X:
  P' = P + "/" + X
  S  = S ∩ { docs with a tag-path having prefix P' }   # path = P' OR path LIKE P'||'/%'  (index-friendly)
  P  = (P' is itself a stored tag) ? "" : P'           # complete tag → reset (pick a co-tag next)
```

The two views are each **one query** over `(S, P)`:
- **facet view** (enumerate) = the distinct component at depth `|P|` among S's
  tag-paths with prefix P. P empty (you just completed a tag) → the first
  components = the **co-facets**; P partial (`year`) → the next level = the **drill
  values**. Same query. The **appearance rule** is a filter on it (a component
  shows iff it *splits* S — which also auto-hides the tag you already applied:
  everything in S has it, so it can't split).
- **doc view** (`=`) = S as documents, recency-windowed.

This is **depth-agnostic** — `year/1920`, `type/image/jpeg`, and `date/1920/08/10`
are the same kind of object (a path) walked the same way, so arbitrary-depth
hierarchy and MIME-as-hierarchy come *free*, not as a later increment (the
`LIKE '%/pdf'` friendly-type hack disappears entirely).

**Tag verbs — `add` and `set` (and the leaf invariant).** How tags on one document
relate is governed by *intent*, expressed as one of two verbs — not by an automatic
"deeper wins" rule:

- **`add key/value`** = "this is true." Accumulates, but with *subsumption* along a
  lineage — the **most-specific is kept**, order-independent: `add date/1920/10/10`
  after `date/1920` replaces the vaguer (the year is now redundant); `add date/1920`
  after `date/1920/10/10` is a **no-op** (1920 is already implied — nothing new
  asserted, and the month/day are *not* discarded). Different lineages coexist
  (`genre/jazz` + `genre/rock` → both: multi-valued, the way `animal/dog` doesn't
  touch `color/brown`).
- **`set key value`** = "this key IS exactly this." Replaces the whole `key/*`
  subtree with the single path given — *coarsening allowed*: `set date 1920` after
  `date/1920/10/10` deliberately discards the month and day. This is the
  single-valued / latest-wins behavior for keys like `date` or `owner`.

Both verbs preserve the **leaf invariant** (after either, each lineage on the
document is a single leaf — no tag is a prefix of another *on the same doc*), which
is what keeps the prefix-walk's complete-vs-partial boundary unambiguous. Two
payoffs: **(1) no per-key cardinality metadata** — "`date` is single-valued" just
means "you always `set` it"; "`genre` is multi-valued" means "you `add`"; the verb
carries the intent and the store stays dumb about which keys are which. **(2)
merge-safe** — model `set` as a claim that clears the `key/*` subtree then adds
(≈ `del key/*` + `add`, atomic), so two concurrent `set date` claims resolve by
timestamp (later wins) with no cardinality table; `add` is commutative subsumption
and merges trivially.

Across documents, granularities still coexist correctly: a year-only doc and a
day-level doc *both* match the `date/1920` prefix (so "all of 1920" finds both),
and at that boundary the facet view shows both the deeper drill (`08/`, from the
day-level doc) *and* the co-facets (the year-only doc is done) — which is right.

What this leaves for the build is the `getattr`/`readdir`/`lookup`/render branching
in `fusefs.cr` (genuinely separate plumbing), plus the prefix query, the
next-component enumeration (substr/instr over the indexed `path`), and the small
fold above. The earlier "stateful key/value machine" and "deferred deep hierarchy"
both dissolve into this.

**The composite boundary.** A directory in this mount is one of two things — a
**query dir** (synthetic; *is* a narrowing query) or a **composite rendered as a
dir** (a document whose head version is a manifest, §5). A composite is **just a
document**: it surfaces by its computed recognition name alongside plain docs
(`Italy Trip/` beside `passport.pdf`), with the same disambiguation on name
collisions — *no* special "directory name", *no* path assigned to it. The only
divergence is at the leaf: `getattr` reports a directory and `readdir` lists its
members, instead of `read` returning bytes.

Descending into a composite crosses a mode boundary (above it a segment is a query
facet; at-and-below it, the document's own internal structure). Whether that's
*perceptible* splits cleanly by kind, and the split tracks a real difference in
the thing:

- **`collection` → imperceptible.** Members are documents with full facets in the
  index, so "the query layer scoped to these members" is *literal*
  (`… WHERE doc_id IN members(C) AND type=pdf`). Narrowing, recognition names,
  globs, commutativity — all keep working. You change *universe*, not gestures.
- **raw-import `tree` → an honest, legible hiccup.** A tree built by importing a
  directory/website is `name → blob`; its entries *were never documents* and have
  no facets. So query-narrowing stops at that boundary — but there is nothing to
  lose, and the lost affordance (filter a website's file tree by tag) is one you'd
  never reach for. The hiccup coincides with the thing genuinely being a sealed
  artifact, so it reads as correct rather than broken. Browse it by its stored
  names.
- **a snapshot of a `collection` keeps its facets — for free.** Freezing a
  collection yields a **pinned collection** (entries still point at doc-ids, each
  pinned to a version-hash), *not* a blob-tree — so document linkage survives.
  Its facets *as of the freeze instant* are obtained by **folding each member's
  log up to the freeze timestamp** (the log is already a time machine): queryable
  as-it-was, with **nothing redundant stored**. The worry "a frozen tree loses
  its facets" therefore does not arise — the only facetless trees are raw-import
  trees, where facetlessness is the truth.

**Transfer / packaging** (banked as its own deferred slice; layout will want the
C0 manifest encoding, §3). Two export artifacts at different fidelity points,
chosen by intent:

- **log-bundle** — ship the **logs + reachable blobs**; the receiver dedups on
  arrival (CID match → skip) and reindexes. Documents arrive **whole**: identity,
  version history, tags-as-claims, mergeability. Facets ride along because the
  *logs* do (nothing copied — they fold on the far side). This is the
  *stays-in-the-ecosystem* transfer ("git push/pull"): the `export`/`import`
  porcelain of the verb catalog.
- **severed blob-tree** — ship **blobs + manifest + a copied facet snapshot**:
  frozen, anonymous, self-contained, transfs-*optional*. The receiver needs
  neither the document graph nor transfs to use it. This is the easy
  *package-files-for-transfer* / archive-offline artifact; the cost is the
  severance (no identity, history, or merge).

This sharpens the spine into a stateable rule: **copy facets ⟺ severed from the
graph.** Inside the live graph, *fold, never copy* (including fold-to-timestamp
for snapshots). The one sanctioned place to store derived facet state is the
moment an artifact is cut loose — because that is precisely the moment it stops
being derivable.

**Open knots (this design pass is not finished):**

1. **Boolean NOT** — descend=AND is free. **OR is decided *out* of the grammar:**
   a path is one place built by *monotonic narrowing*, and OR broadens — so union
   is the *caller's* job (shell multi-arg `ls A B`, GUI multi-select), which is the
   universal filesystem idiom and matches faceted search (multi-select within a
   facet = OR, across facets = AND — exactly what the two modes already produce).
   Intra-facet OR sugar (`type=pdf,doc`) is *parked together with range selection*
   (`size=100+`) for a future notation revisit — same shape (one facet, multiple/
   extended values). **NOT is deferred but has the stronger case** — it has no free
   caller-side equivalent (no `ls everything-except`), so it genuinely needs
   in-grammar support; the open question is *scoping* (does `/a/not=b/` mean
   "a AND not b"? does negation bind the segment or the rest of the path?).
2. **Version addressing** — a version's durable handle is its content hash;
   ordinals (`v1`,`v2`) are *not* merge-stable (fold-order assigned). Lean: a
   `@version` suffix (`/doc=abc@<hash>`, `@head`, `@head~1`) to keep doc=file
   rather than doc-as-dir-of-versions.
3. **Export / transfer format** — the layout of the two artifacts above
   (log-bundle, severed blob-tree); wants the C0 manifest encoding (§3), so it
   waits on the canonical-hashing contract.

*(Settled across this design: the hierarchical tag-tree walk with value-activation,
the two-view `=` toggle, facet enumeration by key (with the `tag/` bucket and the
narrow-iff-it-splits appearance rule), the wildcard and mode-based glob behavior,
recents/recency-windowing at the root, the composite boundary, and the two transfer
artifacts. **Listing scale** is handled — recency-windowed doc views + bounded
facet keys + the appearance rule. **OR** is out of the grammar (union = the
caller's job; intra-facet OR sugar parked with range selection for a notation
revisit). The mount is scoped to browse, not full query.)*

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
ambiguity — Innovation 2 — which is app logic downstream of parsing). Requires
**Jargon ≥ 0.18**, which blesses `x-` schema-annotation passthrough — the
mechanism transfs uses to attach **GUI render hints** to commands/fields (e.g.
"this positional is a document query → search box", "this command is
destructive → confirm", "this is the inbox add → drop-zone"), carried untouched
to GUI consumers. Two further asks transfs surfaced — live store-driven
completions and an interactive disambiguation seam — remain in progress; tracked
in `~/Projects/jargon/transfs-requirements.md`.

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

**As built (interim `transfs2` CLI).** Subcommands are Jargon schemas, one YAML
file per command under `schemas/`, embedded at compile time via `read_file` so
the binary is self-contained; each carries `x-ui` render hints for the future
GUI. Implemented so far: `add`, `addversion`, `rename`, `tag`/`untag`, `list`,
`find <tag:|type:|name:|bare>`, `cat`, `show`, `versions`, `reindex`. Requires
**Jargon ≥ 0.19** (its `--` literal-positional support, added in response to a
transfs requirement, lets `key=value` and leading-dash tag values pass as
positionals: `tag <id> -- stars=4`). Tag add/remove are **separate commands**
(`tag`/`untag`) — originally because a `-tag` prefix collided with flag syntax;
kept because add-vs-remove as distinct verbs reads better than a `+`/`-`
convention regardless.

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
- **C0DATA** record encoding — *chosen* (see §3 Encoding). ETB stream framing
  delivered in C0 0.9; one blocker remains (the canonical-hashing contract).
  Start on JSON one-object-per-line; swap to C0 with no model change. Hand-off:
  `~/Projects/c0/transfs-requirements.md`.
- **btrfs/ZFS backend experiment** — put the blob store on a btrfs subvolume and
  measure whether native block dedup + checksums + snapshots let us simplify our
  own blob layer. (Dev machine is currently **XFS** — `/dev/nvme0n1p2` — which
  has reflinks but not block dedup/snapshots, so this needs a btrfs volume
  created, not the current mount.) The document/claim/merge model lives above
  any FS regardless; this is a backend optimization only.
- **Manifest extras** — file mode / exec bit, symlink entries.
- **Index hash encoding: TEXT vs BLOB benchmark** — the index currently stores
  ids/hashes as hex `TEXT` (debuggable during build-out). BLOB (32 raw bytes)
  would halve them and shrink B-tree keys (smaller index, better page-cache
  density) — but a SHA-256 is 256-bit so it can never be a SQLite `INTEGER`;
  the only choice is TEXT vs BLOB, and conversion cost is negligible either way,
  so the real axis is key-size efficiency vs. inspectability. Decide
  *empirically*, but only once the test is valid: write a **benchmark harness**
  (NOT switchable production code — a throwaway that builds the index both ways)
  with **synthetic volume (~100K docs)** and a **realistic reader (the mount's
  getattr/find loop)**, measuring db size + lookup/query latency. Benchmarking
  now (≈3 docs, no mount) would measure noise and falsely say "marginal." If
  BLOB's win is marginal at scale, keep TEXT for debuggability; else flip
  (one-line-per-column + `reindex`, zero migration — the index is a rebuildable
  cache and the rebuild-identical path is already spec'd).

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
