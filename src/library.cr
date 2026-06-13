require "./cas"
require "./claim"
require "./log"
require "./document"

module TransFS
  # The claim-log core: the operations that the CLI (and later the GUI, and the
  # mount) all go through. This is the new model (docs/architecture.md); it lives
  # alongside the legacy SQL model (store.cr/tagfs.cr) until the new path is
  # proven and the old one retired.
  class Library
    getter root : String

    def initialize(@root : String)
      @cas = CAS.new(@root)
    end

    # Archive a file as a new document (one create claim + one version claim,
    # optionally a name). Returns the folded Document.
    #
    # Ordering (§3 durability): write the content blob FIRST, then the claims
    # that reference it. A crash in between leaves an orphan blob — harmless
    # garbage in a content-addressed store, swept by GC — never a dangling ref.
    def add(filepath : String, name : String? = nil, ts : Time = Time.utc) : Document
      content = File.read(filepath).to_slice

      # 1. content first
      blob_hex = @cas.put(content)

      # 2. mint identity, then the claims (content-before-claim ordering)
      create = CreateClaim.mint(ts)
      log = Log.new(@root, create.doc_id)

      version = VersionClaim.new(hash: blob_hex, parent: nil, ts: ts)
      label = name || File.basename(filepath)
      name_claim = NameClaim.new(name: label, ts: ts)

      # one committed batch: create + version + name
      log.append(create, version, name_claim)

      Document.load(@root, create.doc_id)
    end

    # Read a document's current content bytes (head version), or nil.
    def read(doc : Document) : Bytes?
      if h = doc.head
        @cas.get(h)
      end
    end

    def documents : Array(Document)
      Document.all(@root)
    end

    # Look up a document by full id or a unique hex prefix (git-style). The
    # opaque id is never meant to be typed in full by a human; a short prefix
    # that uniquely resolves is enough. Raises on an ambiguous prefix.
    def document(id : String) : Document?
      # Fast path: a full id whose log exists.
      if Log.new(@root, id).exists?
        return Document.load(@root, id)
      end
      # Prefix resolution.
      matches = Log.all_ids(@root).select(&.starts_with?(id))
      case matches.size
      when 0 then nil
      when 1 then Document.load(@root, matches.first)
      else        raise "ambiguous id prefix '#{id}' (#{matches.size} matches)"
      end
    end
  end
end
