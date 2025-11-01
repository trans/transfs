require "crystalfuse"
require "sqlite3"

lib LibC
  fun memset(dest : Void*, c : Int32, n : SizeT) : Void*

  struct Statvfs
    f_bsize    : LibC::ULong     # Filesystem block size
    f_frsize   : LibC::ULong     # Fragment size
    f_blocks   : LibC::ULong     # Total data blocks
    f_bfree    : LibC::ULong     # Free blocks
    f_bavail   : LibC::ULong     # Free blocks for unprivileged users
    f_files    : LibC::ULong     # Total file nodes (inodes)
    f_ffree    : LibC::ULong     # Free file nodes
    f_favail   : LibC::ULong     # Free inodes for unprivileged users
    f_fsid     : LibC::ULong     # Filesystem ID
    f_flag     : LibC::ULong     # Mount flags
    f_namemax  : LibC::ULong     # Max filename length
  end
end

module TransFS

  # FuseFS
  #
  class FuseSystem < Fuse::FileSystem
    @db : DB::Database
    @mountpoint : String

    # Mount the filesystem using Fuse.
    #
    def self.mount(store)
      fs = new(store)
      p store.mountpoint
      fs.run!(["transfs", "-f", store.mountpoint])
    end

    def initialize(store : Store)
      # We could reduce this @store
      @db = store.database
      @mountpoint = store.root
      super()
      puts "HERE1"
    end

    # Get attributes.
    #
    def getattr(path : String) : LibC::Stat | Int32
      puts "getattr"
      parts = path.split('/').reject(&.empty?)

      # Initialize stat structure
      stat = zeroed_struct(LibC::Stat)

      case parts.size
      when 0
        # /
        stat.st_mode = LibC::S_IFDIR | 0o755
        stat.st_nlink = 2
        #size?
        set_mtime(stat, Time.utc.to_unix)
      when 1
        # /mp3 or /jpg
        stat.st_mode = LibC::S_IFDIR | 0o755
        stat.st_nlink = 2
        #size?
        set_mtime(stat, Time.utc.to_unix)
      when 2
        # /mp3/song.mp3
        filename = parts[1]
        size = @db.query_one?("SELECT size FROM files WHERE original_name = ?", filename, as: Int64)
        if size
          stat.st_mode = LibC::S_IFREG | 0o444
          stat.st_nlink = 1
          stat.st_size = size
          set_mtime(stat, Time.utc.to_unix)
        else
          return LibC::ENOENT
        end
      else
        return LibC::ENOENT
      end

      return stat
    end

    private def set_mtime(stat : LibC::Stat, time : Int64)
      {% if flag?(:linux) %}
        # Works on glibc-based Linux (64-bit)
        stat.st_mtim.tv_sec = time
        stat.st_mtim.tv_nsec = 0
      {% else %}
        # Works on BSD/macOS or 32-bit Linux
        stat.st_mtime = time
      {% end %}
    end

    # Opens a file at *path*.
    # **Watch out**: Return a `UInt64` for a file-handle, and return a `Int32` to return an errno error!
    def open(path) : Int32 | UInt64
      puts "open"
      parts = path.split('/').reject(&.empty?)
      return LibC::ENOENT unless parts.size == 2

      ext  = parts[0]
      name = parts[1]

      exists = @db.query_one?(
        "SELECT 1 FROM files WHERE original_name = ? AND extension = ?", name, ext, as: Int32
      )

      exists ? 0 : LibC::ENOENT
    end

    # Closes a file at *path*.  Please read more about it in
    # `Binding::Operations#release`.  Return `0` on success.
    def release(path, handle, fi) : Int32
      0
    end

    # Read path.
    def read(path, handle, buffer, offset, fi) : Bytes | Int32
      puts "read"
      parts = path.split('/').reject(&.empty?)
      return LibC::ENOENT unless parts.size == 2

      ext = parts[0]
      name = parts[1]

      row = @db.query_one?("SELECT hash FROM files WHERE original_name = ? AND extension = ?", name, ext, as: Bytes)
      return LibC::ENOENT unless row

      hash = row
      hex = hash.hexstring
      subdir = hex[0..1]
      real_path = File.join(@mountpoint, ext, subdir, hex)

      return LibC::ENOENT unless File.exists?(real_path)

      File.open(real_path) do |file|
        file.seek(offset)
        return file.read(buffer) || 0
      end
    rescue
      LibC::EIO
    end

    # Write to path.
    def write(path, handle, buffer : Bytes, offset, fi) : Int32
      0
    end

    # Opens a directory at *path*.  Analogous to `#open`
    def opendir(path) : UInt64 | Int32
      puts "opendir"
      parts = path.split('/').reject(&.empty?)

      if parts.size == 0
        # /
        return 0
      elsif parts.size == 1
        # /mp3 or /jpg
        ext = parts[0]
        exists = @db.query_one?("SELECT 1 FROM files WHERE extension = ? LIMIT 1", ext, as: Int32)
        return exists ? 0 : LibC::ENOENT
      end

      LibC::ENOENT
    end

    # Closes a directory at *path*.  Analogous to `#release`
    def releasedir(path, handle, fi) : Int32
      0
    end

    # Reads the entries of a directory.  The "." and ".." entries are added
    # automatically.  The result may be any enumerable of Strings, or a tuple
    # of a string and a `LibC::Stat`.  If the result is a integer, it's used
    # as resulting error code.
    def readdir(path, handle, offset, fi) : Enumerable(String) | Enumerable(Tuple(String, LibC::Stat)) | Int32
      puts "readdir"
      parts = path.split('/').reject(&.empty?)

      case parts.size
      when 0
        # /
        entries = [".", ".."]
        @db.query("SELECT DISTINCT extension FROM files") do |rs|
          rs.each do
            entries << rs.read(String)
          end
        end
        return entries

      when 1
        # e.g. /mp3
        ext = parts[0]
        exists = @db.query_one?("SELECT 1 FROM files WHERE extension = ? LIMIT 1", ext, as: Int32)
        return LibC::ENOENT unless exists

        entries = [".", ".."]
        @db.query("SELECT original_name FROM files WHERE extension = ?", ext) do |rs|
          rs.each do
            entries << rs.read(String)
          end
        end
        return entries

      else
        return LibC::ENOENT
      end
    end

    def statfs(path : String, stat : Pointer(LibC::Statvfs)) : Int32
      puts "statfs"
      LibC.memset(stat.as(Void*), 0, sizeof(LibC::Statvfs))

      num_files = @db.query_one?("SELECT COUNT(*) FROM files", as: Int64) || 0
      total_size = @db.query_one?("SELECT SUM(size) FROM files", as: Int64) || 0

      stat.value.f_files = num_files
      stat.value.f_blocks = (total_size + 4095) // 4096

      # TODO
      stat.value.f_bsize = 4096
      stat.value.f_frsize = 4096
      stat.value.f_bfree  = 512 * 1024   # 512MB free
      stat.value.f_bavail = 512 * 1024
      stat.value.f_ffree  = 90000
      stat.value.f_favail = 90000
      stat.value.f_fsid   = 0
      stat.value.f_flag   = 0
      stat.value.f_namemax = 255

      0 # success
    end

    #private

    # TODO: Why didn't a method using Generics work, e.g. foo(T).
    macro zeroed_struct(type)
      begin
        _buf = uninitialized {{type}}
        LibC.memset(pointerof(_buf).as(Void*), 0, sizeof({{type}}))
        _buf
      end
    end

  end # FuseFS

end # TransFS
