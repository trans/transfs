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
    getter version_hashes : Array(String)  # content history, oldest-first
    getter tags : Set(String)

    def initialize(@id : String)
      @name = nil
      @created_at = nil
      @version_hashes = [] of String
      @tags = Set(String).new
    end

    # The current content = the newest version's hash (nil if no version yet:
    # the document is a bare shell or a container). §5.
    def head : String?
      @version_hashes.last?
    end

    def version_count : Int32
      @version_hashes.size
    end

    # Fold a log into a Document. Claims are applied in timestamp order so that
    # a merged log (interleaved from two machines) folds deterministically;
    # within equal timestamps, file order is kept as the tiebreak for now.
    def self.fold(id : String, claims : Array(Claim)) : Document
      doc = new(id)
      claims.sort_by(&.ts).each { |c| doc.apply(c) }
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
        @version_hashes << claim.hash
      when NameClaim
        @name = claim.name
      when TagClaim
        claim.add.each { |t| @tags << t }
        claim.del.each { |t| @tags.delete(t) }
      end
    end
  end
end
