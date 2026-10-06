# Chunk study

> **Status:** 2026-10-06. This is gate 2 of the
> [whitepaper's validation plan](data_centric_architecture_architecture_whitepaper.md#6-validation-plan-for-pandora-and-transfs).
> Measured on GIMP files, generated and real SQLite databases, and PNG and
> WebP exports, with and without compression; audio and video are
> [still to measure](#still-to-measure).

transfs stores every version of a file as one complete blob. When two versions
share most of their bytes, both copies are stored in full. Storing files as
**chunks** instead (pieces of a file, each stored once under its own hash)
keeps a shared piece once, so a new version costs only its changed chunks plus
a list of the chunks it is made of.

For example, a 3.4 MB GIMP file gets a small brush stroke. Stored whole, the new
version costs another 3.4 MB. Cut into content-defined chunks averaging 4 KiB,
it costs about 90 KB: the few chunks the stroke changed, plus the new version's
chunk list.

This study measures how much chunking saves on real file types, and which way
of cutting chunks works for each.

## The short answer

- **GIMP files (XCF): a large saving, with content-defined chunks.** A typical
  edit costs 1–10% of a whole-file copy. Different images share 29% of their
  bytes. Fixed-size chunks get almost none of this.
- **SQLite databases: a large saving, with fixed chunks the size of a database
  page.** On a generated database, ordinary updates cost 9% of a whole-file
  copy and a bulk insert 15%. On real ones, a session of an append-heavy
  database adds under 0.5% of the file in new chunks, and a day of a log
  database that reuses its pages 19%. Content-defined chunks cost up to 2.5 times as much.
- **PNG exports keep the part of the file before the first change; WebP exports
  share nothing.**
- **Compressing each chunk with zstd, plus a dictionary trained on the
  database's own content, roughly halves what SQLite versions cost again**
  (generated database 4.5%, Codex's logs 4.9%), and trims GIMP versions a
  little.
- **An edit that changes every pixel saves nothing**, and chunking adds about
  1.5% on top of a whole-file copy.

## How it was measured

`tools/chunk-study` reads files and prints a table with one row per way of
cutting chunks. It only reads its input. It has two modes:

- `corpus LABEL FILE...` stores every file in one place and reports what the
  whole set costs. This measures sharing between different files.
- `pairs LABEL OLD NEW ...` stores OLD, then reports what NEW adds. This
  measures what a new version costs. Rows are summed over all pairs.
- `zpairs LABEL OLD NEW ...` is `pairs` with every stored chunk compressed by
  zstd ([compression](#compression)). `ZSTD_LEVEL` and `ZSTD_DICT_KIB` set
  the level (default 3) and dictionary size (default 110).

It compares ten ways of storing a file:

- **whole file**: one blob per distinct file, as transfs stores files today.
  This is the baseline every percentage is measured against.
- **fixed 1, 4, 16 or 64 KiB**: the file cut every N bytes.
- **FastCDC, average 4, 8, 16, 32 or 64 KiB**: content-defined chunking. The cut
  points are chosen by looking at the bytes themselves, so after an insertion
  or deletion the same content is cut at the same places again. Chunk sizes
  vary between a quarter of the average and four times it.

Chunked storage also pays for bookkeeping: 32 bytes per chunk in each version's
chunk list, and 48 bytes per new chunk in a pack index. Both are included in
every total. Each version is charged for its whole chunk list, as if the list
were stored flat; a tree-shaped list (see [what this means](#what-this-means-for-transfs))
stores only the parts that changed, so for large files the totals below are
pessimistic. Chunks are written in packs of up to 4 MiB. A GIMP version's new
chunks fit in one pack, so it needs one stored object, as a whole file does. A
SQLite version needs several: with FastCDC 4 KiB, 5 for ordinary use and 21
for the `VACUUM` pair, where a whole file needs one.

Speed, on one core of the dev machine: hashing whole files runs at about
1.1 GB/s; FastCDC plus hashing at about 0.8 GB/s.

## Results

All percentages are of the whole-file cost. Lower is better.

### Different GIMP files

The 37 distinct XCF files in Silicon Circus, 125.6 MiB in total:

| fixed 4 KiB | fixed 64 KiB | FastCDC 4 KiB | FastCDC 16 KiB | FastCDC 64 KiB |
|---:|---:|---:|---:|---:|
| 95.6% | 94.3% | **71.3%** | 74.5% | 78.6% |

Different images share content, most likely reused layers. The shared regions
sit at different offsets in different files, so fixed-size chunks miss them.

The whitepaper describes this set as "171 files, about 371 MB". There are 171
files, but they hold only 37 distinct contents, and they total 582 MiB:
curio hard-links its copies, so `du` counts each copy once. Storing whole files
once already reduces the 582 MiB to 125.6 MiB, so 125.6 MiB is the real
baseline. No file has more than one version, and curio's history holds only two
renames, so the version pairs below were made by scripted edits.

### GIMP edits

Each of the 37 files was opened in GIMP 3.2 and saved unchanged (the *resave*).
Each edit then starts from that resave, so a pair measures the edit and not GIMP
rewriting the file. 37 pairs per row:

| version | whole files | fixed 4 KiB | FastCDC 4 KiB | FastCDC 16 KiB | FastCDC 64 KiB |
|---|---:|---:|---:|---:|---:|
| original → resave | 125.4 MiB | 100.4% | 1.1% | 1.1% | 3.5% |
| resave → resave again¹ | 50.3 MiB | 0.9% | 0.9% | 0.9% | 1.6% |
| small brush stroke | 125.3 MiB | 61.6% | **2.5%** | 3.9% | 10.7% |
| long vertical stroke | 123.9 MiB | 100.2% | **10.5%** | 21.2% | 57.7% |
| new layer | 126.7 MiB | 102.0% | **2.2%** | 2.2% | 4.8% |
| layer moved | 125.4 MiB | 1.0% | **0.9%** | 1.0% | 3.0% |
| colour change on the main layer | 112.0 MiB | 102.0% | 101.4% | 100.3% | 100.1% |

¹ 23 of the 37 files saved byte-identically the second time and cost nothing
under any method; the other 14 differed in about one byte. Saving is close to
deterministic, so the pairs measure the edits.

Why fixed-size chunks fail here: XCF compresses each 64×64 tile separately. A
changed tile compresses to a different length, which moves every byte after
it, so every fixed chunk after the first change is new. Content-defined chunks
find the same cut points again right after the change. The vertical stroke
touches one tile in every row, spread through the layer's data, so it changes
more chunks; larger chunks lose more because each one is likelier to contain a
change.

### SQLite

A generated application database (items, tags and events, with indexes), about
122 MiB, 4 KiB pages. Snapshots were taken with SQLite's backup API, which keeps
the page layout. One pair per row:

| change | whole file | fixed 1 KiB | fixed 4 KiB | fixed 16 KiB | FastCDC 4 KiB | FastCDC 16 KiB |
|---|---:|---:|---:|---:|---:|---:|
| ordinary use: 500 rows edited, 2,000 added, 300 deleted | 122.6 MiB | 8.0% | **8.8%** | 25.9% | 16.3% | 37.3% |
| bulk insert, 10% more rows | 134.7 MiB | 16.7% | **15.3%** | 28.3% | 22.8% | 38.6% |
| `VACUUM` (run on a copy) | 133.0 MiB | 45.7% | **42.5%** | 92.5% | 61.0% | 84.0% |

SQLite changes a database in whole pages, and pages never move. Fixed chunks
the size of a page therefore match exactly what changed. The 500 scattered edits
and their index updates touched about 2,500 of the database's 31,000 pages;
with 16 KiB chunks each change spoils four pages' worth, and with 64 KiB chunks
(not shown, 64%) nearly every chunk holds a change. Content-defined chunks
ignore the page boundaries and pay for it at both edges of every changed page.

Fixed 1 KiB chunks store slightly less than 4 KiB ones, but their bookkeeping
costs more than they save.

### Real SQLite use

Two databases Codex uses every day, both with 4 KiB pages: its conversation
history (about 308 MiB), which mostly grows by appending, and its logs (about
121 MiB), which stay the same size because old entries are pruned and their
pages reused. Copies were taken at 02:46 and 15:47 on 2026-10-05, and at 00:25
and 05:33 on 2026-10-06, with Codex stopped or idle so each copy was a
consistent database. Codex was broken for most of 2026-10-05, so the first
pair changed only 79 bytes in two pages.

| pair | whole file | fixed 4 KiB (one page) | FastCDC 8 KiB |
|---|---:|---:|---:|
| history, broken day (02:46 → 15:47) | 307.1 MiB | 0.8% (2 pages) | 0.6% |
| history, an evening of use (15:47 → 00:25) | 307.7 MiB | 1.0% (171 pages) | 0.7% |
| history, overnight (00:25 → 05:33) | 308.8 MiB | 1.3% (378 pages) | 1.2% |
| logs, about a day (02:46 → 05:33 next day) | 120.6 MiB | **19.0%** (5,550 pages) | 47.8% |

For the conversation history, nearly all of each total is the flat chunk list,
written again in full for every version (2.4 MiB at 4 KiB). The new data alone
is 0.7 MiB for the evening and 1.5 MiB overnight, 0.2–0.5% of the file, which
is what a version costs with a tree-shaped list.

The log database is the scattered-edit case on real data: a day of writes
touched about 18% of its pages, spread through the file as freed pages were
reused. Page-sized chunks store those pages and nothing else; content-defined
chunks also lose the bytes on either side of every changed page, and store
2.5 times as much.

### PNG and WebP exports

The resave and small-stroke versions of the 37 files, flattened and exported:

| small brush stroke | whole files | fixed 4 KiB | FastCDC 4 KiB | FastCDC 64 KiB |
|---|---:|---:|---:|---:|
| PNG | 48.6 MiB | 67.7% | 67.6% | 74.8% |
| WebP | 7.8 MiB | 102.0% | 101.6% | 100.1% |

PNG compresses the image row by row from the top, so everything before the
first changed row compresses to the same bytes and is shared. The stroke here
sits 45–50% of the way down, so about a third of each file is shared; an edit
near the bottom would share more, one near the top almost nothing. Because the
shared part starts at the beginning of the file, fixed and content-defined
chunks do equally well. WebP compresses the whole image together and shares
nothing.

## Compression

Chunking stores less; compression then shrinks what is stored. The two combine
if each chunk is compressed on its own, after chunking, as Borg and restic do.
A chunk's identity stays the hash of its uncompressed bytes, so identical
chunks still match, and compressing differently later changes no identity.

For example, the new version of Codex's log database after a day costs 120.6
MiB stored whole, 27.5 MiB compressed as one stream with zstd, 22.9 MiB as
page-sized chunks, and 5.9 MiB as page-sized chunks each compressed with zstd
and a dictionary.

The `zpairs` mode of the tool compresses every new chunk separately with zstd
at level 3, plainly and with a 110 KiB dictionary trained on the pair's old
version. A dictionary is a sample of typical content that the compressor
starts from, so a small chunk compresses nearly as well as if it were part of
a large file. The baseline is the whole new file compressed as one stream:
what compression gets without chunking. Percentages are of the new version
stored whole and uncompressed, as before.

| new version | best chunking | chunks, uncompressed | chunks, zstd + dictionary | whole file, zstd |
|---|---|---:|---:|---:|
| XCF small stroke | FastCDC 8 KiB | 2.8% | **2.2%** | 77.2% |
| XCF long vertical stroke | FastCDC 8 KiB | 14.5% | **11.8%** | 77.0% |
| XCF new layer | FastCDC 8 KiB | 2.0% | **1.1%** | 76.7% |
| XCF colour change on the main layer | FastCDC 8 KiB | 100.6% | 75.3%¹ | 72.5% |
| SQLite (generated), ordinary use | page-sized | 8.8% | **4.5%** | 38.5% |
| SQLite (generated), bulk insert | page-sized | 15.3% | **7.4%** | 38.5% |
| Codex logs, a day | page-sized | 19.0% | **4.9%** | 22.8% |
| Codex conversation history, overnight | page-sized | 1.3% | **0.9%** | 28.9% |
| PNG small stroke | FastCDC 8 KiB | 67.6% | 67.3% | 100% |
| WebP small stroke | FastCDC 8 KiB | 100.8% | 100.3% | 100% |

¹ zstd without the dictionary: 74.6%. When every tile changes, chunks compress
about as well as the whole file, and chunking adds only its bookkeeping.

For the conversation history, 0.8% of its 0.9% is the flat chunk list, which
compression cannot shrink (it is hashes) and a tree-shaped list would mostly
avoid.

### A file stored fresh

A small chunk compresses worse on its own than as part of a large file,
because the compressor starts each one with no history. This shows when a
file is stored with nothing to share, as on its first version:

| stored fresh | whole file, zstd | chunks, zstd | chunks, zstd + dictionary |
|---|---:|---:|---:|
| XCF (FastCDC 8 KiB) | 77.2% | 79.2% | 77.0% |
| SQLite, generated (page-sized) | 38.5% | 57.7% | 42.8% |
| Codex logs (page-sized) | 22.8% | 39.5% | 30.1% |
| Codex conversation history (page-sized) | 28.9% | 43.1% | 39.7% |
| PNG (FastCDC 8 KiB) | 100% | 100.7% | 97.2% |

XCF loses nothing: its content is already arranged in independent tiles. For
SQLite the dictionary closes most of the gap, and the gap is paid once: every
later version is far cheaper chunked than compressed whole (0.9–4.9% against
22.8–38.5% for the databases above).

### Dictionary size and compression level

A 110 KiB dictionary, zstd's usual size, did clearly better than 16 KiB: on
Codex's logs, 4.9% against 5.5% per version and 30.1% against 33.4% fresh. A
dictionary is stored once per database, so its size costs little next to a
large file.

Higher zstd levels compress a little better and run much slower (one core):

| zstd level | SQLite fresh | XCF fresh | speed |
|---|---:|---:|---:|
| 1 | 50.8% | 79.3% | 410–550 MB/s |
| **3** | **42.8%** | **77.0%** | **300–330 MB/s** |
| 9 | 38.9% | 74.1% | 73–80 MB/s |
| 19 | 38.0% | 72.4% | 14–15 MB/s |

Version costs barely change with the level (SQLite 4.5% at level 3, 4.0% at
level 19). Level 3 keeps writes fast. Because a chunk's identity is its
uncompressed hash, rarely used chunks can be recompressed at a high level
later, in the background, without changing anything that refers to them.

### Where a dictionary helps

The dictionaries above were trained on the previous version of the same file.
Trained on other files, they did not help. Measured with `zstd`'s benchmark
mode, compressing the generated database's bulk-insert version in 4 KiB pages:

| dictionary trained on | compression |
|---|---:|
| none | 1.79× |
| an earlier version of the same database | **2.46×** |
| another database with the same schema and different data | 1.77× |
| a database with a different schema | 1.76× |

PNG gained nothing from a dictionary trained on other images (1.000× without,
0.999× with), and GIMP files nothing from one trained on all 37 XCFs (1.279×
without, 1.273× with). A dictionary helps by knowing a file's own vocabulary,
not its format. The generated databases draw their words from a random
vocabulary that differs per database, so two real databases from one
application may share more than these did; that is untested.

### Compute cost

On one core of the dev machine (Intel Core Ultra 7 155H), from `zstd`'s
benchmark mode and this tool:

| step | speed |
|---|---:|
| SHA-256 of a whole file (what transfs does today) | 1.1–1.8 GB/s |
| fixed-size chunks plus SHA-256 per chunk | 1.0–1.1 GB/s |
| FastCDC plus SHA-256 per chunk | 0.8 GB/s |
| zstd level 3, 4–8 KiB chunks | 290–310 MB/s |
| zstd level 3, 4–8 KiB chunks with a dictionary | 200–240 MB/s |
| training a dictionary on 120 MiB of samples | about 3 s, once |
| zstd decompression, 4–8 KiB chunks | 800–930 MB/s |
| zstd decompression, a whole file | 1.1–1.3 GB/s |

Chunking costs about a third more than today's single hash. Compression is the
expensive step, about four times slower than chunking, so storing a large file
fresh runs at about 190 MB/s on one core. Two things soften that:

- **Only new chunks are compressed.** A new version is read and chunked in
  full but compressed only where it changed. For Codex's log database that is
  about 0.15 s to chunk 120 MiB and 0.09 s to compress the 22 MiB that changed,
  about the same work as compressing the whole file once.
- **Chunks are independent**, so compression spreads across cores. On a few
  cores it outruns most disks, and it is far faster than uploading to a remote
  store over a home connection, where it saves time by sending a quarter of the
  bytes or less.

Put together, writing new data with compression runs at about 220 MB/s per
core: about four times slower than chunking alone, and five to eight times
slower than today's single hash. Because a chunk's identity is the hash of its
uncompressed bytes, compression could also be deferred: chunks stored raw at
chunking speed and compressed later in the background, with no identity
changed. Already-compressed formats skip compression and always write at
chunking speed.

Reading is cheap: a 3.5 MB GIMP file decompresses in about 4 ms.

## What this means for transfs

Two ways of cutting chunks, chosen by file type, with one way of storing them:

1. **Most files: content-defined chunks, 8 KiB average (decided 2026-10-05).**
   FastCDC with a 2 KiB minimum and a 32 KiB maximum. Smaller chunks store
   less, but every chunk costs an entry in the index of stored chunks and
   another piece to fetch on read. 8 KiB is where the curve bends: it keeps
   nearly all of 4 KiB's saving with half as many chunks.

   | FastCDC average | 4 KiB | 8 KiB | 16 KiB | 32 KiB | 64 KiB |
   |---|---:|---:|---:|---:|---:|
   | different XCFs | 71.3% | 73.0% | 74.5% | 76.5% | 78.6% |
   | small stroke | 2.5% | 2.8% | 3.9% | 7.2% | 10.7% |
   | long vertical stroke | 10.5% | 14.5% | 21.2% | 37.8% | 57.7% |
   | new layer | 2.2% | 2.0% | 2.2% | 3.0% | 4.8% |

   Roughly 100 bytes of index per chunk, so a terabyte of distinct data is
   about 134 million chunks and a 13 GB index at 8 KiB (27 GB at 4 KiB, 7 GB at
   16 KiB). If real stores grow to where that hurts, move to 16 KiB. A change
   only loses sharing between versions stored before and after it, because a
   version's identity is its content hash. The chunking parameters (sizes and
   FastCDC's settings) must be fixed in the format, since writers share chunks
   only if they cut the same way. One size for all files: choosing by file size
   would cut a file differently once it grew past the threshold, and its
   versions would stop sharing.
2. **SQLite files: fixed-size chunks, one database page each.** A SQLite file
   starts with `SQLite format 3\0`, and bytes 16–17 give the page size
   (usually 4 KiB). Fixed page-sized chunks halve the cost of SQLite versions
   compared with content-defined chunks, and this is the workload @pandora is
   waiting on.
3. **Either way, a file version is a list of chunk references, held in
   merkle-champ's `Sequence`.** The chunks can be any length and are stored once
   each under their own hash; the list holds each chunk's identity and length.
   The list's structure matters for big files. A 307 MiB database at 4 KiB is
   about 78,000 chunks, a 2.4 MiB list: stored flat, every version would write
   all of it again, even for a change to two pages (measured on a real
   database, below). `Sequence` is a content-defined tree (a "prolly tree"): a
   rolling hash over the entries decides where its nodes end, the same idea
   FastCDC applies to bytes. An edit, including an insert that shifts every
   later entry, rewrites only the nodes near it, about 3–4 nodes at a million
   entries, and equal lists always have equal identities. Branches will carry
   each child's byte length, so finding the chunk that holds a given byte is one
   walk down the tree.
4. **Compress each chunk with zstd at level 3.** A large SQLite database also
   gets its own dictionary of about 110 KiB, trained on its own content; a
   dictionary trained on other files did not help. Formats that are already
   compressed, such as PNG and WebP, are stored as they are. The per-type
   settings are collected in [file types](file-types.md).
5. **No RRB tree is needed for storage.** Content-defined chunks already keep
   an edit's cost local. An RRB tree would only be needed for an editor that
   splices bytes in place.
6. **Small files** were not measured. Below some size the chunk list costs more
   than chunking can save, so they should stay whole blobs; the threshold is
   still to be found.

## Still to measure

- **Audio and video** were not tested.

## Limits

- The GIMP edits are scripted, made with one GIMP version on 37 images from
  one project. Real editing sessions mix several kinds of edit before saving.
- The generated SQLite database's schema and access pattern are guesses at an
  application's. The real databases are two, both from one application.
- The dictionaries in the compression tables were trained on the previous
  version of the same file. Dictionaries trained on other files gained nothing
  ([where a dictionary helps](#where-a-dictionary-helps)), but the generated
  databases share no vocabulary, so real databases from one application are
  untested.
- Each baseline resave cleared any saved selection, which would otherwise
  confine the scripted paint. One file had one.

## Rerunning

The tool and the scripts that made the test files are in `tools/chunk-study/`.
Generated files go in `~/.local/share/transfs-chunk-study/`, outside the
repository.

```sh
cd tools/chunk-study
cargo build --release

# SQLite snapshots (seeded, so the same every run)
python3 make_sqlite.py ~/.local/share/transfs-chunk-study/sqlite

# GIMP edit versions, then PNG and WebP exports
XCF_LIST=sources.txt XCF_OUT=~/.local/share/transfs-chunk-study/xcf-edits \
  gimp-console -i --quit --batch-interpreter=python-fu-eval \
  -b 'exec(open("make_xcf_edits.py").read())'
XCF_OUT=~/.local/share/transfs-chunk-study/xcf-edits \
  gimp-console -i --quit --batch-interpreter=python-fu-eval \
  -b 'exec(open("export_flat.py").read())'

# Measure a pair
D=~/.local/share/transfs-chunk-study/sqlite
target/release/chunk-study pairs "SQLite: ordinary use" $D/base.sqlite $D/updates.sqlite
target/release/chunk-study zpairs "SQLite: ordinary use" $D/base.sqlite $D/updates.sqlite
```

`sources.txt` lists one XCF path per line; the run above used one path for each
of the 37 distinct XCF contents in Silicon Circus.
