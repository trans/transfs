require "./spec_helper"
require "digest/sha256"
require "file_utils"

# Specs for the claim-log core (docs/architecture.md). These lock in the
# truth-layer invariants proven by hand during slice 1 + the claim-ops slice.
module TransFS
  describe Library do
    describe "#add" do
      it "creates a document whose id is sha256 of its create claim" do
        with_store do |fs, root|
          with_file("hello\n") do |f|
            doc = fs.add(f, "hello.txt")
            create = Log.new(root, doc.id).claims
              .find!(&.is_a?(CreateClaim)).as(CreateClaim)
            doc.id.should eq Digest::SHA256.hexdigest(create.canonical)
          end
        end
      end

      it "stores content whose bytes hash to the blob filename" do
        with_store do |fs, root|
          with_file("some content\n") do |f|
            doc = fs.add(f)
            head = doc.head.not_nil!
            bytes = CAS.new(root).get(head).not_nil!
            Digest::SHA256.hexdigest(bytes).should eq head
          end
        end
      end

      it "dedups identical content to one blob" do
        with_store do |fs, root|
          with_file("dup\n") do |a|
            with_file("dup\n") do |b|
              d1 = fs.add(a)
              d2 = fs.add(b)
              d1.id.should_not eq d2.id      # distinct documents
              d1.head.should eq d2.head      # one shared blob
              Dir.glob(File.join(root, "blobs", "*", "*")).size.should eq 1
            end
          end
        end
      end
    end

    describe "tag folding" do
      it "applies add then del in order (add finance,q2; del q2 => finance)" do
        with_store do |fs|
          with_file("x") do |f|
            doc = fs.add(f)
            doc = fs.tag(doc, add: ["finance", "q2"])
            doc = fs.tag(doc, del: ["q2"])
            doc.tags.should eq Set{"finance"}
          end
        end
      end

      it "is idempotent on repeated adds" do
        with_store do |fs|
          with_file("x") do |f|
            doc = fs.add(f)
            doc = fs.tag(doc, add: ["a"])
            doc = fs.tag(doc, add: ["a"])
            doc.tags.should eq Set{"a"}
          end
        end
      end
    end

    describe "versions" do
      it "chains parent edges and moves head to the newest" do
        with_store do |fs|
          with_file("v1") do |f1|
            doc = fs.add(f1)
            v1 = doc.head.not_nil!
            with_file("v2") do |f2|
              doc = fs.add_version(doc, f2)
              doc.version_count.should eq 2
              doc.versions.last.parent.should eq v1
              doc.head.should_not eq v1
            end
          end
        end
      end
    end

    describe "rename" do
      it "takes the latest name claim" do
        with_store do |fs|
          with_file("x") do |f|
            doc = fs.add(f, "first")
            doc = fs.rename(doc, "second")
            doc.name.should eq "second"
          end
        end
      end
    end

    describe "the log is the source of truth" do
      it "reconstructs identical state from a fresh fold of disk" do
        with_store do |fs, root|
          with_file("body") do |f|
            doc = fs.add(f, "Doc")
            doc = fs.tag(doc, add: ["t1", "t2"])
            doc = fs.rename(doc, "Renamed")
            id = doc.id

            # Fresh fold — no in-memory state carried over.
            reloaded = Document.load(root, id)
            reloaded.name.should eq "Renamed"
            reloaded.tags.should eq Set{"t1", "t2"}
            reloaded.version_count.should eq 1
            reloaded.head.should eq doc.head
          end
        end
      end

      it "skips an unparseable trailing line (torn-tail rule)" do
        with_store do |fs, root|
          with_file("body") do |f|
            doc = fs.add(f, "Doc")
            # Simulate a crash-torn final append.
            File.open(Log.new(root, doc.id).path, "a") do |io|
              io.print %({"op":"name","name":"half-writ)
            end
            reloaded = Document.load(root, doc.id)
            reloaded.name.should eq "Doc"  # torn line ignored
          end
        end
      end
    end

    describe "#document prefix resolution" do
      it "resolves a unique short prefix" do
        with_store do |fs|
          with_file("x") do |f|
            doc = fs.add(f)
            fs.document(doc.id[0, 12]).not_nil!.id.should eq doc.id
          end
        end
      end

      it "returns nil for an unknown prefix" do
        with_store do |fs|
          fs.document("deadbeef").should be_nil
        end
      end
    end
  end

  describe Index do
    it "lists documents materialized from the logs" do
      with_store do |fs|
        with_file("a") { |f| fs.add(f, "alpha.txt") }
        with_file("b") { |f| fs.add(f, "beta.txt") }
        fs.index.all.map(&.name).compact.sort.should eq ["alpha.txt", "beta.txt"]
      end
    end

    it "splits key=value tags into (key, value) and keeps booleans null" do
      with_store do |fs, root|
        with_file("x") do |f|
          doc = fs.add(f)
          fs.tag(doc, add: ["finance", "stars=4"])
          idx = Index.new(root)
          # boolean tag: queryable by key
          idx.by_tag("finance").map(&.id).should eq [doc.id]
          # key=value tag: rendered back as stars=4 in the row's tag list
          idx.by_tag("stars").first.tags.should contain "stars=4"
        end
      end
    end

    it "finds by type derived from the name" do
      with_store do |fs|
        with_file("img") { |f| fs.add(f, "pic.jpg") }
        with_file("doc") { |f| fs.add(f, "paper.pdf") }
        fs.index.by_type("image").map(&.name).should eq ["pic.jpg"]
      end
    end

    it "rebuilds identically from the logs after the db is deleted" do
      with_store do |fs, root|
        with_file("a") { |f| fs.add(f, "one.txt") }
        with_file("b") do |f|
          doc = fs.add(f, "two.txt")
          fs.tag(doc, add: ["t"])
        end
        before = fs.index.all
        fs.index.close

        File.delete(File.join(root, ".transfs", "index.db"))
        after = Index.new(root).all # opening rebuilds from logs

        before.map(&.id).sort.should eq after.map(&.id).sort
        before.size.should eq after.size
      end
    end

    it "exposes the collision neighborhood (same name) for disambiguation" do
      with_store do |fs|
        with_file("1") { |f| fs.add(f, "report.txt") }
        with_file("2") { |f| fs.add(f, "report.txt") }
        with_file("3") { |f| fs.add(f, "other.txt") }
        fs.index.neighborhood("report.txt", "text/plain").size.should eq 2
      end
    end
  end
end
