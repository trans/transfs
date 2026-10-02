# transfs — Architecture & Design

> Status: **design**, partly implemented. This is the source of intent for
> transfs's storage model, claims, index, mount, and CLI. The
> [user guide](guide.md) describes what works today; the
> [substrate proposal](proposal-substrate.md) explores a possible next design.
>
> Note: this hand-written design doc lives at `docs/architecture.md` and is
> tracked. If you later run `crystal docs`, send generated API HTML to a
> separate, ignored path (e.g. `docs/api/`) so it never collides with this.

---

## 1. What transfs is

transfs is a read-mostly archive for files. For example, `transfs add notes.md`
archives a file as a document. After tagging it `garden`,
`transfs find tag:garden` finds it without knowing where the original file
lived. The [user guide](guide.md) shows the commands and their output.

Its content-addressed store works like this:

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
2. **Every fact is recorded once, in the store's own records — never in where or
   how a file is stored.** transfs keeps two kinds of record on disk: *blobs*
   (file contents) and *claim logs* (everything said about each document). Every
   fact lives in one of them. On disk, both are named only by a hash, so a
   filename, a directory or a path never carries meaning.

   Compare an ordinary folder tree, where the folder *is* the fact: a receipt in
   `finance/2026/` is a 2026 finance document only because of where it sits. Move
   it and the fact is gone — and it can only ever be in one folder. In transfs,
   `finance` and `2026` are tags in the document's log, and the folders you see in
   the mount are computed from them.

   A fact that can be worked out from the data is worked out, never written down a
   second time. A file's size and type can be read from its bytes, so transfs never
   stores them. A stored copy could only agree with the bytes or be wrong — and
   when it is wrong, nothing says which one to believe.

   (This rule is why blob filenames carry no extension or document id, why there
   is no `versions/` directory, and why a claim does not repeat its document's id.
   Each would have put a fact in a second place where it could drift.)
3. **A blob is not a document.**
   - A **blob** is a file's contents: just bytes, with no name, tags or history,
     stored once under the hash of those bytes.
   - A **document** is the thing you care about: it has a name, tags and a
     history. It contains no bytes itself.

   A document *points* at a blob by recording the blob's hash in a **version
   claim** — "as of now, my content is the blob with this hash." Editing the
   document stores the new bytes as a blob and adds a version claim pointing at
   it.

   So the link runs many-to-many, both ways:
   - **one document, many blobs** — its history, one blob per edit;
   - **one blob, many documents** — two documents whose version claims name the
     same hash. The bytes are stored once, and both documents point at them.

   Identical content never turns two documents into one. If two venues use the
   same texture, that is two documents pointing at one blob. Rename, retag or edit
   one and the other is untouched: the edit gives the edited document a new blob,
   and the other keeps pointing at the old one.
4. **Two layers, opposite disciplines.** The truth layer (logs/claims) is
   minimal and non-redundant. The index layer (SQLite) is maximally
   denormalized and redundant — which is *allowed precisely because it is
   rebuildable*.
5. **Humans never produce the machine identifier.** The opaque document id is
   for machines. People navigate by recognition (queries, computed
   descriptions), never by recalling a hash.

---

## 2. The store

The store keeps file bytes under their SHA-256 hashes and one claim log per
document. Adding the notes file from the guide writes a blob under
`blobs/7c/7ccada64…` and a log under `.transfs/docs/2e/2ea429b8….log`.
The index beside them can be rebuilt from those records.

### On-disk layout

