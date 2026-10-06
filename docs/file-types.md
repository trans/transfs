# File types

> **Status:** proposed, 2026-10-06. Nothing here is built yet. The numbers come
> from the [chunk study](chunk-study.md); rows marked *not measured* are
> defaults to check.

transfs stores a file's bytes as chunks, and the best way to cut and compress
them depends on what kind of file it is. A SQLite database changes in whole
pages, so it is cut at page boundaries; a GIMP image shifts bytes when a tile
changes, so it is cut where its content says; a WebP image is already
compressed, so compressing it again only wastes time. This table records the
settings for each kind of file and what they cost.

For example, a 120 MiB SQLite database that changes for a day is stored as
page-sized chunks, each compressed with zstd and the SQLite dictionary. Its
new version costs 5.9 MiB instead of another 120 MiB.

## Settings

| kind of file | recognized by | chunks | compression | a new version costs | a fresh copy costs |
|---|---|---|---|---|---|
| SQLite database | starts with `SQLite format 3\0` | fixed, one page each; page size from bytes 16–17 | zstd level 3 with a dictionary | 0.9–7.4% | 30–43% |
| GIMP image (XCF) | starts with `gimp xcf ` | FastCDC, 8 KiB average | zstd level 3 with a dictionary | 1.1–11.8% for local edits; about 75% when a whole layer changes | 77% |
| PNG image | starts with `89 50 4E 47 0D 0A 1A 0A` | FastCDC, 8 KiB average | zstd level 3 with a dictionary (saves about 3%) | about 67% for an edit halfway down; depends on where the edit is | 97% |
| WebP image | `RIFF`, then `WEBP` at byte 8 | FastCDC, 8 KiB average | none | 100% | 100% |
| anything else | – | FastCDC, 8 KiB average | zstd level 3; stored raw when that doesn't shrink it | *not measured* | *not measured* |
| small files | under 32 KiB | none: one whole blob | zstd level 3 when it shrinks the file | *not measured* | *not measured* |

Percentages are of the file stored whole and uncompressed. "A new version"
counts what the previous version did not already have; "a fresh copy" is a file
stored with nothing to share. For comparison, compressing the whole file as one
stream costs 23–39% for the SQLite databases measured and 77% for XCF, for
every version.

## How the settings apply

- **The kind of file is recognized from its first bytes, not its name.**
  transfs already derives a file's type from its content, and names carry no
  meaning in the store.
- **FastCDC uses one set of parameters everywhere:** 2 KiB minimum, 8 KiB
  average, 32 KiB maximum. Writers share chunks only if they cut the same way,
  so these are part of the format. Choosing a chunk size by file size would cut
  a file differently once it grew past the threshold, and its versions would
  stop sharing.
- **A chunk's identity is the SHA-256 of its uncompressed bytes.** Compression
  is how a chunk is stored, not what it is. Identical chunks match whatever
  their compression, and chunks can be recompressed later (for example at a
  higher level, for rarely used data) without changing any identity.
- **Dictionaries are one per kind of file per store**, about 110 KiB, trained
  on a sample of that kind's chunks once enough exist. A dictionary is stored
  once, under its own hash. Each compressed chunk names the dictionary it needs
  (zstd records a dictionary's id in every frame). Chunks stored before a
  dictionary existed stay as they are.
- **A version's chunk list is a merkle-champ `Sequence`** of chunk identities
  and lengths, so an edit to a large file rewrites only a few nodes of its list.

## Still to decide or measure

- The small-file threshold (32 KiB above is a guess).
- Audio, video, JPEG, archives and office documents. Most are already
  compressed and likely behave like WebP; metadata edits near the start of a
  file may still share the rest.
- How well a dictionary trained on other files of a kind does, compared with
  the study's dictionaries, which were trained on the previous version of the
  same file.
