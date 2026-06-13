require "crystalfuse"
require "sqlite3"

# `statvfs(2)` isn't bound by Crystal's stdlib or crystalfuse, so bind it here.
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

# Local short alias for this project's comfort. The library's own short name is
# `Crystalfuse::FS`; this just lets us write `Fuse::...`.
alias Fuse = Crystalfuse

module TransFS

  # A read-only FUSE view over the content-addressable store. The mount presently
  # presents files grouped by extension (`/<ext>/<original_name>`) — an interim
  # presentation scheme; the design calls for synthetic query views (`/by-tag/`,
  # `/by-type/`, …). Bytes are served from the pure-CAS layout
  # `<root>/blobs/<2-hex>/<full-hex>` (keyed on the content hash alone).
  #
  # Writing through the mount isn't supported yet (files arrive via `transfs
  # add`), so the write-side operations are left at the library's `-ENOSYS`
  # defaults and files are exposed mode 0o444.
  class FuseSystem < Fuse::FileSystem
    @db : DB::Database
    @root : String

    # Mount the filesystem using Fuse.
    def self.mount(store)
      fs = new(store)
      fs.mount(["transfs", "-f", store.mountpoint])
    end

    def initialize(store : Store)
      @db = store.database
      @root = store.root
      super()
    end

    # Get attributes for a path: the root, an extension directory, or a file.
    def getattr(path : String) : Fuse::FileAttr | Int32
      parts = path.split('/').reject(&.empty?)

      case parts.size
      when 0 # /
        Fuse::FileAttr.dir
      when 1 # /mp3, /jpg, …
        Fuse::FileAttr.dir
      when 2 # /mp3/song.mp3
        ext, name = parts
        size = @db.query_one?(
          "SELECT size FROM files WHERE original_name = ? AND extension = ?",
          name, ext, as: Int64
        )
        size ? Fuse::FileAttr.file(size: size, mode: 0o444) : -Errno::ENOENT.value
      else
        -Errno::ENOENT.value
      end
    end

    # Open a file: succeed only if it exists in the store.
    def open(path : String) : Int32
      parts = path.split('/').reject(&.empty?)
      return -Errno::ENOENT.value unless parts.size == 2

      ext, name = parts
      exists = @db.query_one?(
        "SELECT 1 FROM files WHERE original_name = ? AND extension = ?", name, ext, as: Int32
      )
      exists ? 0 : -Errno::ENOENT.value
    end

    # Read by filling the kernel's own buffer directly (the zero-copy escape
    # hatch), streaming the bytes from the backing CAS object.
    def read(path : String, buffer : Bytes, offset : Int64, fi : Fuse::FileInfo) : Int32
      parts = path.split('/').reject(&.empty?)
      return -Errno::ENOENT.value unless parts.size == 2

      ext, name = parts
      hash = @db.query_one?(
        "SELECT hash FROM files WHERE original_name = ? AND extension = ?", name, ext, as: Bytes
      )
      return -Errno::ENOENT.value unless hash

      hex = hash.hexstring
      # Pure-CAS blob path, keyed on the hash alone (no <ext>). Must match the
      # writer in tagfs.cr. See docs/architecture.md §2.
      real_path = File.join(@root, "blobs", hex[0..1], hex)
      return -Errno::ENOENT.value unless File.exists?(real_path)

      File.open(real_path) do |file|
        file.seek(offset)
        file.read(buffer)
      end
    rescue
      -Errno::EIO.value
    end

    # List extension directories at the root, or the files within one. Include
    # "." and ".." as the library expects.
    def readdir(path : String) : Array(String) | Int32
      parts = path.split('/').reject(&.empty?)

      case parts.size
      when 0 # /
        entries = [".", ".."]
        @db.query("SELECT DISTINCT extension FROM files") do |rs|
          rs.each { entries << rs.read(String) }
        end
        entries
      when 1 # /mp3
        ext = parts[0]
        exists = @db.query_one?("SELECT 1 FROM files WHERE extension = ? LIMIT 1", ext, as: Int32)
        return -Errno::ENOENT.value unless exists

        entries = [".", ".."]
        @db.query("SELECT original_name FROM files WHERE extension = ?", ext) do |rs|
          rs.each { entries << rs.read(String) }
        end
        entries
      else
        -Errno::ENOENT.value
      end
    end

    # Filesystem statistics. For a read-only CAS view the only real constraint
    # is the disk the store sits on, so pass through the backing filesystem's
    # own `statvfs` for the block/space figures. Inode counts are overlaid with
    # the store's own content (files tracked, not the backing fs's inodes).
    def statfs(path : String) : Fuse::StatVFS | Int32
      buf = uninitialized LibC::Statvfs
      return -Errno::ENOSYS.value unless LibC.statvfs(@root, pointerof(buf)) == 0

      num_files = @db.query_one?("SELECT COUNT(*) FROM files", as: Int64) || 0_i64

      Fuse::StatVFS.new(
        bsize: buf.f_bsize,
        frsize: buf.f_frsize,
        blocks: buf.f_blocks,
        bfree: buf.f_bfree,
        bavail: buf.f_bavail,
        files: num_files.to_u64,
        ffree: buf.f_ffree,
        favail: buf.f_favail,
        namemax: buf.f_namemax,
        flag: buf.f_flag,
      )
    end

  end # FuseSystem

end # TransFS
