require "crystalfuse"
require "./library"
require "./query"

# `statvfs(2)` isn't bound by Crystal's stdlib or crystalfuse, so bind it here.
# (crystalfuse exposes the top-level module `Fuse` since v0.3.0.)
# The kernel fills this struct, so its size must be >= the real C `statvfs`;
# the spare array is over-sized on purpose (over-allocation is safe, only
# under-allocation would let the syscall scribble past it). Field layout
# mirrors glibc's x86-64 `struct statvfs`.
lib LibC
  struct Statvfs
    f_bsize   : ULong
    f_frsize  : ULong
    f_blocks  : ULong
    f_bfree   : ULong
    f_bavail  : ULong
    f_files   : ULong
    f_ffree   : ULong
    f_favail  : ULong
    f_fsid    : ULong
    f_flag    : ULong
    f_namemax : ULong
    __spare   : StaticArray(ULong, 6) # absorbs f_type + __f_spare[5] and any drift
  end

  fun statvfs(path : Char*, buf : Statvfs*) : Int32
end

module TransFS
  # A read-only FUSE view over the claim-log store: the **facets-default
  # query-path mount** (docs/architecture.md §7). Facets are the default view, so
  # `ls /` is the facet menu (the tag keys) and what you `cd` into is what `ls`
  # lists — POSIX-consistent. Navigation is a prefix-walk over hierarchical
  # tag-paths (`Index#walk`); a lone `=` toggles to the document view, where the
  # matching docs are *rendered* (recency-windowed) and a trailing segment names a
  # leaf to read.
  #
  # Deferred to later slices: supersession (`add`/`set` verbs), composites
  # (dir-rendered manifests), computed recognition names (here, same-name
  # collisions get a minimal `~id` suffix), and literal `*` interpretation (the
  # shell expands `*` against the listed facet keys for free).
  #
  # Read-only and honest about it: the mount is taken with `-o ro`, so the kernel
  # itself returns EROFS on writes (mutation is via the CLI).
  class FuseSystem < Fuse::FileSystem
    # Recency window: a doc view lists at most this many most-recent matches.
    RECENT_LIMIT = 200

    def initialize(@lib : Library)
      super()
    end

    # Mount over *root* at *mountpoint*, foreground.
    def self.mount(root : String, mountpoint : String)
      new(Library.new(root)).mount(["transfs", "-f", mountpoint])
    end

    # The display leaves of a query result: each row mapped to a display name,
    # head-less documents skipped (no content to open yet — §5 bare shells), and
    # same-name collisions disambiguated *within this set* (the result set is the
    # neighborhood). Minimal stable rule, NOT the design's computed recognition
    # name (a later slice): "report.pdf" and "report~a1b2.pdf".
    private def leaves(rows : Array(Index::Row2)) : Array({String, Index::Row2})
      rows = rows.reject { |r| r.head_hash.nil? }
      seen = Hash(String?, Int32).new(0)
      rows.each { |r| seen[r.name] += 1 }
      rows.map do |r|
        base = r.name || "untitled-#{r.id[0, 8]}"
        name = seen[r.name] > 1 ? disambiguate(base, r.id) : base
        {name, r}
      end
    end

    # Insert a short id suffix before the extension: report.pdf -> report~a1b2.pdf
    private def disambiguate(base : String, id : String) : String
      suffix = "~#{id[0, 4]}"
      if (dot = base.rindex('.')) && dot > 0
        "#{base[0, dot]}#{suffix}#{base[dot..]}"
      else
        "#{base}#{suffix}"
      end
    end

    # The document leaf named `name` in the doc view of `walk`, or nil.
    private def leaf_row(walk : Index::Walk, name : String) : Index::Row2?
      leaves(@lib.index.docs(walk, RECENT_LIMIT))
        .find { |(n, _)| n == name }.try { |(_, row)| row }
    end

    def getattr(path : String) : Fuse::FileAttr | Int32
      parsed = Query.parse(path)
      walk = @lib.index.walk(parsed.components)
      return -Errno::ENOENT.value unless walk.valid
      if name = parsed.doc_name
        row = leaf_row(walk, name)
        return -Errno::ENOENT.value unless row
        hex = row.head_hash
        return -Errno::ENOENT.value unless hex
        real = @lib.blob_path(hex)
        return -Errno::ENOENT.value unless File.exists?(real)
        Fuse::FileAttr.file(size: File.size(real).to_i64, mode: 0o444)
      else
        Fuse::FileAttr.dir # facet view and doc listing are both directories
      end
    end

    def readdir(path : String) : Array(String) | Int32
      parsed = Query.parse(path)
      walk = @lib.index.walk(parsed.components)
      return -Errno::ENOENT.value unless walk.valid
      return -Errno::ENOENT.value if parsed.doc_name # a file, not a directory
      entries =
        if parsed.doc_view
          leaves(@lib.index.docs(walk, RECENT_LIMIT)).map { |(n, _)| n }
        else
          @lib.index.facets(walk)
        end
      [".", ".."] + entries
    end

    def open(path : String) : Int32
      parsed = Query.parse(path)
      return -Errno::ENOENT.value unless name = parsed.doc_name
      walk = @lib.index.walk(parsed.components)
      return -Errno::ENOENT.value unless walk.valid
      leaf_row(walk, name) ? 0 : -Errno::ENOENT.value
    end

    # Read by filling the kernel's own buffer directly (the zero-copy escape
    # hatch), streaming bytes from the leaf's head blob (index-only — the head
    # hash rides on the matched row, no log re-fold).
    def read(path : String, buffer : Bytes, offset : Int64, fi : Fuse::FileInfo) : Int32
      parsed = Query.parse(path)
      return -Errno::ENOENT.value unless name = parsed.doc_name
      walk = @lib.index.walk(parsed.components)
      return -Errno::ENOENT.value unless walk.valid
      row = leaf_row(walk, name)
      return -Errno::ENOENT.value unless row
      hex = row.head_hash
      return -Errno::ENOENT.value unless hex

      real_path = @lib.blob_path(hex)
      return -Errno::ENOENT.value unless File.exists?(real_path)
      File.open(real_path) do |file|
        file.seek(offset)
        file.read(buffer)
      end
    rescue
      -Errno::EIO.value
    end

    # Filesystem statistics: pass through the backing disk (the only real space
    # constraint for a read-mostly CAS), overlay the store's document count.
    def statfs(path : String) : Fuse::StatVFS | Int32
      buf = uninitialized LibC::Statvfs
      return -Errno::ENOSYS.value unless LibC.statvfs(@lib.root, pointerof(buf)) == 0
      Fuse::StatVFS.new(
        bsize: buf.f_bsize, frsize: buf.f_frsize, blocks: buf.f_blocks,
        bfree: buf.f_bfree, bavail: buf.f_bavail,
        files: @lib.documents.size.to_u64,
        ffree: buf.f_ffree, favail: buf.f_favail,
        namemax: buf.f_namemax, flag: buf.f_flag,
      )
    end
  end
end