```
<root>/
  blobs/<hh>/<hex-sha256>             content blobs — PURE CAS, hash only
  .transfs/docs/<hh>/<hex-id>.log     one append-only claim log per document
  .transfs/index.db                   the rebuildable SQLite index (disposable)
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

## 3. Documents and claims

A document is a named thing with tags and a history of file contents. Its log
starts with a `create` claim; adding `notes.md`, tagging it `garden`, then
adding a new version gives this sequence:

```text
create → version(7ccada64…) → name(notes.md) → tag(garden) → version(27af67e0…)
```

The version claims point to blobs in the store. The name and tag claims describe
the document itself, so changing either leaves its bytes and identity alone.

### The claim model

A **document** is its log: an append-only sequence of **claims** (timestamped
mutation records). Reading the claims in timestamp order gives its current
state. The word "claim" follows Perkeep (it connotes an *assertion by a party at a time*, which
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
| `tag` | `add: [..]`, `del: [..]`, `ts` | One claim can add some tags and remove others: `add` lists the tags to add, `del` the tags to remove, and either may be empty. Within one claim, `del` applies first and then `add`, so a tag named in both ends up present. That is how `set` (below) works: one claim clears the key and adds back the single value it keeps, even when that value was already there. In the claim a tag is just text; any structure in it, such as the path in `stars/4`, is read *at index time*, never stored as structure in the claim. |
| *(stub)* `supersede` | … | reserved |
| *(stub)* `derived-from` | `ref: <doc-id>` | cross-document provenance; a claim on one doc *referencing* another's id (never shared ownership) |

A claim carries **no `doc` field**. Identity is established once by the `create`
line (its hash is the id); every later claim belongs to it by position in the
file.

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

### Durable document identity

The document id is the hash of its `create` claim. It does not change when the
name, tags, or content changes. Programs use it for collection membership,
provenance references, and merging stores. The CLI accepts a unique prefix of
it as an escape hatch; people normally find documents by description or by
browsing the mount.

### Tags and their two verbs

How tags on one document relate is governed by *intent*, expressed as one of two
verbs rather than an automatic "deeper wins" rule. Both are CLI commands
(`transfs tag`, `transfs set`); each
writes one `tag` claim (§3). Don't confuse `tag` with `transfs add`, which
archives a file.

- **`tag key/value`** = "this is true." Accumulates, but with *subsumption* along a
  lineage — the **most-specific is kept**, order-independent: `tag date/1920/10/10`
  after `date/1920` replaces the vaguer (the year is now redundant); `tag date/1920`
  after `date/1920/10/10` is a **no-op** (1920 is already implied — nothing new
  asserted, and the month/day are *not* discarded). Different lineages coexist
  (`genre/jazz` + `genre/rock` → both: multi-valued, the way `animal/dog` doesn't
  touch `color/brown`).
- **`set key value`** = "this key IS exactly this." Replaces the whole `key/*`
  subtree with the single path given — *coarsening allowed*: `set date 1920` after
  `date/1920/10/10` deliberately discards the month and day. This is the
  single-valued / latest-wins behavior for keys like `date`.

Both verbs preserve the **leaf invariant** (after either, each lineage on the
document is a single leaf — no tag is a prefix of another *on the same doc*), which
is what keeps the prefix-walk's complete-vs-partial boundary unambiguous. Two
payoffs: **(1) no per-key cardinality metadata** — "`date` is single-valued" just
means "you always `set` it"; "`genre` is multi-valued" means "you `tag`"; the verb
carries the intent and the store stays dumb about which keys are which. **(2)
merge-safe** — `set` writes one claim whose `del` list clears the `key/*` subtree
and whose `add` list holds the new value, so two concurrent `set date` claims
resolve by timestamp (later wins) with no cardinality table; `tag` is commutative
subsumption and merges trivially.

Across documents, granularities still coexist correctly: a year-only doc and a
day-level doc *both* match the `date/1920` prefix (so "all of 1920" finds both),
and at that boundary the facet view shows both the deeper drill (`08/`, from the
day-level doc) *and* the co-facets (the year-only doc is done) — which is right.

## 4. Composites (`tree` and `collection`)

Composites are designed but not built yet. A website folder can be one `tree`
document: its manifest records names such as `index.html` and `css/style.css`
with their blob hashes. An album can be one
`collection` document whose manifest points to the documents it contains. A
new version of either document records a changed manifest.

**One mechanism, not three.** There is no separate machinery for files, trees,
and collections — there is a **document** whose **content** is one of three
kinds of blob, pointed at by ordinary `version` claims, described by the same
`name`/`tag` claims, and owned the same way (§3):

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

**Membership is content, changed by versioning — not by claims.** A collection's
members are the list in its manifest, so adding or removing a member produces a
**new `version`** pointing at a new manifest blob. Say a collection holds
documents A and B. To add C, write a new manifest `[A, B, C]` as a blob and
append a `version` claim to the collection pointing at it — exactly how a new
version of any file is saved. Each past membership is a content-addressed
snapshot, so an older manifest hash gives the collection as it was then.

A collection with many daily changes produces many manifest versions; unchanged
parts deduplicate, and eventual GC (§8) can reclaim unneeded history.

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

## 5. The index

The index answers searches and supplies the mount's facet listings. For
example, `transfs find tag:finance` returns the finance documents from the
user guide:

```text
b88976cb1af5  tax-return-2025.pdf       application/pdf   v1  owner/local,tag/finance,type/application/pdf,year/2025
a4270f088d7f  receipt-laptop.pdf        application/pdf   v1  owner/local,tag/finance,tag/warranty,type/application/pdf,year/2024
```

These rows come from claim logs and blobs. `transfs reindex` reconstructs them
if the SQLite database is lost.

The SQLite index is a **fold over the logs and manifests**, rebuildable at any
time, and deliberately **denormalized** so facet queries and the
collision-neighborhood lookup (below) are instant.

The database is at `<root>/.transfs/index.db`. Opening a missing index
rebuilds it from the logs; each change refreshes that document's rows. IDs and
hashes are hex text, making them easy to inspect with `sqlite3`. File size comes
from the blob, and libmagic sniffs its type from the bytes. `membership` exists
but stays empty until composites are built.

```sql
-- Current schema, shortened to the columns relevant to this design.
CREATE TABLE documents (
  id            TEXT PRIMARY KEY,
  head_hash     TEXT,
  name          TEXT,
  type          TEXT,      -- sniffed from the head blob
  size          INTEGER,   -- measured from the head blob
  is_collection INTEGER NOT NULL DEFAULT 0,
  owner         TEXT NOT NULL DEFAULT 'local',
  source        TEXT,
  date_added    TEXT NOT NULL,
  date_content  TEXT,
  version_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE versions (
  doc_id TEXT NOT NULL,
  hash   TEXT NOT NULL,
  parent TEXT,
  seq    INTEGER NOT NULL,
  ts     TEXT NOT NULL,
  size   INTEGER,
  type   TEXT,
  PRIMARY KEY (doc_id, seq)
);

-- Both user tags and derived facets are full paths.
CREATE TABLE doc_tags (
  doc_id TEXT NOT NULL,
  path   TEXT NOT NULL,    -- e.g. stars/4, tag/finance, type/image/jpeg
  PRIMARY KEY (doc_id, path)
);

-- Reserved for manifest entries when composites exist.
CREATE TABLE membership (
  coll_id    TEXT NOT NULL,
  member_ref TEXT NOT NULL,
  name       TEXT,
  kind       TEXT NOT NULL
);

CREATE TABLE blob_refs (
  blob_hash TEXT NOT NULL,
  referrer  TEXT NOT NULL,
  PRIMARY KEY (blob_hash, referrer)
);

CREATE INDEX idx_documents_name_type ON documents(name, type);
CREATE INDEX idx_doc_tags_path ON doc_tags(path);
CREATE INDEX idx_versions_hash ON versions(hash);
```


**Discipline reminder:** redundancy here is intentional and safe because every
row is reconstructable from the logs + manifests on disk.

### Recognition names (planned)

**A document's displayed name is the shortest description that
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

## 6. The mount

The read-only FUSE mount presents searches as folders. In the archive from the
[user guide](guide.md), `ls year/2024/=` lists `beach.png`,
`receipt-laptop.pdf`, and `sunset.png`; `ls year/2024/tag/vacation/=` narrows
that to `beach.png` and `sunset.png`.

A mount path is a navigable handle for a document, not its permanent identity.
Retagging a document can move it to another query path.

### Reading and writing

The mount is a superb **read** surface and a treacherous **write** surface.
Making it a normal read-write folder fights the architecture (drag-in → which
doc/name/tags? save → a silent new version the user didn't model; two files
named `report.md` can't coexist in a folder but can in the store). So:

- **Read (built):** browse, open, and copy out. Every path is a query (for
  example, `/tag/finance/year/2026/=`), so one document can appear in several
  views without storing extra copies. Rendering composites as directories is
  planned.
- **Add (planned)**: one obvious safe spot, the **inbox**, where "drop = archive a new
  document" is unambiguous. Elsewhere the mount is read-only and *honest* about
  it (you discover you can't save-in-place early and clearly, not late and
  confusingly).
- **Edit (planned)**: a deliberate **checkout → edit → checkin** round-trip that makes the
  copy-on-write reality visible and intentional ("git for files"). A future
  one-shot `edit <doc> <cmd>` (checkout → run → checkin) is the seam the GUI
  leans on to hide the round-trip.

### Query paths

A path through the mount is a search you can browse with normal file tools.
Unlike a document id, it can change meaning when documents are renamed or
retagged. The facets-default navigation described below is already built;
composites and computed recognition names are planned.

**Implementation note:** the facets-default model below is built
(`src/index.cr` tags-as-paths + `walk`/`docs`/`facets`, `src/query.cr` parser,
`src/fusefs.cr` resolver): `ls /` is the facet menu, hierarchical walk
(`/year/1920/`, `/type/image/`), `=` renders documents (`/=/` newest first,
`/year/1920/=/` filtered), shell-glob `*` cooperates against the listed keys.
Still deferred: composites as dir-rendered manifests, computed recognition names (the `~id`
disambiguation stands in), and literal `*` interpretation (the shell covers the
common case).

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
  composite is a *directory*, §4), newest first. `/=/` lists all documents;
  `/year/1920/=/` lists the documents for that query. A recency window with
  paging is planned so large result sets do not require one huge listing.

**Why facets-default — the POSIX-consistency win.** The thing you `cd` into must be
the thing `ls` shows, or `find` / tab-completion / file managers (all `readdir`-
based) break. With facets as the *default listing*, the **navigation vocabulary is
the listing** at every level: `cd /year/` works *because* `year/` is listed in `/`,
and `cd /year/1920/` works because `1920/` is listed in `/year/`. Fully walkable by
ordinary tools. Documents are one render away at `/=/`, while opening the root
shows how the archive can be narrowed.

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
monotonic, no thresholds. The current implementation falls back to showing
candidate facets when none splits; the strict rule would show an empty listing
at an unsplittable point. Rendering a dangling key (`/year/=/`) means "the
documents that have *any* `year` set." Inside a `collection` this whole apparatus
recurs on the membership set, for free, by the composite-boundary rule below.

**The wildcard, and the shell cooperating for free.** `*` = any key, one level, so
`/*/1920/` is the explicit loose "value 1920 under any key." Because facet view is
the *default* and lists keys as **real directory entries**, a shell's own globbing
expands `*` against exactly the right set and keeps only expansions that exist — so
`ls /*/1920/` works **unquoted**, the shell computing "any key with a 1920" for us
(and you get **tab-completion** the same way, for free). Interpreting a quoted
literal `*` is planned. `**` (any depth) is deferred.

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
- **doc view** (`=`) = S as documents, newest first; paging is planned.

This is **depth-agnostic** — `year/1920`, `type/image/jpeg`, and `date/1920/08/10`
are the same kind of object (a path) walked the same way, so arbitrary-depth
hierarchy and MIME-as-hierarchy come *free*, not as a later increment (the
`LIKE '%/pdf'` friendly-type hack disappears entirely).

**The composite boundary.** A directory in this mount is one of two things — a
**query dir** (synthetic; *is* a narrowing query) or a **composite rendered as a
dir** (a document whose head version is a manifest, §4). A composite is **just a
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

**Open path questions:**

1. **Boolean NOT** has no simple caller-side equivalent. It needs a path
   notation, but its scope is unresolved: would `/a/not=b/` mean "a AND not b,"
   and would negation bind one segment or the rest of the path? Intra-facet OR
   sugar (`type=pdf,doc`) and range selection (`size=100+`) are parked for a
   later notation pass.
2. **Version addressing** — ordinals (`v1`,`v2`) are not stable across a merge.
   The current `parent` field names a content hash, but two versions can have
   identical bytes; undo can even make that relation loop. A version needs an
   identity from its own claim before an `@version` path can be settled (see
   [the substrate proposal](proposal-substrate.md#facts-never-conflict-authority-is-a-choice)).
## 7. The CLI

The CLI is how a person adds, finds, and changes documents. For example:

```text
$ transfs tag b88976cb1af5 finance year/2025
tags: finance, year/2025
$ transfs set b88976cb1af5 year 2024
tags: finance, year/2024
```

A short id prefix works today. Planned description lookup will let a person
type a rough description such as `finance 2025 taxes`; the CLI will act on one
match or show a choice when several match. That description is used for this
command only and is never stored.

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
  ambiguity — the recognition behavior above — which is app logic downstream of parsing). Requires
**Jargon ≥ 0.18**, which blesses `x-` schema-annotation passthrough — the
mechanism transfs uses to attach **GUI render hints** to commands/fields (e.g.
"this positional is a document query → search box", "this command is
destructive → confirm", "this is the inbox add → drop-zone"), carried untouched
to GUI consumers. Two further asks transfs surfaced — live store-driven
completions and an interactive disambiguation seam — remain in progress; tracked
in `~/Projects/jargon/transfs-requirements.md`.

Planned verb catalog (the operational API; only some commands are built):

- **in:** `add <file|dir> [--tag] [--name]`; inbox drop
- **find:** `find [--tag][--type][--name]`; `tags`; browse the mount
- **read out:** `cat <q>`; `get <q> <dest>`
- **change:** `checkout <q> [dest]` → `checkin <path>`; `status`
- **history:** `versions <q>`; `log <q>`; `revert <q> <version>` (just a new
  version pointing at old content)
- **move between stores:** `export <q> <bundle>` / `import <bundle>` (document +
  reachable blobs, deduped — the porcelain that replaces `cp`)

Here `<q>` is the planned query or recognition handle. Today, commands that
act on one document take a unique id prefix.

**As built (`transfs` CLI, `src/cli.cr`).** Subcommands are Jargon schemas, one
YAML file per command under `schemas/`, embedded at compile time via `read_file`
so the binary is self-contained; each carries `x-ui` render hints for the future
GUI. Implemented so far: `add`, `addversion`, `rename`, `tag`, `untag`, `set`,
`list`, `find <tag:|type:|name:|bare>`, `cat`, `show`, `versions`, `reindex`,
`check`, and `mount`. Requires
**Jargon ≥ 0.19** (its `--` literal-positional support, added in response to a
transfs requirement, lets `key=value` and leading-dash tag values pass as
positionals: `tag <id> -- stars=4`). Tag add/remove are **separate commands**
(`tag`/`untag`) — originally because a `-tag` prefix collided with flag syntax;
kept because add-vs-remove as distinct verbs reads better than a `+`/`-`
convention regardless.

### Export and import (planned)

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

## 8. Garbage collection & reachability

Garbage collection is planned. If transfs writes a blob and crashes before it
records the version claim, no document refers to that blob. A later sweep can
remove it. A blob still used by any document or manifest must stay.

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
- **Permissions** — who may read or write a document is not worked out yet.
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
  changed by versioning. Separate member claims would make collections a
  parallel subsystem beside trees. They would not remove the merge problem:
  concurrent add and delete of one member still needs a decision. A manifest
  gives each past membership its own deduplicated snapshot.
- "a blob is a document / one version" — breaks dedup; identical content must
  not merge identity.
- an in-kernel filesystem — abandons Crystal, takes on block management we
  correctly delegate, makes bugs into panics, kills portability. Userspace model
  + FUSE over a normal backing FS is correct (as Perkeep, git-annex, IPFS do).
- An empty value slot such as `type=` to list facets — it made `=` mean two
  things and produced an unbounded list of values at the root.
- A sigil such as `@year/` or a leading dot for facet entries — both reserve
  names that archived files may need, while still mixing facets and documents
  in one listing.
- `//` as a facet marker — POSIX collapses it before FUSE sees the path.
- Documents as the mount's default listing — facet names would not be listed,
  so `cd /year/` could work while `ls /` failed to show `year/`. Facets as the
  default keep navigation visible to ordinary tools.
- Boolean OR inside a path — paths narrow one set as they descend; callers can
  take a union with shell multi-argument commands or GUI selection. OR syntax
  remains outside the mount grammar.
