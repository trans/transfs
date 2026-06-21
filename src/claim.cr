require "digest/sha256"
require "json"
require "random/secure"

module TransFS
  # A claim is one timestamped, append-only mutation record in a document's log.
  # State = fold the claims in order. See docs/architecture.md §3.
  #
  # Interim encoding is JSON, one object per line (newline-framed; an unparseable
  # trailing line is skipped on replay — the torn-tail rule). This will become
  # C0DATA later with no change to the model: the encoding sits behind this
  # module's serialize/parse, and identity is hashed from canonical VALUES (never
  # from the serialized bytes), so the swap can't shift a single id.
  abstract struct Claim
    getter ts : Time

    def initialize(@ts : Time)
    end

    # The op tag stored as the record's first field.
    abstract def op : String

    # The canonical value bytes that *define* this claim, independent of the
    # storage encoding. (Only `create` is hashed — for identity — but every
    # claim defines it for uniformity and future content-addressing.)
    abstract def canonical : Bytes

    # ISO-8601 with nanosecond precision (see §3): a real standard, and combined
    # with the create nonce, collision-free.
    def self.format_ts(t : Time) : String
      t.to_utc.to_rfc3339(fraction_digits: 9)
    end

    # --- JSON-line codec (the interim encoding) ---

    def to_json_line : String
      JSON.build do |j|
        j.object do
          j.field "op", op
          emit_fields(j)
          j.field "ts", Claim.format_ts(ts)
        end
      end
    end

    # Subclasses add their own fields between "op" and "ts".
    protected abstract def emit_fields(j : JSON::Builder)

    class ParseError < Exception
    end

    # Parse one log line into a claim. Returns nil only for blank lines or valid
    # unknown future ops; malformed records raise so the log reader can tolerate
    # only a torn final record.
    def self.parse(line : String) : Claim?
      line = line.strip
      return nil if line.empty?
      obj = JSON.parse(line).as_h
      ts = Time.parse_rfc3339(obj["ts"].as_s)
      case obj["op"].as_s
      when "create"
        CreateClaim.new(nonce: obj["nonce"].as_s, ts: ts)
      when "version"
        parent = obj["parent"]?.try(&.as_s?)
        VersionClaim.new(hash: obj["hash"].as_s, parent: parent, ts: ts)
      when "name"
        NameClaim.new(name: obj["name"].as_s, ts: ts)
      when "tag"
        add = obj["add"]?.try(&.as_a.map(&.as_s)) || [] of String
        del = obj["del"]?.try(&.as_a.map(&.as_s)) || [] of String
        TagClaim.new(add: add, del: del, ts: ts)
      else
        nil # unknown op: ignore forward-compatibly
      end
    rescue ex : JSON::ParseException | KeyError | Time::Format::Error | TypeCastError
      raise ParseError.new(ex.message || ex.class.name)
    end
  end

  # The id-less root. `sha256(canonical(ts, nonce))` IS the document id. Content-
  # free on purpose, so identity is independent of any content that flows through
  # the document. The nonce makes the id deterministic-yet-unique without needing
  # clock resolution to guarantee uniqueness.
  struct CreateClaim < Claim
    getter nonce : String

    def initialize(@nonce : String, @ts : Time)
    end

    # Mint a fresh document: random nonce + now.
    def self.mint(ts : Time) : CreateClaim
      new(nonce: Random::Secure.hex(16), ts: ts)
    end

    def op : String
      "create"
    end

    # The document id: hex SHA-256 of the canonical values. NOT of the JSON line.
    def doc_id : String
      Digest::SHA256.hexdigest(canonical)
    end

    def canonical : Bytes
      # Defined byte layout, encoding-independent: "create" ␟ ts ␟ nonce.
      String.build do |s|
        s << "create\x1f" << Claim.format_ts(ts) << "\x1f" << nonce
      end.to_slice
    end

    protected def emit_fields(j : JSON::Builder)
      j.field "nonce", nonce
    end
  end

  # A new content state. Content-only (name/tags are document-level claims).
  # `parent` = the content hash this derived from (nil for the first) — explicit,
  # so versions form a fork-detectable DAG. `hash`/`parent` are hex (§3).
  struct VersionClaim < Claim
    getter hash : String
    getter parent : String?

    def initialize(@hash : String, @parent : String?, @ts : Time)
    end

    def op : String
      "version"
    end

    def canonical : Bytes
      String.build do |s|
        s << "version\x1f" << hash << "\x1f" << (parent || "") << "\x1f" << Claim.format_ts(ts)
      end.to_slice
    end

    protected def emit_fields(j : JSON::Builder)
      j.field "hash", hash
      j.field "parent", parent
    end
  end

  # A flat, blessed label. Current name = latest name claim.
  struct NameClaim < Claim
    getter name : String

    def initialize(@name : String, @ts : Time)
    end

    def op : String
      "name"
    end

    def canonical : Bytes
      String.build { |s| s << "name\x1f" << name << "\x1f" << Claim.format_ts(ts) }.to_slice
    end

    protected def emit_fields(j : JSON::Builder)
      j.field "name", name
    end
  end

  # Tag mutation. Both add and del allowed in one claim; values are opaque
  # strings (the key=value convention is parsed at index time, not here).
  struct TagClaim < Claim
    getter add : Array(String)
    getter del : Array(String)

    def initialize(@add : Array(String), @del : Array(String), @ts : Time)
    end

    def op : String
      "tag"
    end

    def canonical : Bytes
      String.build do |s|
        s << "tag\x1f" << add.join(",") << "\x1f" << del.join(",") << "\x1f" << Claim.format_ts(ts)
      end.to_slice
    end

    protected def emit_fields(j : JSON::Builder)
      j.field "add", add unless add.empty?
      j.field "del", del unless del.empty?
    end
  end
end
