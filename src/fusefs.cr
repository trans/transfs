require "crystalfuse"
require "sqlite3"

# Local short alias for this project's comfort. The library's own short name is
# `Crystalfuse::FS`; this just lets us write `Fuse::...`.
alias Fuse = Crystalfuse

module TransFS

  # A read-only FUSE view over the content-addressable store. The mount presents
  # files grouped by extension: `/<ext>/<original_name>`, with the bytes served
  # from the CAS layout `<root>/<ext>/<2-hex>/<full-hex>`.
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
      real_path = File.join(@root, ext, hex[0..1], hex)
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

    # Filesystem statistics, derived from the store's contents.
    def statfs(path : String) : Fuse::StatVFS | Int32
      num_files = @db.query_one?("SELECT COUNT(*) FROM files", as: Int64) || 0_i64
      total_size = @db.query_one?("SELECT SUM(size) FROM files", as: Int64) || 0_i64

      Fuse::StatVFS.new(
        bsize: 4096_u64,
        frsize: 4096_u64,
        blocks: ((total_size + 4095) // 4096).to_u64,
        bfree: (512_u64 * 1024),   # 512MB free (placeholder)
        bavail: (512_u64 * 1024),
        files: num_files.to_u64,
        ffree: 90_000_u64,
        favail: 90_000_u64,
        namemax: 255_u64,
      )
    end

  end # FuseSystem

end # TransFS
