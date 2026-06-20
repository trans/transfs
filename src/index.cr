require "db"
require "sqlite3"
require "mime"
require "set"
require "./cas"
require "./document"

module TransFS
  # The SQLite index: a *materialized facet view* folded from the claim logs
  # (docs/architecture.md §6). It is a rebuildable cache, never the source of
  # truth — delete it and the next open rebuilds it from the logs.
  #
  # Two deliberate deviations from the §6 DDL, both because this is a disposable
  # cache, not the truth layer:
  #   * ids/hashes are stored as **hex TEXT**, not BLOB — debuggable with the
  #     sqlite3 CLI, no hex<->bytes conversion at every boundary, and the whole
  #     codebase already speaks hex. 32 bytes/row is irrelevant for a personal
  #     archive's metadata.
  #   * the db lives at `<root>/.transfs/index.db` (the §6 DDL sketch wrote
  #     `<root>/files.db`).
  class Index
    @db : DB::Database
    @cas : CAS

    def initialize(@root : String)
      @cas = CAS.new(@root)
      existed = File.exists?(db_path)
      Dir.mkdir_p(File.dirname(db_path))
      @db = DB.open("sqlite3://#{db_path}")
      ensure_schema
      rebuild unless existed # a missing db rebuilds itself from the logs
    end

    def db_path : String
      File.join(@root, ".transfs", "index.db")
    end

    def close
      @db.close
    end

    private def ensure_schema
      @db.exec <<-SQL
        CREATE TABLE IF NOT EXISTS documents (
          id            TEXT PRIMARY KEY,
          head_hash     TEXT,
          name          TEXT,
          type          TEXT,
          size          INTEGER,
          is_collection INTEGER NOT NULL DEFAULT 0,
          owner         TEXT NOT NULL DEFAULT 'local',
          source        TEXT,
          date_added    TEXT NOT NULL,
          date_content  TEXT,
          version_count INTEGER NOT NULL DEFAULT 0
        )
      SQL
      @db.exec <<-SQL
        CREATE TABLE IF NOT EXISTS versions (
          doc_id TEXT NOT NULL,
          hash   TEXT NOT NULL,
          parent TEXT,
          seq    INTEGER NOT NULL,
          ts     TEXT NOT NULL,
          size   INTEGER,
          type   TEXT,
          PRIMARY KEY (doc_id, seq)
        )
      SQL
      # Tags are stored as full hierarchical PATH strings (docs/architecture.md §7
      # "tags as paths"): a kv-tag `stars=4` is `stars/4`, a boolean tag is bucketed
      # under `tag/` (`tag/vacation`), and derived facets ride here too
      # (`type/image/jpeg`, `owner/local`). The mount is a prefix-walk over these.
      @db.exec <<-SQL
        CREATE TABLE IF NOT EXISTS doc_tags (
          doc_id TEXT NOT NULL,
          path   TEXT NOT NULL,
          PRIMARY KEY (doc_id, path)
        )
      SQL
      # membership: created for schema completeness; populated once composites
      # exist (no manifests yet).
      @db.exec <<-SQL
        CREATE TABLE IF NOT EXISTS membership (
          coll_id    TEXT NOT NULL,
          member_ref TEXT NOT NULL,
          name       TEXT,
          kind       TEXT NOT NULL
        )
      SQL
      @db.exec <<-SQL
        CREATE TABLE IF NOT EXISTS blob_refs (
          blob_hash TEXT NOT NULL,
          referrer  TEXT NOT NULL,
          PRIMARY KEY (blob_hash, referrer)
        )
      SQL
      @db.exec "CREATE INDEX IF NOT EXISTS idx_documents_name_type ON documents(name, type)"
      @db.exec "CREATE INDEX IF NOT EXISTS idx_doc_tags_path ON doc_tags(path)"
      @db.exec "CREATE INDEX IF NOT EXISTS idx_versions_hash ON versions(hash)"
    end

    # Full rebuild from the logs — the disposable-cache guarantee made real.
    def rebuild : Nil
      @db.exec "DELETE FROM documents"
      @db.exec "DELETE FROM versions"
      @db.exec "DELETE FROM doc_tags"
      @db.exec "DELETE FROM membership"
      @db.exec "DELETE FROM blob_refs"
      @db.transaction do
        Document.all(@root).each { |doc| upsert(doc) }
      end
    end

    # Materialize one document's facet rows (delete-then-insert so it's
    # idempotent — safe to call after every mutating op to keep the index fresh).
    def index_document(doc : Document) : Nil
      @db.transaction { upsert(doc) }
    end

    private def upsert(doc : Document) : Nil
      delete_rows(doc.id)

      head = doc.head
      head_size = head ? blob_size(head) : nil
      head_type = type_for(doc.name)

      @db.exec(
        "INSERT INTO documents (id, head_hash, name, type, size, is_collection, " \
        "owner, source, date_added, date_content, version_count) " \
        "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        doc.id, head, doc.name, head_type, head_size, 0,
        "local", nil, ts_str(doc.created_at), ts_str(doc.versions.last?.try(&.ts)),
        doc.version_count
      )

      doc.versions.each_with_index do |v, i|
        @db.exec(
          "INSERT INTO versions (doc_id, hash, parent, seq, ts, size, type) " \
          "VALUES (?, ?, ?, ?, ?, ?, ?)",
          doc.id, v.hash, v.parent, i, Claim.format_ts(v.ts), blob_size(v.hash), head_type
        )
        @db.exec(
          "INSERT OR IGNORE INTO blob_refs (blob_hash, referrer) VALUES (?, ?)",
          v.hash, doc.id
        )
      end

      facet_paths(doc).each do |p|
        @db.exec("INSERT OR IGNORE INTO doc_tags (doc_id, path) VALUES (?, ?)", doc.id, p)
      end
    end

    # All of a document's facets as hierarchical path strings (§7). User tags are
    # normalized (`=` is an alias for `/`); a single-component tag is a boolean and
    # is bucketed under `tag/`; derived facets (type, owner) ride here too so the
    # prefix-walk is uniform over user + derived facets.
    private def facet_paths(doc : Document) : Array(String)
      paths = [] of String
      if t = type_for(doc.name) # e.g. "image/jpeg" -> "type/image/jpeg"
        paths << "type/#{t}"
      end
      paths << "owner/local" # owner column default until ownership lands
      doc.tags.each do |tag|
        p = tag.gsub('=', '/')                 # `=` aliases `/`
        p = "tag/#{p}" unless p.includes?('/') # a boolean tag -> the `tag/` bucket
        paths << p
      end
      paths.uniq
    end

    private def delete_rows(id : String) : Nil
      @db.exec("DELETE FROM documents WHERE id = ?", id)        # PK is `id`
      @db.exec("DELETE FROM versions  WHERE doc_id = ?", id)
      @db.exec("DELETE FROM doc_tags  WHERE doc_id = ?", id)
      @db.exec("DELETE FROM membership WHERE coll_id = ?", id)  # keyed by collection
      @db.exec("DELETE FROM blob_refs WHERE referrer = ?", id)
    end

    # --- queries (read side) ---

    # A lightweight row for listing/finding — facets without re-folding logs.
    record Row, id : String, name : String?, type : String?, size : Int64?,
      version_count : Int32, date_added : String, tags : Array(String)

    # Like Row, but carries head_hash so the query-path mount can serve a leaf's
    # bytes index-only (no log re-fold in getattr/read). Returned by #match.
    record Row2, id : String, name : String?, type : String?, size : Int64?,
      version_count : Int32, date_added : String, head_hash : String?,
      tags : Array(String)

    # The parsed tag-structure of a mount path (§7 prefix-walk): a list of
    # completed tag-paths (each an AND constraint) plus the trailing `partial`
    # prefix (the current drill anchor). `valid` is false if a segment named no
    # real tag-prefix (an unknown path component).
    record Walk, constraints : Array(String), partial : String, valid : Bool

    def all : Array(Row)
      query_rows("SELECT id, name, type, size, version_count, date_added " \
                 "FROM documents ORDER BY date_added")
    end

    # CLI `find tag:<key>` — docs having `key` as any component of a tag-path.
    def by_tag(key : String) : Array(Row)
      k = escape_like(key)
      query_rows(
        "SELECT d.id, d.name, d.type, d.size, d.version_count, d.date_added " \
        "FROM documents d WHERE EXISTS (SELECT 1 FROM doc_tags t WHERE t.doc_id = d.id " \
        "AND (t.path = ? OR t.path LIKE ? ESCAPE '\\' OR t.path LIKE ? ESCAPE '\\' " \
        "OR t.path LIKE ? ESCAPE '\\')) ORDER BY d.date_added",
        key, "#{k}/%", "%/#{k}", "%/#{k}/%")
    end

    def by_type(prefix : String) : Array(Row)
      query_rows(
        "SELECT id, name, type, size, version_count, date_added FROM documents " \
        "WHERE type LIKE ? ORDER BY date_added", "#{prefix}%")
    end

    def by_name(substr : String) : Array(Row)
      query_rows(
        "SELECT id, name, type, size, version_count, date_added FROM documents " \
        "WHERE name LIKE ? ORDER BY date_added", "%#{substr}%")
    end

    # The collision neighborhood (§7): documents sharing a name (and type) — the
    # stable set against which a minimal distinguishing description is computed.
    # Exposed now; the description-rendering UX is a later slice.
    def neighborhood(name : String, type : String?) : Array(Row)
      if type
        query_rows("SELECT id, name, type, size, version_count, date_added " \
                   "FROM documents WHERE name = ? AND type = ?", name, type)
      else
        query_rows("SELECT id, name, type, size, version_count, date_added " \
                   "FROM documents WHERE name = ?", name)
      end
    end

    # --- the query-path prefix-walk (§7 "tags as paths") ---

    # Does any tag-path equal `prefix` or extend it (`prefix/...`)? Drives the
    # walk's boundary detection: a segment extends the current partial iff this is
    # true; otherwise the partial was a completed leaf and a new tag begins.
    def tag_prefix_exists?(prefix : String) : Bool
      return true if prefix.empty?
      found = false
      @db.query("SELECT 1 FROM doc_tags WHERE path = ? OR path LIKE ? ESCAPE '\\' LIMIT 1",
        prefix, "#{escape_like(prefix)}/%") do |rs|
        rs.each { found = true }
      end
      found
    end

    # Does a tag-path equal `path` exactly (a completed tag, not just a prefix)?
    def tag_complete?(path : String) : Bool
      return false if path.empty?
      found = false
      @db.query("SELECT 1 FROM doc_tags WHERE path = ? LIMIT 1", path) do |rs|
        rs.each { found = true }
      end
      found
    end

    # Fold path components into a Walk: greedily extend the current partial while
    # it stays a real tag-prefix; a component that can't extend it starts a fresh
    # tag (the old partial becomes an AND constraint). Crucially, when the partial
    # reaches a COMPLETE tag it is committed as a constraint and reset to empty —
    # so the next position enumerates the docs' *other* keys (co-facets), not just
    # the (nonexistent) children of the completed leaf (§7: "P' is a stored tag =>
    # reset").
    def walk(components : Array(String)) : Walk
      constraints = [] of String
      partial = ""
      valid = true
      components.each do |c|
        cand = partial.empty? ? c : "#{partial}/#{c}"
        if tag_prefix_exists?(cand)
          partial = cand
        else
          constraints << partial unless partial.empty?
          partial = c
          valid = false unless tag_prefix_exists?(c)
        end
        if !partial.empty? && tag_complete?(partial)
          constraints << partial
          partial = ""
        end
      end
      Walk.new(constraints, partial, valid)
    end

    # The documents matching a walk (S), newest first. Empty walk => all docs.
    # `limit` is optional and currently unused by the mount (no paging yet — it
    # lists everything); kept for when DirFiller-based paging lands (§7 scale).
    def docs(walk : Walk, limit : Int32? = nil) : Array(Row2)
      where, args = where_for(walk)
      sql = String.build do |s|
        s << "SELECT d.id, d.name, d.type, d.size, d.version_count, d.date_added, " \
             "d.head_hash FROM documents d" << where << " ORDER BY d.date_added DESC"
        if limit
          s << " LIMIT ?"
          args << limit.to_i64
        end
      end
      query_rows2(sql, args)
    end

    # The facet entries at a walk position: the distinct next path-component among
    # S's tag-paths with prefix `partial`, kept only if it would actually narrow S
    # (some doc lacks it, or it branches further) — the §7 appearance rule.
    def facets(walk : Walk) : Array(String)
      ids = doc_ids(walk)
      return [] of String if ids.empty?
      total = ids.size
      p = walk.partial
      depth = p.empty? ? 0 : p.split('/').size
      docs_by_comp = Hash(String, Set(String)).new
      deeper_by_comp = Hash(String, Set(String)).new
      each_tag_path(ids, p) do |doc_id, path|
        comps = path.split('/')
        next if comps.size <= depth # path == partial exactly: a leaf here, no child
        c = comps[depth]
        (docs_by_comp[c] ||= Set(String).new) << doc_id
        if deeper = comps[depth + 1]?
          (deeper_by_comp[c] ||= Set(String).new) << deeper
        end
      end
      all_comps = docs_by_comp.keys
      splitting = all_comps.select do |c|
        docs_by_comp[c].size < total || (deeper_by_comp[c]?.try(&.size) || 0) > 1
      end
      # Fallback: never hide *everything* — if nothing splits (e.g. a key whose one
      # value is universal), show the candidates rather than an empty listing.
      (splitting.empty? ? all_comps : splitting).sort
    end

    # WHERE clause (+ bound args) selecting docs that satisfy every constraint and
    # the trailing partial — each an EXISTS over a tag-path prefix.
    private def where_for(walk : Walk) : {String, Array(DB::Any)}
      prefixes = walk.constraints.dup
      prefixes << walk.partial unless walk.partial.empty?
      return {"", [] of DB::Any} if prefixes.empty?
      clauses = [] of String
      args = [] of DB::Any
      prefixes.each do |pre|
        clauses << "EXISTS (SELECT 1 FROM doc_tags t WHERE t.doc_id = d.id " \
                   "AND (t.path = ? OR t.path LIKE ? ESCAPE '\\'))"
        args << pre << "#{escape_like(pre)}/%"
      end
      {" WHERE " + clauses.join(" AND "), args}
    end

    private def doc_ids(walk : Walk) : Array(String)
      where, args = where_for(walk)
      ids = [] of String
      @db.query("SELECT d.id FROM documents d#{where}", args: args) do |rs|
        rs.each { ids << rs.read(String) }
      end
      ids
    end

    # Yield (doc_id, path) for the given docs whose tag-path has prefix `prefix`
    # (empty prefix => all their tag-paths).
    private def each_tag_path(ids : Array(String), prefix : String, &)
      return if ids.empty?
      placeholders = Array.new(ids.size, "?").join(", ")
      args = ids.map { |i| i.as(DB::Any) }
      cond = ""
      unless prefix.empty?
        cond = " AND (path = ? OR path LIKE ? ESCAPE '\\')"
        args << prefix << "#{escape_like(prefix)}/%"
      end
      @db.query("SELECT doc_id, path FROM doc_tags WHERE doc_id IN (#{placeholders})#{cond}",
        args: args) do |rs|
        rs.each { yield rs.read(String), rs.read(String) }
      end
    end

    private def query_rows(sql : String, *args) : Array(Row)
      rows = [] of Row
      @db.query(sql, *args) do |rs|
        rs.each do
          id = rs.read(String)
          name = rs.read(String?)
          type = rs.read(String?)
          size = rs.read(Int64?)
          vc = rs.read(Int32 | Int64).to_i32
          date = rs.read(String)
          rows << Row.new(id, name, type, size, vc, date, tags_for(id))
        end
      end
      rows
    end

    private def query_rows2(sql : String, args : Array(DB::Any)) : Array(Row2)
      rows = [] of Row2
      @db.query(sql, args: args) do |rs|
        rs.each do
          id = rs.read(String)
          name = rs.read(String?)
          type = rs.read(String?)
          size = rs.read(Int64?)
          vc = rs.read(Int32 | Int64).to_i32
          date = rs.read(String)
          head = rs.read(String?)
          rows << Row2.new(id, name, type, size, vc, date, head, tags_for(id))
        end
      end
      rows
    end

    private def tags_for(id : String) : Array(String)
      out = [] of String
      @db.query("SELECT path FROM doc_tags WHERE doc_id = ? ORDER BY path", id) do |rs|
        rs.each { out << rs.read(String) }
      end
      out
    end

    # --- helpers ---

    # Escape LIKE metacharacters so a tag-path used as a prefix matches literally
    # (the queries pair this with `ESCAPE '\'`).
    private def escape_like(s : String) : String
      s.gsub('\\', "\\\\").gsub('%', "\\%").gsub('_', "\\_")
    end

    private def blob_size(hex : String) : Int64?
      path = @cas.path_for(hex)
      File.exists?(path) ? File.size(path).to_i64 : nil
    end

    # Interim type derivation from the name's extension. Real content-sniffing
    # (a true content fact, §3) is a later refinement; this keeps /by-type
    # queries working in the meantime.
    private def type_for(name : String?) : String?
      return nil unless name
      MIME.from_filename?(name)
    end

    private def ts_str(t : Time?) : String?
      t ? Claim.format_ts(t) : nil
    end
  end
end
