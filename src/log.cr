require "./claim"

module TransFS
  # The append-only claim log for one document. This is the source of truth;
  # the (future) SQLite index is a rebuildable fold of these. See §3.
  #
  # Layout: `<root>/.transfs/docs/<hh>/<hex-id>.log`, where the id is the hex of
  # the document's create-claim hash and `<hh>` is its first two hex chars — the
  # same fan-out as the blob tree.
  #
  # Durability (§3): the log is the write-ahead log; we don't journal it.
  #  - append writes the record then fsyncs before the caller acts on it;
  #  - a torn trailing line (crash mid-append) fails to parse and is skipped;
  #  - content blobs are always written *before* the claim that references them,
  #    so a crash leaves only harmless orphan blobs (GC sweeps them), never a
  #    dangling reference.
  class Log
    getter id : String

    def initialize(@root : String, @id : String)
    end

    def self.docs_dir(root : String) : String
      File.join(root, ".transfs", "docs")
    end

    def path : String
      File.join(Log.docs_dir(@root), @id[0, 2], "#{@id}.log")
    end

    def exists? : Bool
      File.exists?(path)
    end

    # Append one or more claims as a single committed unit. (Batch atomicity:
    # all the lines, then one fsync — either the batch is durable or, after a
    # torn write, the unparseable tail is skipped on replay.)
    def append(*claims : Claim) : Nil
      Dir.mkdir_p(File.dirname(path))
      File.open(path, "a") do |f|
        claims.each { |c| f.puts c.to_json_line }
        f.flush
        f.fsync
      end
    end

    # Read and parse every claim in order, skipping blank/unparseable lines
    # (the torn-tail rule). Returns [] if the log doesn't exist.
    def claims : Array(Claim)
      return [] of Claim unless File.exists?(path)
      result = [] of Claim
      File.each_line(path) do |line|
        if claim = Claim.parse(line)
          result << claim
        end
      end
      result
    end

    # Every document id present in the store (by walking the docs tree).
    def self.all_ids(root : String) : Array(String)
      dir = docs_dir(root)
      return [] of String unless Dir.exists?(dir)
      ids = [] of String
      Dir.glob(File.join(dir, "*", "*.log")) do |p|
        ids << File.basename(p, ".log")
      end
      ids
    end
  end
end
