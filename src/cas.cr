require "digest/sha256"
require "random/secure"

module TransFS
  # Content-addressable blob store. Blobs live at `<root>/blobs/<hh>/<hex>`,
  # keyed on the SHA-256 of their content *alone* — no extension, no name, no
  # document id in the path (see docs/architecture.md §2). `<hh>` is the first
  # two hex chars, a 256-way fan-out.
  class CAS
    def initialize(@root : String)
    end

    def blobs_dir : String
      File.join(@root, "blobs")
    end

    # The on-disk path a hex hash resolves to — a pure function of the hash,
    # identical on every machine (the CAS "the hash *is* the address" property).
    def path_for(hex : String) : String
      File.join(blobs_dir, hex[0, 2], hex)
    end

    def exists?(hex : String) : Bool
      File.exists?(path_for(hex))
    end

    # Store *content*, returning its hex hash. Dedups: identical bytes hash to
    # the same path, so a re-store is a no-op. Writes to a temp file then
    # fsyncs + renames, so a reader never sees a half-written blob and a claim
    # is not allowed to reference content that has not been forced durable.
    def put(content : Bytes) : String
      hex = Digest::SHA256.hexdigest(content)
      path = path_for(hex)
      unless File.exists?(path)
        dir = File.dirname(path)
        Dir.mkdir_p(dir)
        fsync_dir(@root)
        fsync_dir(File.dirname(dir))
        fsync_dir(dir)
        tmp = "#{path}.tmp.#{Process.pid}.#{Random::Secure.hex(4)}"
        begin
          File.open(tmp, "w") do |file|
            file.write(content)
            file.fsync
          end
          File.rename(tmp, path)
          fsync_dir(dir)
        rescue ex
          File.delete(tmp) if File.exists?(tmp)
          raise ex
        end
      end
      hex
    end

    def get(hex : String) : Bytes?
      path = path_for(hex)
      return nil unless File.exists?(path)
      File.read(path).to_slice
    end

    private def fsync_dir(path : String) : Nil
      {% if flag?(:unix) %}
        fd = LibC.open(path, LibC::O_RDONLY)
        raise IO::Error.from_errno("Error opening directory for sync", target: path) if fd < 0
        begin
          IO::FileDescriptor.new(fd, close_on_finalize: false).fsync
        ensure
          LibC.close(fd)
        end
      {% end %}
    end
  end
end
