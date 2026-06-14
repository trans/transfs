require "crystalfuse"
require "mime"
require "./library"

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
  # A read-only FUSE view over the claim-log store (the NEW model). This is the
  # *minimal boring port* (slice 4): it presents documents grouped by MIME major
  # type — `/<type>/<display-name>` — sourced from the claim logs via `Library`,
  # not the legacy SQL `files` table.
  #
  # This `/<type>/` view is deliberately a STOPGAP. The real design is a
  # composable query-path mount (`facet:value` segments, AND/OR/NOT, recognition
  # navigation, version addressing) — a separate design pass, NOT built here.
  # Don't grow this scheme; it will be replaced wholesale. See docs/architecture.md
  # §7 and the "deferred — query-path mount" note in §9.
  #
  # Read-only and honest about it: write ops return -EROFS (the structural
  # enforcement of "don't edit through the mount"; mutation is via the CLI).
  class FuseSystem < Fuse::FileSystem
    def initialize(@lib : Library)
      super()
    end

    # Mount over *root* at *mountpoint*, foreground.
    def self.mount(root : String, mountpoint : String)
      new(Library.new(root)).mount(["transfs", "-f", mountpoint])
    end

    # MIME major type ("image", "application", "text", …) used as the bucket.
    # Untyped documents (flat label, no extension) go under "other".
    private def type_bucket(name : String?) : String
      mime = name ? MIME.from_filename?(name) : nil
      mime ? mime.split('/', 2).first : "other"
    end

    # All (bucket, display_name, document) triples. The display name disambiguates
    # collisions within a bucket by appending a short id suffix — a minimal,
    # stable, openable rule (NOT the design's computed recognition name, which is
    # a later slice). Same name+bucket twice => "report.pdf" and "report~a1b2.pdf".
    private def entries : Array({String, String, Document})
      docs = @lib.documents
      # group by (bucket, name) to detect collisions
      seen = Hash({String, String?}, Int32).new(0)
      docs.each { |d| seen[{type_bucket(d.name), d.name}] += 1 }

      docs.map do |d|
        bucket = type_bucket(d.name)
        base = d.name || "untitled-#{d.id[0, 8]}"
        display =
          if seen[{bucket, d.name}] > 1
            disambiguate(base, d.id)
          else
            base
          end
        {bucket, display, d}
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

    private def buckets : Array(String)
      entries.map { |(b, _, _)| b }.uniq!.sort!
    end

    # Resolve a "/<bucket>/<display>" path to its document, or nil.
    private def resolve(bucket : String, display : String) : Document?
      entries.find { |(b, name, _)| b == bucket && name == display }.try { |t| t[2] }
    end

    def getattr(path : String) : Fuse::FileAttr | Int32
      parts = path.split('/').reject(&.empty?)
      case parts.size
      when 0 # /
        Fuse::FileAttr.dir
      when 1 # /<bucket>
        buckets.includes?(parts[0]) ? Fuse::FileAttr.dir : -Errno::ENOENT.value
      when 2 # /<bucket>/<display>
        doc = resolve(parts[0], parts[1])
        return -Errno::ENOENT.value unless doc
        size = head_size(doc)
        return -Errno::ENOENT.value unless size
        Fuse::FileAttr.file(size: size, mode: 0o444)
      else
        -Errno::ENOENT.value
      end
    end

    def readdir(path : String) : Array(String) | Int32
      parts = path.split('/').reject(&.empty?)
      case parts.size
      when 0 # / -> the type buckets
        [".", ".."] + buckets
      when 1 # /<bucket> -> its documents
        bucket = parts[0]
        return -Errno::ENOENT.value unless buckets.includes?(bucket)
        names = entries.select { |(b, _, _)| b == bucket }.map { |(_, n, _)| n }
        [".", ".."] + names
      else
        -Errno::ENOENT.value
      end
    end

    def open(path : String) : Int32
      parts = path.split('/').reject(&.empty?)
      return -Errno::ENOENT.value unless parts.size == 2
      resolve(parts[0], parts[1]) ? 0 : -Errno::ENOENT.value
    end

    # Read by filling the kernel's own buffer directly (the zero-copy escape
    # hatch), streaming bytes from the document's head blob.
    def read(path : String, buffer : Bytes, offset : Int64, fi : Fuse::FileInfo) : Int32
      parts = path.split('/').reject(&.empty?)
      return -Errno::ENOENT.value unless parts.size == 2
      doc = resolve(parts[0], parts[1])
      return -Errno::ENOENT.value unless doc
      hex = doc.head
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

    private def head_size(doc : Document) : Int64?
      hex = doc.head
      return nil unless hex
      path = @lib.blob_path(hex)
      File.exists?(path) ? File.size(path).to_i64 : nil
    end
  end
end
