# Claim format 2

This is the identity contract for Rust transfs claims. JSON lines are the
readable log format; a claim ID hashes its validated value, independent of JSON
key order or whitespace. Format 2 replaces the earlier Crystal-compatible log
format. The first record of every document log is a `create` claim with
`"format":2`; unsupported formats are rejected.

## Identity bytes

The claim's [`merkle-champ` `Identify` value](https://docs.rs/crate/merkle-champ/0.1.0/source/FORMAT.md)
is fed to SHA-256 and displayed as 64 lowercase hex digits. The same value
encoding can later be used inside a CHAMP node. It starts with the custom type
tag `C`, followed by merkle-champ's `Identify(str)` for the domain
`transfs/claim/v2` and `Identify(u64)` for the operation number. The operation
numbers are `0=create`, `1=version`, `2=name`, `3=tag add`, `4=tag remove`.

Subsequent fields, in order:

| Operation | Fields after number |
|---|---|
| `create` | `Identify(u64)` format (=2), `Identify(bytes)` 16-byte nonce, `Identify(str)` timestamp |
| `v2_version` | nonce, timestamp, `Identify(identity)` complete-content SHA-256, identity set of parent version IDs |
| `v2_name` | nonce, timestamp, `Identify(str)` name, identity set of superseded name IDs |
| `v2_tag_add` | nonce, timestamp, `Identify(str)` normalized tag, `Identify(u64)` scope-present flag (0 or 1), optional `Identify(str)` normalized scope, identity set of superseded tag assertions |
| `v2_tag_remove` | nonce, timestamp, `Identify(str)` normalized tag, identity set of removed tag-assertion IDs |

`nonce` and `timestamp` use the same encodings described in the create row.
An identity set is `Identify(u64)` of its member count, followed by each
member's `Identify(identity)` in ascending lowercase-hex order, with duplicates
removed. `Identify(identity)` takes the 32 decoded bytes, not the 64 text
characters. `Identify(str)` and `Identify(bytes)` include a one-byte type tag
and a little-endian `u64` byte length. `Identify(u64)` also carries a
little-endian length of 8 and then the little-endian value. `Identify(identity)`
uses tag `#` and its 32 bytes. The custom `C` tag and domain keep claims
separate from other CHAMP values.

Timestamps are UTC RFC3339 with exactly nine fractional digits and `Z` for
identity purposes. Nonces are 16 random bytes; JSON writes them as 32 lowercase
hex digits. Blob hashes and claim IDs are 32-byte SHA-256 values written as 64
lowercase hex digits. Tag paths replace `=` with `/` and remove empty segments
before identity. Names are nonempty flat labels without `/`, NUL, `.` or `..`.

The create claim's ID is the document ID. A version claim's ID identifies one
edit; its `hash` identifies the complete resulting bytes, so a revert to
identical bytes still has a new version ID. Parent links always use version
IDs. Name and tag references use claim IDs. Timestamps never resolve conflicts.

## Golden vectors

All timestamps below are `2026-01-02T03:04:05.000000000Z`. The examples use
the complete values shown; no implicit document ID is part of an edit claim's
identity.

| Claim | ID |
|---|---|
| `create(format=2, nonce=0000000000000000000000000000002a)` | `0782bcfaf44d3aebd6930307859512c4340c4acfa5be7b26ec3ff7028eb8068b` |
| `v2_version(nonce=00000000000000000000000000000007, hash=` followed by 64 `a` digits, `parents=[])` | `91af52793f4038eaa1c2ef8b04ad9d3ba8fd6858be25d7e4a71148f7c7cabf9d` |
| `v2_name(nonce=00000000000000000000000000000001, name="a\x1fb,c☃", supersedes=[])` | `d6c1e316c4d72c90f3696ae8d1d9347ccde20e8570911931c82c2854f566f7d3` |

The executable vectors are in [`tests/causal.rs`](../tests/causal.rs).
