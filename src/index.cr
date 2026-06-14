require "db"
require "sqlite3"
require "mime"
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
  #   * the db lives at `<root>/.transfs/index.db`, not `<root>/files.db` — the
  #     legacy SQL model still owns files.db during the transition.
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
      @db.exec <<-SQL
        CREATE TABLE IF NOT EXISTS doc_tags (
          doc_id TEXT NOT NULL,
          key    TEXT NOT NULL,
          value  TEXT,
          PRIMARY KEY (doc_id, key, value)
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
      @db.exec "CREATE INDEX IF NOT EXISTS idx_doc_tags_key_value ON doc_tags(key, value)"
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

      doc.tags.each do |t|
        key, value = split_tag(t)
        @db.exec("INSERT OR IGNORE INTO doc_tags (doc_id, key, value) VALUES (?, ?, ?)",
          doc.id, key, value)
      end
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

    def all : Array(Row)
      query_rows("SELECT id, name, type, size, version_count, date_added " \
                 "FROM documents ORDER BY date_added")
    end

    def by_tag(key : String) : Array(Row)
      query_rows(
        "SELECT d.id, d.name, d.type, d.size, d.version_count, d.date_added " \
        "FROM documents d JOIN doc_tags t ON t.doc_id = d.id " \
        "WHERE t.key = ? ORDER BY d.date_added", key)
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

    private def tags_for(id : String) : Array(String)
      out = [] of String
      @db.query("SELECT key, value FROM doc_tags WHERE doc_id = ? ORDER BY key", id) do |rs|
        rs.each do
          k = rs.read(String)
          v = rs.read(String?)
          out << (v ? "#{k}=#{v}" : k)
        end
      end
      out
    end

    # --- helpers ---

    # Split the key=value tag convention at index time (§6). First '=' splits;
    # a bare tag has a nil value (boolean facet).
    private def split_tag(tag : String) : {String, String?}
      if i = tag.index('=')
        {tag[0, i], tag[(i + 1)..]}
      else
        {tag, nil}
      end
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
