set tempdir := "/tmp"

# List the available tasks.
default:
    @just --list

# Check that this shell can use the host FUSE device.
fuse-device:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -c /dev/fuse ]]; then
        echo '/dev/fuse is unavailable here; run this on a host with FUSE access.' >&2
        exit 1
    fi
    if ! command -v fusermount3 >/dev/null; then
        echo 'fusermount3 is unavailable; install the FUSE 3 userspace tools.' >&2
        exit 1
    fi
    echo 'FUSE device and fusermount3 are available.'

# Build the native CLI.
fuse-build:
    cargo build --offline --bin transfs

# Create or reuse an isolated example store under ignored target/.
fuse-prepare: fuse-build
    #!/usr/bin/env bash
    set -euo pipefail
    base=target/fuse-smoke
    mkdir -p "$base/mnt"
    if [[ -f "$base/.seeded" && -d "$base/store" && -f "$base/hello.txt" ]]; then
        echo "Reusing $base/store"
        exit 0
    fi
    if [[ -e "$base/store" || -e "$base/.seeded" ]]; then
        echo "Incomplete fixture at $base; inspect it before retrying." >&2
        exit 1
    fi
    printf 'hello fuse\n' > "$base/hello.txt"
    target/debug/transfs --store "$base/store" add "$base/hello.txt" hello.txt
    touch "$base/.seeded"

# Mount the example store in the foreground; use a second terminal for verify/unmount.
fuse-mount: fuse-device fuse-prepare
    target/debug/transfs --store target/fuse-smoke/store mount target/fuse-smoke/mnt

# Ensure the example mount is active before checking it.
fuse-mounted:
    @mountpoint -q target/fuse-smoke/mnt || { echo "The example mount is not active; run 'just fuse-mount' in another terminal." >&2; exit 1; }

# List the facet directory and the document view.
fuse-list: fuse-mounted
    ls target/fuse-smoke/mnt
    ls target/fuse-smoke/mnt/=

# Read the example document and compare it with its source file.
fuse-read: fuse-mounted
    cat target/fuse-smoke/mnt/=/hello.txt
    cmp target/fuse-smoke/hello.txt target/fuse-smoke/mnt/=/hello.txt

# Request filesystem statistics from the mount.
fuse-stat: fuse-mounted
    stat -f target/fuse-smoke/mnt

# Confirm that the mount rejects a new file.
fuse-readonly: fuse-mounted
    #!/usr/bin/env bash
    set -euo pipefail
    if touch target/fuse-smoke/mnt/new-file 2>/dev/null; then
        echo 'A write unexpectedly succeeded on the read-only mount.' >&2
        exit 1
    fi
    echo 'The mount rejected the write.'

# Run all live mount checks.
fuse-verify: fuse-list fuse-read fuse-stat fuse-readonly
    @echo 'FUSE smoke test passed.'

# Unmount the example store and let the foreground fuse-mount task exit.
fuse-unmount:
    @mountpoint -q target/fuse-smoke/mnt || { echo 'The example mount is not active.' >&2; exit 1; }
    fusermount3 -u target/fuse-smoke/mnt
