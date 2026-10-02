# Proposal: transfs as the substrate for curio and DataDungeon

> **Status: proposal — not design.** This is the case for a direction and the
> shape it would take, written to be argued with. `architecture.md` remains the
> source of intent for transfs itself; nothing here changes it until a decision
> is made and the relevant parts are folded in there.
>
> Drafted 2026-10-01 from a working session across curio, DataDungeon and
> transfs. Figures are measured on the live Silicon Circus store and the dev
> machine (Core Ultra 7 155H, XFS) unless marked otherwise.
>
> Diagrams, and the corrections that drawing them produced: [`model.md`](model.md).
>
> Revised later the same day: the ledger lives on a machine and R2 keeps
> checkpoints, with one ref per writer, replacing claim-log segments in the
> bucket (§6). [The storage diagram](model.md#2-proposed-additions) shows this
> revised proposal.

---

## 1. Summary

Three projects are doing overlapping work in three different ways:

- **curio** — Silicon Circus's asset server. Rust. Serves assets by name over HTTP,
  converts formats on demand, runs the intake workflow.
- **DataDungeon** — a store for generated assets. Crystal. Find-or-generate by
  parameters, with pools, eviction and provenance.
- **transfs** — a content-addressed archive with append-only claim logs. Crystal.
  The "do it right" data model.

The proposal: **make transfs the substrate, and make curio and DataDungeon
front-ends over it.** transfs gains a storage seam (local and R2), and the other
two stop managing bytes, identity and history themselves.

The reason is not tidiness. Asset systems here have already failed twice on the
same wall — **no reliable path for content from where it is made to where it is
served** — and a shared, cloud-backed substrate is what removes that wall.

---

## 2. What the three turned out to be

| | **curio** | **DataDungeon** | **transfs** |
|---|---|---|---|
| Identity | a human-chosen **name**, which is also the URL | a **canonical tag set**, hashed | an **opaque document id**; name and tags are mutable claims on it |
| Content | name → file; SHA-256 for dedup, backup, manifest | SHA-256 for integrity and write-skip, deliberately *not* the path | pure content-addressed blobs by SHA-256 |
| Source of truth | files in `assets/` | the storage backend; DB is an index | on-disk state; DB is an index |
| History | reflink mirror + dated history | single-best overwrite; pools | version DAG in a claim log |
| Derivation | on **read** (png→webp, resize on request) | on **write** (compression policy at store) | none |
| Storage seam | none | `Storage` abstraction, shaped for R2 | none — `CAS` is hardwired to `File.*` |
| Surface | HTTP + workflow CLI | a library; serves nothing | CLI + read-only FUSE mount |

### What all three reinvented

Every one of them independently arrived at **SHA-256 over the bytes**, **"the
index is rebuildable, something else is the truth"** (all three say it in nearly
those words), **tags or name segments to find things by**, and an **integrity
check** (`curio --verify`, DataDungeon `check_consistency`/`vacuum`,
`transfs check`).

Three independent designs converging on the same core is evidence the core is
right.

---

## 3. The axis that matters: curated versus fungible

It is tempting to split these by *authored* versus *generated* content. That is
the wrong line: almost every asset in curio is AI-generated too — requested by
hand through mjanime, reviewed in intake, kept or binned.

The line that matters is **whether a human chose this specific one.**

- **Curated** — someone picked these exact bytes. Irreplaceable however they were
  made, because generation is not deterministic: re-prompting gives a *different*
  image, not that one.
- **Fungible** — any output matching the parameters will do. A game asking for "a
  pg silver human male portrait" does not care *which*.

**The durability boundary is curation, not generation.** Which turns three
projects into three stages of one pipeline:

```
  GENERATE          →  CURATE             →  ARCHIVE              →  SERVE
  candidates           review, choose        curated, versioned      HTTP, derive,
  fungible             THE BOUNDARY          irreplaceable           deploy

  DataDungeon's        curio's intake:       transfs's model:        curio
  model: params,       keep / bin            identity, names,
  generator, pools,                          tags, renames
  eviction                                   recorded
```

