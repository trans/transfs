require "./claim"
require "./log"

module TransFS
  # The folded state of a document — the result of replaying its claim log in
  # order. This is *derived*, never stored on disk as truth (the log is truth;
  # this is what the future SQLite index materializes). See §1, §6.
  class Document
    getter id : String
    getter name : String?
    getter created_at : Time?
    getter versions : Array(VersionClaim)  # content history, oldest-first
    getter tags : Set(String)

    def initialize(@id : String)
      @name = nil
      @created_at = nil
      @versions = [] of VersionClaim
      @tags = Set(String).new
    end

    # Just the content hashes, oldest-first.
    def version_hashes : Array(String)
      @versions.map(&.hash)
    end

    # The current content = the newest version's hash (nil if no version yet:
    # the document is a bare shell or a container). §5.
    def head : String?
      @versions.last?.try(&.hash)
    end

    def version_count : Int32
      @versions.size
    end

    # Fold a log into a Document. Claims are applied in timestamp order so that
    # a merged log (interleaved from two machines) folds deterministically;
    # within equal timestamps, **file order** is the tiebreak. The sort key is
    # `{ts, original_index}` because Crystal's `sort_by` is not stable on its
    # own — and a batch (`create`+`version`+`name`) shares one timestamp, so
    # without the index tiebreak the fold order would be undefined.
    def self.fold(id : String, claims : Array(Claim)) : Document
      doc = new(id)
      claims.map_with_index { |c, i| {c, i} }
        .sort_by! { |(c, i)| {c.ts, i} }
        .each { |(c, _)| doc.apply(c) }
      doc
    end

    def self.load(root : String, id : String) : Document
      fold(id, Log.new(root, id).claims)
    end

    # Every document in the store, folded.
    def self.all(root : String) : Array(Document)
      Log.all_ids(root).map { |id| load(root, id) }
    end

    protected def apply(claim : Claim) : Nil
      case claim
      when CreateClaim
        @created_at = claim.ts
      when VersionClaim
        @versions << claim
      when NameClaim
        @name = claim.name
      when TagClaim
        claim.add.each { |t| @tags << t }
        claim.del.each { |t| @tags.delete(t) }
      end
    end
  end
end
