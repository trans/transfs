# Using transfs

transfs is an archive for your files. You give it files and it keeps them —
every version, nothing overwritten. You find them again by describing them
(what they are, what year, what you tagged them with) instead of remembering
which folder you put them in.

This guide walks through using it with commands and illustrative output, and
ends with a look at what transfs writes to disk. For building it and a command
reference, see the [README](../README.md). For why it is designed the way it
is, see [architecture.md](architecture.md).

transfs is early software. It runs on one machine, from the command line, with a
read-only view of the archive as folders. [Not built yet](#not-built-yet) lists
what's missing.

The Rust format starts at claim format 2. Crystal-era stores are unsupported;
there are no existing user stores to migrate.

## How it differs from folders

In a folder tree, each file lives in one place, and finding it means remembering
that place. In transfs:

- **A file you add becomes a *document*.** A document has a name, tags and a
  history of versions. It doesn't live in a folder.
- **You find a document by what it is**, not where it is: everything tagged
  `finance`, every image, everything from 2024 tagged `vacation`.
- **Nothing is overwritten.** Changing a document adds a new version, and the
  old ones stay.

## Before you start

Build transfs and put `target/debug/transfs` on your PATH (see the README). Then choose
where the archive lives by setting `TRANSFS_STORE`:

```sh
export TRANSFS_STORE=~/archive        # bash, zsh
set -x TRANSFS_STORE ~/archive        # fish
```

transfs creates the directory the first time you add something. If you don't
set it, transfs uses `test/store` under whatever directory you're in, which is
meant for development. You can also give the location per command:
`transfs --store ~/archive list`.

## Add a file

```
$ transfs add tax-return-2025.pdf
added b88976cb1af5  "tax-return-2025.pdf"  (1 version, head 98fa12007f70)
```

transfs copies the file's contents into the archive, so you can move or delete
your copy afterwards.

`b88976cb1af5` is the start of the new document's id. Commands that act on one
document take its id, and any start of it that's unique in your archive will
do, so `b889` works too. (`head` identifies the file's contents; see
[Under the hood](#under-the-hood). The `add` summary abbreviates the blob
hash; `show` and `versions` identify the version claim separately.)

The document's name defaults to the file's name. To give it a different one, add
it after the file: `transfs add scan0042.pdf "lease.pdf"`.

## See what's in the archive

After adding a few more files and tagging them (next section), `list` shows:

```
$ transfs list
b88976cb1af5  tax-return-2025.pdf       application/pdf   v1 heads=1  owner/local,tag/finance,type/application/pdf,year/2025
a4270f088d7f  receipt-laptop.pdf        application/pdf   v1 heads=1  owner/local,tag/finance,tag/warranty,type/application/pdf,year/2024
173d4a043633  beach.png                 image/png         v1 heads=1  owner/local,stars/4,tag/vacation,type/image/png,year/2024
9c44465cc681  sunset.png                image/png         v1 heads=1  owner/local,stars/3,tag/vacation,type/image/png,year/2024
2ea429b8634e  notes.md                  text/plain        v1 heads=1  owner/local,tag/garden,type/text/plain
```

The columns are the id, the name, the type, the number of versions, the number
of current content heads, and everything you can search on. Here `v1` means
one content version, not claim format 1.

- **The type comes from the file's contents**, not its name. A PDF named
  `notes.txt` is still `application/pdf`.
- **`type/…` and `owner/local` are added by transfs itself.** The owner is
  always `local` for now; it will mean something once an archive can be shared.
- **Plain tags show as `tag/finance`.** That's how they're grouped in the
  folder view, below.

## Tag documents

```
$ transfs tag b88976cb1af5 finance year/2025
tags: finance, year/2025
```

A tag is either a plain word (`finance`, `vacation`, `garden`) or a key and a
value (`year/2025`, `stars/4`). Use a key and value when the tag answers a
question that other documents answer differently: what year? how many stars?
which project? Keys are what make the folder view useful.

`year=2025` means the same as `year/2025`. On the command line, though, a word
containing `=` looks like an option, so put `--` before the tags if you use it:
`transfs tag b88976cb1af5 -- year=2025`.

Values can go deeper, like `date/2024/04/30`. The visible facet uses the most
specific tag on each lineage: `date/1920/10/10` implies `date/1920`.
Concurrent assertions remain in the claim log even when the facet shows only
the deeper path.

### A key with one value: `set`

`tag` adds. Tag the beach photo `stars/5` when it already has `stars/4`, and it
ends up with both:

```
$ transfs tag 173d4a043633 stars/5
tags: stars/4, stars/5, vacation, year/2024
```

That's right for a key like `project`, where a document can belong to several.
For a key that should hold one value, like a rating, use `set`, which replaces
whatever the key held:

```
$ transfs set 173d4a043633 stars 5
tags: stars/5, vacation, year/2024
```

transfs doesn't know which keys hold one value. The command you use decides:
`set` for those keys, `tag` for the rest.

### Remove a tag, rename, show

```
$ transfs untag a4270f088d7f warranty
tags: finance, year/2024

$ transfs rename b88976cb1af5 "taxes 2025.pdf"
renamed b88976cb1af5 -> "taxes 2025.pdf"
```

A name is just a label, and two documents can have the same one. The folder view
tells them apart by adding the start of each id, as in `notes~4005.md` and
`notes~9c86.md`.

`show` prints everything about one document:

```
$ transfs show b88976cb1af5
id:        b88976cb1af506c5865a6ab1a918a8a48867bdb0a3d139aeb29ec6fcbd014927
names:     taxes 2025.pdf
created:   2026-10-02T00:32:56.701581260Z
versions:  1
heads:     98fa12007f70
tags:      finance, year/2025
```

## Find documents

`find` takes one condition:

| you type | it finds |
|---|---|
| `tag:finance` | documents with that tag |
| `tag:year/2024` | documents with that key and value |
| `tag:year` | documents with any `year` |
| `type:image` | documents whose type starts with `image` (`image/png`, `image/jpeg`, …) |
| `beach` | documents whose name contains `beach` |

```
$ transfs find tag:finance
b88976cb1af5  tax-return-2025.pdf       application/pdf   v1 heads=1  owner/local,tag/finance,type/application/pdf,year/2025
a4270f088d7f  receipt-laptop.pdf        application/pdf   v1 heads=1  owner/local,tag/finance,tag/warranty,type/application/pdf,year/2024

$ transfs find type:image
173d4a043633  beach.png                 image/png         v1 heads=1  owner/local,stars/4,tag/vacation,type/image/png,year/2024
9c44465cc681  sunset.png                image/png         v1 heads=1  owner/local,stars/3,tag/vacation,type/image/png,year/2024
```

To combine conditions, such as vacation photos from 2024, use the folder view.

## Versions

When a document changes, add the new file as a new version of it:

```
$ transfs addversion 2ea429b8634e notes-v2.md
added version 27af67e011be to 2ea429b8634e (now v2)

$ transfs versions 2ea429b8634e
  7ccada649807  blob=2bb80d537b1d  parents=(root)  2026-10-02T00:32:56.701581260Z
* 27af67e011be  blob=7d3dc8e23a83  parents=7ccada649807  2026-10-02T00:33:10.792006856Z

$ transfs cat 2ea429b8634e
# Garden notes

Plant tomatoes in May.
Basil next to them.
```

`versions` shows each version ID, complete-content blob hash, and parent
version IDs. `*` marks every current head. `cat` prints the content when there
is one head; `cat ID VERSION_ID` reads a specified version, including an older
one. Concurrent edits from one base create multiple heads, and plain `cat ID`
then asks you to select one. `addversion ID FILE --parents ID,ID` creates a
resolution version after inspecting both heads.

## Browse the archive as folders

`transfs mount` shows the archive as a folder tree that you can browse with
`ls`, open in a file manager, or read from any program. It keeps running until
you stop it, so start it in a second terminal:

```sh
mkdir ~/archive-view
transfs mount ~/archive-view
```

Then, in your first terminal, `cd ~/archive-view`. None of these folders is
stored anywhere. Each one is a search, and each level down narrows it.

At the top are the keys:

```
$ ls
stars
tag
type
year
```

`stars` and `year` come from your tags, and `type` is the one transfs adds.
Plain tags like `finance` are gathered under `tag`.

Inside a key are its values, and inside a value are the keys you can narrow by
next:

```
$ ls year
2024
2025

$ ls year/2024
stars
tag
type
```

To see documents, open the `=` folder at any level:

```
$ ls year/2024/=
beach.png
receipt-laptop.pdf
sunset.png
```

Narrowing by two things is just going down two levels, in either order:

```
$ ls year/2024/tag/vacation/=
beach.png
sunset.png
```

`tag/vacation/year/2024/=` lists the same two, and `year=2024/tag=vacation/=`
works as well. An `=` at the top lists every document:

```
$ ls =
beach copy.png
beach.png
notes.md
receipt-laptop.pdf
sunset.png
taxes 2025.pdf
```

Files open like any others:

```
$ cat tag/garden/=/notes.md
# Garden notes

Plant tomatoes in May.
Basil next to them.
```

When content has forked, the mount shows one file per head, such as
`notes~a1b2c3d4.md` and `notes~e5f6a7b8.md`. Concurrent names show the
document under each current label.

The view is read-only. Saving into it fails, and that's deliberate: it couldn't
know what name, tags or document the new file should have. Use the commands
above to change the archive.

```
$ cp ~/notes.md tag/garden/=/
cp: cannot create regular file 'tag/garden/=/notes.md': Read-only file system
```

Stop the view with `fusermount3 -u ~/archive-view`.

## The same file twice

Adding a file whose contents are already in the archive makes a new document,
with its own name, tags and history. The contents are stored only once:

```
$ transfs add beach.png "beach copy.png"
added e7ceed25288e  "beach copy.png"  (1 version, head cc9c6b0a76f4)

$ transfs check
ok: 6 documents, 5 blobs
```

Six documents but still five stored files (blobs): the copy shares the beach
photo's. Its `head`, `cc9c6b0a76f4`, is the same as the beach photo's.

## Check the archive

`transfs check` reads the whole archive and reports problems: a stored file
whose contents don't match its name, a version whose file is missing, a damaged
log. It prints `ok` when there are none. Stored files that no document uses are
reported as warnings.

`transfs reindex` rebuilds the search database (see below) from scratch.

## Under the hood

The archive directory holds three things:

```
archive/
  blobs/27/27af67e011be08fabe402f960deab26c6e2e2462a5d810af89fc4ed9629f1e7f
  blobs/7c/7ccada649807dd1385b6ecb6a8e5a3a53c425349f35a94aac59683cee20bc1ae
  …
  .transfs/docs/2e/2ea429b8634e6b730d995a5acfc4b626f2bdcf6203b913170767a161c8502e27.log
  .transfs/docs/17/173d4a043633b2956b6a2b0ceb688c628df84baf2e6dfb61fd40e685d239b5c2.log
  …
  .transfs/index.db
```

**Blobs hold file contents.** Each blob is named by the SHA-256 hash of its
bytes, a 64-character fingerprint. Identical bytes always give the same name,
which is how the beach copy shares the beach photo's blob. The first two
characters are a subfolder, to keep any one folder from getting huge. A blob has
no name, type or tags; it's just bytes.

**Logs hold everything else.** Each document has one log, and transfs only ever
adds lines to the end of it. This schematic shows the claim fields; the IDs and
hashes below are abbreviated for readability:

```text
create(format=2, nonce=..., ts=...)                      → document ID D
v2_version(nonce=..., hash=blob-A, parents=[])          → version ID V1
v2_name(nonce=..., name="notes.md", supersedes=[])     → name ID N1
v2_tag_add(nonce=..., tag="garden", supersedes=[])     → tag ID T1
v2_version(nonce=..., hash=blob-B, parents=[V1])       → version ID V2
```

Each ID hashes the claim's validated canonical value, using merkle-champ's
`Identify` encoding; it does not hash the JSON text. A version parent is a
**version ID**, while `hash` identifies the complete file bytes. Reverting to
identical bytes creates a new version ID. Transfs derives current names, tags,
and content heads from the causal references, so claim order and timestamps do
not silently choose a winner. Lines are never changed or deleted.

**`index.db` is a search database** built from the logs, so that `list`, `find`
and the folder view are fast. It holds nothing the logs don't. If it's deleted
or damaged, `transfs reindex` rebuilds it.

## Not built yet

- **Deleting a document.** Nothing removes a document or its stored files yet.
- **Editing.** A way to check a document out, edit it, and check it back in as a
  new version.
- **Finding by description.** Typing something like
  `transfs checkout finance 2025 taxes` and picking from a list when several
  documents match.
- **Folders and groups as documents.** A whole folder, such as a website, kept
  as one document; and collections, which group documents like an album.
- **Export and import**, for moving documents between archives.
- **Cleaning up** stored files that no document uses any more.
- **More than one machine or person**: syncing archives, signing, permissions.