curio's intake workflow already *is* the curation step — mjanime drops candidates,
a person keeps or bins, keepers become served assets. DataDungeon models the stage
before it formally; mjanime does it informally.

### The provenance that gets lost

mjanime writes a `README.md` per batch describing what it generated and how. That
README is generation provenance — DataDungeon's `generator` and `generation`
fields, hand-written and unstructured. It is lossy: curio's `notes/` dispositions
had to be reconstructed by hashing bytes weeks after the fact, and one pairing
could not be recovered at all, because the link from *generated as X with these
parameters* to *curated into name N* was never recorded.

In this proposal there is no link to lose: a candidate arrives as a document
carrying its parameters in a `meta` claim, and keeping it promotes that same
document (§8; `model.md` §6.4).

---

## 4. Evidence: why DataDungeon left boardwalk

boardwalk removed its DataDungeon store in `e145ef8` (2026-09-29). The reasons
were not about DataDungeon's design:

> the store held a staler cutout of nicks than the file beside it and silently
> won, "so the better file is the one nobody sees" — and there was no way to
> deploy an asset into it, since data/ is gitignored runtime state and Cyclops
> deploys code from tags, so its one venue reached production by hand.

Two failure modes, and both have to be designed against:

1. **A second source of truth that diverged and won.** In §3's terms it was a
   category error: *curated* venue art held in a store with *fungible* semantics.
   The store treated "any cutout of nicks" as interchangeable and served its own.
2. **No deploy path for content.** curio hit the same wall the same week — 1.4 GB
   to place on a server with 3 GB free, when the one consumer uses ~14 MB.

A shared bucket that development and production both read makes "deploying
content" stop being an operation that can be forgotten, done by hand, or done
wrong.

---

## 5. The proposal

```
     DataDungeon            curio                 transfs CLI / mount
     generate, pools        curate, serve         personal archive
          \                    |                       /
           \___________________|______________________/
                               |
              transfs — the substrate
        blobs · claim logs · tags · collections · retention
                               |
              storage seam   (DataDungeon's Storage, moved DOWN)
                    local FS   |   R2   |   S3
```

DataDungeon stops talking to storage. Its `Storage` abstraction was the right idea
in the wrong layer — it belongs beneath the data model, not beside it.

### Retention is a policy, not a second system

DataDungeon has no version history and transfs keeps all of it — but keeping one
version or all of them is a **policy on a document**, not a reason for two
systems. transfs already defines GC as reachability: "unreachable from any root is
garbage." So:

- DataDungeon's **single-best** is a document whose retention keeps only the head.
- DataDungeon's **eviction** is dropping the document — a new `drop` claim
  (`model.md` §6.1) — and letting GC sweep its blobs.
- curio's curated assets keep everything.

### Consumer semantics stay out of the core

Canonical keys, served names, usage classes, compression policy — all expressed as
tags, claims and collections that transfs indexes *generically*. The moment transfs
knows what a "portrait" or a "venue" is, it has stopped being a substrate.

---

## 6. The storage seam

Two kinds of stored object, which behave very differently on an object store.

### Blobs

A natural fit. Content-addressed, written once, never modified — exactly what an
object store is. `put` / `get` / `exists` by hash; `delete` only from GC. transfs's
`CAS` needs its `File.*` calls lifted behind an interface and nothing else.

The 256-way `<hh>/` fan-out exists to keep local directories small; on an object
store keys are flat and it is unnecessary, though harmless.

### The ledger lives on a machine; R2 keeps checkpoints

*Revised later on 2026-10-01. An earlier draft put the claim logs in the bucket
as small immutable "transaction segments", because R2 objects can't be appended
to. That pushed too much into R2. The design below replaces it.*

