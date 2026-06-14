require "./cas"
require "./claim"
require "./log"
require "./document"
require "./index"

module TransFS
  # The claim-log core: the operations that the CLI (and later the GUI, and the
  # mount) all go through. This is the new model (docs/architecture.md); it lives
  # alongside the legacy SQL model (store.cr/tagfs.cr) until the new path is
  # proven and the old one retired.
  class Library
    getter root : String
    @index : Index?

    def initialize(@root : String)
      @cas = CAS.new(@root)
      @index = nil
    end

    # The materialized facet index. Lazily opened (and self-rebuilt if missing),
    # so pure log operations don't pay for it unless a query needs it.
    def index : Index
      @index ||= Index.new(@root)
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

      reindex(Document.load(@root, create.doc_id))
    end

    # Archive a new version of an existing document from a file. Content-first
    # ordering as in `add`; the new version's parent is the document's current
    # head (so versions form a fork-detectable DAG, §3).
    def add_version(doc : Document, filepath : String, ts : Time = Time.utc) : Document
      content = File.read(filepath).to_slice
      blob_hex = @cas.put(content)
      Log.new(@root, doc.id).append(
        VersionClaim.new(hash: blob_hex, parent: doc.head, ts: ts)
      )
      reindex(Document.load(@root, doc.id))
    end

    # Add and/or remove tags (a single tag claim). A no-op if both lists empty.
    def tag(doc : Document, add : Array(String) = [] of String,
            del : Array(String) = [] of String, ts : Time = Time.utc) : Document
      return doc if add.empty? && del.empty?
      Log.new(@root, doc.id).append(TagClaim.new(add: add, del: del, ts: ts))
      reindex(Document.load(@root, doc.id))
    end

    # Set the document's blessed label (a name claim; latest wins on fold).
    def rename(doc : Document, name : String, ts : Time = Time.utc) : Document
      Log.new(@root, doc.id).append(NameClaim.new(name: name, ts: ts))
      reindex(Document.load(@root, doc.id))
    end

    # Write the index row for one document through after every mutation, so the
    # persistent index stays fresh across separate CLI processes (each command
    # is its own process; a mutation that didn't write through would leave the
    # next query stale). Opening the index is cheap; if index.db is missing it
    # rebuilds itself from the logs first, then this upsert is idempotent.
    private def reindex(doc : Document) : Document
      index.index_document(doc)
      doc
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
