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
    record TornTail, path : String, line_number : Int32, reason : String

    class Corrupt < Exception
      getter path : String
      getter line_number : Int32
      getter reason : String

      def initialize(@path : String, @line_number : Int32, @reason : String)
        super("#{path}:#{line_number}: #{reason}")
      end
    end

    record ReadResult, claims : Array(Claim), torn_tail : TornTail?

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

    # Append one or more claims and fsync before returning. The interim JSON-lines
    # format can recover a torn final line, but true multi-record batch atomicity
    # waits on a record-stream commit marker.
    def append(*claims : Claim) : Nil
      Dir.mkdir_p(File.dirname(path))
      File.open(path, "a") do |f|
        claims.each { |c| f.puts c.to_json_line }
        f.flush
        f.fsync
      end
    end

    # Read and parse every claim in order. A malformed final line is tolerated as
    # a recoverable torn tail; malformed earlier lines are corruption.
    def claims : Array(Claim)
      read.claims
    end

    def read : ReadResult
      return ReadResult.new([] of Claim, nil) unless File.exists?(path)
      result = [] of Claim
      lines = File.read_lines(path)
      torn_tail = nil
      lines.each_with_index do |line, i|
        begin
          if claim = Claim.parse(line)
            result << claim
          end
        rescue ex : Claim::ParseError
          line_number = i + 1
          reason = ex.message || "malformed claim"
          if i == lines.size - 1
            torn_tail = TornTail.new(path, line_number, reason)
          else
            raise Corrupt.new(path, line_number, reason)
          end
        end
      end
      ReadResult.new(result, torn_tail)
    end

    # Every syntactically valid known claim in the log. Unlike `#read`, this
    # reports malformed lines wherever they appear instead of applying recovery.
    def scan(& : Claim | Corrupt ->) : Nil
      return unless File.exists?(path)
      File.each_line(path).with_index do |line, i|
        line_number = i + 1
        begin
          if claim = Claim.parse(line)
            yield claim
          end
        rescue ex : Claim::ParseError
          yield Corrupt.new(path, line_number, ex.message || "malformed claim")
        end
      end
    end

    def scan : Array(Claim | Corrupt)
      result = [] of Claim | Corrupt
      scan do |entry|
        result << entry
      end
      result
    end

    def self.read_all(root : String) : Array({String, ReadResult | Corrupt})
      all_ids(root).map do |id|
        log = new(root, id)
        begin
          {id, log.read.as(ReadResult | Corrupt)}
        rescue ex : Corrupt
          {id, ex.as(ReadResult | Corrupt)}
        end
      end
    end

    def self.corruptions(root : String) : Array(Corrupt)
      errors = [] of Corrupt
      read_all(root).each do |_, result|
        case result
        in ReadResult
        in Corrupt
          errors << result
        end
      end
      errors
    end

    def self.torn_tails(root : String) : Array(TornTail)
      warnings = [] of TornTail
      read_all(root).each do |_, result|
        case result
        in ReadResult
          if torn_tail = result.torn_tail
            warnings << torn_tail
          end
        in Corrupt
        end
      end
      warnings
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