**The live ledger runs on a machine** — a server such as curio or DataDungeon,
or your own computer. It holds the store's current state as a
[merkle-champ](https://github.com/tabcomputing/merkle-champ) map: document id →
that document's name, tags and versions (as blob hashes). Nested maps can serve
as indexes, such as tag → the set of documents with it. Writes go to it
immediately, and to a log on that machine's own disk, so a crash between
checkpoints loses nothing. transfs's claim log stays, as a local file; it does
not go to the bucket.

**R2 holds blobs and checkpoints.** Every so often the machine writes a
checkpoint: the pieces of the ledger that changed since the last one, as a
single pack file, and then its ref, pointing at the new ledger. merkle-champ
makes this cheap, because an unchanged piece is never written twice
(merkle-champ `PERSISTENCE.md` §5–6). A bucket looks like:

```
blobs/<sha256>        file contents, as now
packs/<id>            ledger pieces written by one checkpoint
refs/laptop           → the laptop's latest ledger
refs/desktop          → the desktop's
refs/curio            → the curio server's
```

**The cost is a window.** A write is durable on its own machine at once, but
reaches the cloud only at the next checkpoint. When that matters, **push now**:
checkpoint immediately instead of waiting for the next batch.

**Starting cold.** A new server reads a ref and fetches only the ledger pieces it
needs, when it needs them (`PERSISTENCE.md` §4). Nothing has to replay every
claim ever written, which is what the earlier draft's "index snapshots" were
for. A machine may still keep a local search index (substring search, the
mount's facet menu), built from the ledger and never stored in R2.

### Facts never conflict; authority is a choice

Say you and I both start from version A of `logo.png`. I make version B from it
and you make version C. The facts are:

- B was made from A.
- C was made from A.

Neither contradicts the other. Combining your facts with mine means keeping all
of them, and a store of facts that only grows always converges: two copies with
the same facts agree, whatever order they arrived in.

What needs agreement is **authority**: which version `logo.png` shows now, and
for whom. That is a decision, not a fact, and there are several ways to make it:

- **Winner takes all.** One head wins — the owner's, a designated server's, or
  the latest. The others stay in the history.
- **Merge.** A new version records both B and C as its parents. Text can really
  be merged; for an image or a PDF "merge" means choosing one, but the choice is
  recorded.
- **Keep both.** The document shows two heads until someone decides.
- **Each sees their own.** I see B, you see C, and an "official" version exists
  only if someone maintains one.

transfs already records the facts this way: every version names its parent, and
`architecture.md` §3 calls the versions "a fork-detectable DAG". What it lacks is
authority. Its head is simply the latest timestamp (§9 there), which hides a
fork instead of surfacing it. Linear versioning is just the case where nobody
forked; the history should be treated as a graph throughout.

The mechanics are straightforward. **The hard part is the user experience of
authority**: how a fork is shown, and how people settle it.

**A version must be identified by its own claim, not by its content.** Today a
version's `parent` is the hash of the content it came from. Undo exposes the
problem: go from A to B and back to A's exact bytes, and the graph says A came
from B and B came from A — a loop, not a DAG. Identified by its claim (as a git
commit is separate from the files in it), the undo is a new version that happens
to point at the same blob.

### Several writers: one ref each

**Each writer publishes only its own ref.** No two writers ever write the same
object, so nothing can be overwritten, and writers need no coordination. A
reader merges the refs it trusts: the facts combine, and an authority policy
picks each document's head. An "official" view is one more ref, kept by
whoever holds authority — the owner, or a server. Only a ref that more than one
party updates needs protecting, with a conditional write: "replace this only if
it still points where I think". Prior art: git remotes, Bluesky's one
repository per user, Secure Scuttlebutt's one feed per person.

For the projects here:

- **curio and DataDungeon:** one writer each — the server — and one ref.
- **A personal archive across machines:** one ref per machine, so it works
  offline.

### Hearing about changes early

Machines can also announce changes as they happen, before the next checkpoint.
If you change a file I have just changed, my machine hears about it at once, and
we can reconcile while we are both still at it, instead of finding the fork at
the next sync. The announcements are a convenience, not the record: the refs
are still the truth, and a missed announcement only means the fork is found
later. merkle-champ's planned subscriptions ("tell me when this map's identity
changes", with a diff; `PERSISTENCE.md` §14 and Addendum A.4) have the right
shape for this.

### R2 specifics

- **Nothing in the bucket is appended to or modified, except refs.** Blobs and
  packs are new objects, named by their contents. That removes the "R2 can't
  append" problem rather than working around it.
- **Order of writes:** the pack before the ref that points into it, and a blob
  before any version that names it. A reader that sees a ref can then always
  find everything it points to.
- **Listing is by prefix only** — no substring, no suffix. Listing refs and
  finding a blob are both prefix-shaped, so this costs nothing. Searching stays
  the index's job.
- **No egress fees**, and storage at this scale is pennies a month — which
  removes any reason to slim the store. Everything is kept.
- **To verify:** conditional writes on R2, needed only for a ref more than one
  party updates.

### The hash

**SHA-256**, as all three already use. BLAKE3 was considered and measured on the
dev machine, with the crates in use, on curio's real workload of ~1.4 MB files:

```
                        one core     one file per core
  sha256 (SHA-NI)      2.24 GB/s        31.29 GB/s
  blake3 (AVX2)        4.67 GB/s        33.66 GB/s
```

BLAKE3 is ~2× faster on one core, but these workloads hash many *separate* files,
which parallelise across cores for either hash — and there the gap is ~7%. A
"34 vs 2 GB/s" headline compares BLAKE3 on all cores to SHA-256 on one. SHA-256
keeps ubiquity (`sha256sum`, every standard library, any machine can verify a key)
and continuity with every existing store. The real speed lever is hashing in
parallel, whichever hash.

---

## 7. What DataDungeon needs, and where it lands

| DataDungeon | in the substrate |
|---|---|
| canonical-key exact lookup — 90%+ of its queries | a tag such as `dd/portrait/<key>`. DataDungeon computes it; transfs only indexes it. Single-tag lookup is already indexed |
| pools | several documents sharing that tag — tags are many-to-many |
| single-best | one document; retention keeps the head |
| eviction | retention = evictable → a `drop` claim → GC. transfs has no way to remove a document yet (`model.md` §6.1) |
| `last_used_at`, `use_count` | **soft state** (§9) — not a claim |
| `generator`, `generation` params | a structured **`meta` claim** — genuine facts about origin, not derivable from bytes, so they belong in the truth layer |
| width, height, duration | **derived in the indexer** by probing bytes, like transfs's existing `type` and `size` |
| usage class, compression policy, presentation | intent rather than derivable fact → tags or `meta` |
| 100K–millions, Postgres in production | a different index backend. The index is rebuildable, so this is a backend choice, not a migration |
| promotion — a generated asset worth keeping | promote the same document in place — name it, retain it, publish it — so its `meta` provenance stays in its own log (`model.md` §6.4). DataDungeon's `rating` and `permanent` are already the hooks |

---

## 8. What curio needs, and where it lands

| curio | in the substrate |
|---|---|
| serve `/a/<name>` | name → head blob, from the index |
| names unique, because they are URLs | a **published collection** whose entries are keyed by served name (name → document id), so uniqueness is a property of the data rather than a check curio must remember. `architecture.md` §4 leaves the entry form open, and an id-only form would not give this (`model.md` §6.2) |
| intake | transfs's **inbox**, already designed in `architecture.md` §7: "drop = archive, no prompts" |
| keep | promote in place: name it, add it to the published collection, set retention keep-all (`model.md` §6.4) |
| png → webp, resize | a **derivation cache keyed on (master content hash, params)** → rendition blob |
| the request table | soft state, shared with DataDungeon's usage |
| a consumer's declared asset list | a **collection** — `architecture.md` §4's rule already says `project=boardwalk` as a tag is "a relationship masquerading as a property" |
| deploy | **freeze the published collection** into a `tree` (served name → blob hash), stored as a new version of a deploys document; production serves it; rollback is a new version pointing at an older tree (`model.md` §6.3) |

Two of these fix defects found in curio this week:

- Keying renditions on the master's **content hash** instead of its size and mtime
  removes a known blind spot: an edit that changes no byte count within the same
  second is currently invisible to curio's sync. A content hash changes on any
  edit.
- A **frozen tree** is the deploy artifact curio never had — immutable,
  inspectable, and the thing whose absence sank DataDungeon in boardwalk.

---

## 9. What transfs has to grow

In dependency order:

1. **Storage seam** — blob access behind an interface with local and R2
   backends; a local ledger and claim log with checkpoint persistence to R2.
2. **The ledger and checkpoints** — a merkle-champ ledger on the machine, a local
   log, checkpoints of changed pieces to R2, and one ref per writer (§6).
3. **Collections** — designed in `architecture.md` §4 but not yet built (the index's `membership`
   table exists, unpopulated), and now load-bearing: the published namespace,
   consumer asset lists, generation batches, deploy snapshots.
4. **Retention policies, a `drop` claim, and GC** — keep-all, head-only,
   evictable; `drop` removes a document, which nothing can do today (`model.md`
   §6.1). GC is designed in §8 and deferred in §9; §8 mentions a retention policy
   but does not define one.
5. **`meta` claim** — structured key → value for provenance, policy and
   presentation. Tag values are opaque strings; structured data needs a home that is
   not JSON crammed into a tag.
6. **Soft-state sidecar** — usage counts and request tracking. Explicitly *not*
   truth and *not* rebuildable: losing it resets eviction order and the request
   record, which is acceptable. It must not go in the claim log — a claim per access
   would be absurd.
7. **Media probing** in the indexer.
8. **Authority** — a policy for choosing a document's head when its versions
   fork, and a way to show the fork to people (§6).
9. **Version identity** — a version identified by its own claim, not its content,
   so undo doesn't create a loop (§6).

Several of these are already designed in `architecture.md` but not built. Building
on transfs puts them on the critical path — collections most of all,
since curio serving, consumer asset lists, generation batches and deploy snapshots
all depend on them.

---

## 10. Principles to protect

**Consumer semantics stay out of the core** (§5).

**No names on blobs.** An earlier idea in this session was to store each object's
name in R2 custom metadata, so names would survive a lost manifest. That violates
`architecture.md` principle 3 — a blob is not a document — and is unnecessary
once the ledger's checkpoints live in the bucket beside the blobs. **The checkpoints are the recovery.**

**One source of truth through the transition.** The failure that removed DataDungeon
from boardwalk was a second authority that diverged and won. While curio moves onto
the substrate, curio's manifest and a transfs log must never both claim authority:
one is the truth from the first day, and the other is derived from it.

**DataDungeon's live consumers keep working.** infocomic (dormant since 2026-03-07,
still on the pre-0.3 `store_asset`/`find_asset` API) and pirateship (voice quips)
should not have to migrate for any of this to be useful.

---

## 11. The Rust move

The stack is moving to Rust — WASM above all, with Windows support and the
practical fact that models write Rust more fluently than Crystal. Readability is
the cost, and Crystal does win there.

Consequences for this proposal:

- **The cross-language problem mostly disappears.** With transfs in Rust, curio
  links it as a crate rather than reimplementing a format. "Format as contract"
  remains valuable — for other languages, for the bucket being readable without
  transfs, for merge across implementations — but it stops being a prerequisite.
- **C0 is available.** transfs has already chosen C0DATA (§3 Encoding), with one
  blocker left: the canonical-encoding contract. `c0-rs` exists (0.2.1), alongside
  implementations in C, Go, JavaScript, Python and Ruby. JSON lines until that
  blocker clears and C0 has more maturity behind it. Identity is already hashed
  from canonical *values* rather than serialised bytes, so the swap is an encoding
  change with no model change — and the move to a ledger is a natural point to
  adopt it.
- **Crystal dependencies need Rust counterparts.** `crystalfuse` (FUSE), `magic`
  (libmagic bindings) and `jargon` (CLI-as-JSON-Schema). The first two have obvious
  Rust equivalents. `jargon` is the interesting one: its schema-per-command is what
  makes "the CLI is the API" structural rather than aspirational, and that property
  is worth keeping whatever replaces it.
- **WASM forces a split.** FUSE, local filesystem access and fsync discipline
  cannot exist in WASM. The core — claims, folding, the CAS client, index logic —
  must build without them, with local storage and the mount as optional layers. The
  index is the open problem there: SQLite in a browser is possible but awkward, and
  an in-memory or IndexedDB-backed index may be the better fit for a client.

### Frontends

Front-ends are web stacks, packaged as applications through Capacitor (or Flutter
where it fits) rather than native UI per platform. A WASM build of the transfs core
runs in that same environment — a client that reads the bucket directly, rather
than talking to a server about it.

---

## 12. axiomatic — an opportunity, not a design

axiomatic's data connectors are unbuilt and open. transfs looks like an unusually
natural first one, for reasons that are structural rather than incidental:

- **Claims are already facts.** `architecture.md` §3 takes the word from Perkeep
  because it means "an assertion by a party at a time." A tag claim `stars=4` on
  document D is the fact `tag(D, stars, 4)`. The index is a fact base.
- **The log is the change stream.** Reactive UI driven by forward chaining needs to
  be told when facts change. An append-only claim log *is* that notification — new
  claims, new facts, propagate (§6, "Hearing about changes early").
- **Facet paths are conjunctive goals.** transfs's mount treats each path segment as
  an AND; that is a conjunction of goals.
- **Both target WASM.** axiom ships as WASM; a WASM transfs core would sit beside it.

This is an observation worth testing, not a commitment. It is noted here because if
it holds, it is an argument for keeping the core's fact model clean and its change
stream exposed — choices that are cheap to make early and expensive to retrofit.

---

## 13. Open questions

1. **Is transfs's model the contract?** Everything after depends on this, and it is
   the one decision that is genuinely a choice rather than an engineering call.
2. **How is authority chosen, and shown?** Winner takes all, merge, keep both, or
   each their own (§6) — per store, per document, or per person. The mechanics are
   settled; the user experience is not.
3. **Conditional writes on R2** — needed only for a ref more than one party
   updates; verify before relying on it.
4. **Where does a WASM client keep its index?**
5. **Uniqueness of served names.** A published collection keyed by served name
   gives it by construction (`model.md` §6.2); is one published namespace enough,
   or does each consumer want its own?
6. **Derivation on read, on write, or both?** curio converts on request; DataDungeon
   converts at store time; an R2 bucket fronted by a CDN favours baking at write. A
   per-asset policy may be right.
7. **Does the mjanime pipeline write candidates into the substrate directly**,
   recording parameters as structure rather than a README? That is where the lost
   provenance would stop being lost.

---

## 14. Phasing

Ordered so that each step is useful on its own and nothing waits on the end state.

1. **Decide the contract.** transfs's model, written down as what the others build
   on. No code moves.
2. **Port the transfs core to Rust, with the storage seam.** Local and R2 backends,
   the merkle-champ ledger with checkpoints and one ref per writer, WASM-buildable
   core. transfs becomes cloud-capable on its
   own — useful even if nothing else happens.
3. **Collections, retention and the `meta` claim** — the pieces the others need.
4. **curio onto the substrate.** It keeps HTTP serving, derivation and the workflow
   surfaces; its manifest becomes a view of the index; its consumers' asset sets
   become collections; deploy becomes a frozen tree. This is the step where the
   deploy problem goes away.
5. **The mjanime pipeline writes candidates with structured provenance**, and
   curio's keep becomes a recorded promotion.
6. **DataDungeon onto the substrate**, as a policy layer — last, and optional for
   its existing consumers.
