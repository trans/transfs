require "digest/sha256"
require "set"
require "./cas"
require "./claim"
require "./document"
require "./log"

module TransFS
  class Check
    record Issue, path : String, message : String
    record Result, documents : Int32, blobs : Int32, warnings : Array(Issue), errors : Array(Issue) do
      def clean? : Bool
        errors.empty?
      end
    end

    def initialize(@root : String)
      @cas = CAS.new(@root)
    end

    def run : Result
      warnings = [] of Issue
      errors = [] of Issue
      referenced = Set(String).new
      document_count = 0
      unreadable_logs = false

      Log.all_ids(@root).each do |id|
        log = Log.new(@root, id)
        begin
          result = log.read
          document_count += 1
          if torn_tail = result.torn_tail
            warnings << Issue.new(torn_tail.path,
              "line #{torn_tail.line_number}: ignored torn trailing record (#{torn_tail.reason})")
          end
          check_claims(id, log.path, result.claims, referenced, errors)
        rescue ex : Log::Corrupt
          unreadable_logs = true
          errors << Issue.new(ex.path, "line #{ex.line_number}: #{ex.reason}")
        end
      end

      blob_hashes = all_blob_hashes
      blob_hashes.each do |hex|
        path = @cas.path_for(hex)
        if Digest::SHA256.hexdigest(File.read(path).to_slice) != hex
          errors << Issue.new(path, "blob hash mismatch")
        end
        if !unreadable_logs && !referenced.includes?(hex)
          warnings << Issue.new(path, "orphan blob is not referenced by any version claim")
        end
      end

      Result.new(document_count, blob_hashes.size, warnings, errors)
    end

    private def check_claims(id : String, path : String, claims : Array(Claim),
                             referenced : Set(String), errors : Array(Issue)) : Nil
      creates = claims.compact_map { |c| c.as?(CreateClaim) }
      if creates.size != 1
        errors << Issue.new(path, "expected exactly one create claim, found #{creates.size}")
      elsif creates.first.doc_id != id
        errors << Issue.new(path, "document id mismatch: create hashes to #{creates.first.doc_id}")
      end

      claims.each do |claim|
        next unless version = claim.as?(VersionClaim)
        referenced << version.hash
        unless valid_hash?(version.hash)
          errors << Issue.new(path, "version references invalid blob hash #{version.hash.inspect}")
          next
        end
        blob_path = @cas.path_for(version.hash)
        unless File.exists?(blob_path)
          errors << Issue.new(path, "version references missing blob #{version.hash}")
        end
      end
    end

    private def all_blob_hashes : Array(String)
      blobs_dir = @cas.blobs_dir
      return [] of String unless Dir.exists?(blobs_dir)
      Dir.glob(File.join(blobs_dir, "*", "*")).map { |path| File.basename(path) }.sort
    end

    private def valid_hash?(hex : String) : Bool
      hex.size == 64 && hex.each_char.all? { |ch| ('0' <= ch && ch <= '9') || ('a' <= ch && ch <= 'f') }
    end
  end
end
