require "digest/sha256"
require "mime"
require "./config"

module TransFS
  extend self

  # Get a list of existing tags.
  # TODO: Support filter?
  def tags(store : Store)
    db = store.database
    list = [] of String
    db.query("SELECT name FROM tags ORDER BY name") do |rs|
      rs.each { list << rs.read(String) }
    end
    list
  end

  # Get a list of files.
  #
  def files(store : Store, tag : String | Nil = nil)
    db = store.database
    list = [] of String
    if tag
      db.query(<<-SQL, tag) do |rs|
        SELECT f.original_name FROM files f
          JOIN file_tags ft ON f.hash = ft.file_hash
          JOIN tags t ON ft.tag_id = t.id
          WHERE t.name = ?
      SQL
        rs.each { list << rs.read(String) }
      end
    else
      db.query("SELECT original_name FROM files ORDER BY added_at DESC") do |rs|
        rs.each { list << rs.read(String) }
      end
    end
    list
  end

  # Given the hash of the a file, link the given tags to the file in the database.
  #
  def tag_file(store : Store, hash : Bytes, tags = [] of String) : Bytes
    db = store.database
    db.transaction do
      # Verify that the file exists in database
      found = db.query_one?("SELECT 1 FROM files WHERE hash = ?", hash, as: Int32)
      raise "File not found" unless found
      # TODO: instead of raise, see if in files and if so, what to do about the inconsistancy?
      tags.each do |tag|
        # Insert tag if it doesn't exist yet
        tag_id = db.query_one?("SELECT id FROM tags WHERE name = ?", tag, as: Int32) ||
                 db.query_one("INSERT INTO tags (name) VALUES (?) RETURNING id", tag, as: Int32)
        # Link file + tag (ignore if already linked)
        db.exec("INSERT OR IGNORE INTO file_tags (file_hash, tag_id) VALUES (?, ?)", hash, tag_id)
      end
    end
    return hash
  end

  # Add file to data store, and tag if any tags are given.
  # 
  def add_file(store : Store, filepath : String, tags = [] of String) : Bytes
    filename = File.basename(filepath)
    ext = File.extname(filename).lstrip('.').downcase
    size = File.size(filepath)

    # Read file content and compute hash
    content = File.read(filepath).to_slice
    hash = Digest::SHA256.digest(content)
    hex = hash.hexstring
    prefix = hex[0..1]

    # Destination path
    target_dir = File.join(store.root, ext, prefix)
    target_path = File.join(target_dir, hex)

    # Deduplicate
    unless File.exists?(target_path)
      Dir.mkdir_p(target_dir)
      File.write(target_path, content)
    end

    # Infer MIME type if possible
    mime_type = MIME.from_filename(filename) || ""

    # Insert metadata if not already present
    db = store.database
    db.exec <<-SQL, hash, size, ext, mime_type, filename
      INSERT OR IGNORE INTO files (hash, size, extension, mime_type, original_name)
      VALUES (?, ?, ?, ?, ?)
    SQL

    tag_file(store, hash, tags) unless tags.empty?

    return hash
  end

end
