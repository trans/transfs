require "./spec_helper"
require "digest/sha256"
require "file_utils"

# Specs for the claim-log core (docs/architecture.md). These lock in the
# truth-layer invariants proven by hand during slice 1 + the claim-ops slice.
module TransFS
  PDF_BYTES  = "%PDF-1.4\n%\xE2\xE3\xCF\xD3\n1 0 obj\n<<>>\nendobj\n"
  JPEG_BYTES = "\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01\x01\x01\x00H\x00H\x00\x00"

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
              d1.id.should_not eq d2.id # distinct documents
              d1.head.should eq d2.head # one shared blob
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
            reloaded.name.should eq "Doc" # torn line ignored
          end
        end
      end

      it "raises on an unparseable middle line" do
        with_store do |fs, root|
          with_file("body") do |f|
            doc = fs.add(f, "Doc")
            log = Log.new(root, doc.id)
            lines = File.read_lines(log.path)
            lines.insert(1, %({"op":"name","name":"broken))
            File.write(log.path, lines.join("\n") + "\n")

            expect_raises(Log::Corrupt) do
              Document.load(root, doc.id)
            end
          end
        end
      end

      it "ignores valid unknown future ops" do
        with_store do |fs, root|
          with_file("body") do |f|
            doc = fs.add(f, "Doc")
            File.open(Log.new(root, doc.id).path, "a") do |io|
              io.puts %({"op":"future","ts":"#{Claim.format_ts(Time.utc)}","field":"ok"})
            end

            Document.load(root, doc.id).name.should eq "Doc"
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

  describe Query do
    it "parses facet-walk components; root is empty; lone = toggles to doc view" do
      Query.parse("/").components.should be_empty
      Query.parse("/").doc_view.should be_false
      Query.parse("/year/1920").components.should eq ["year", "1920"]
      Query.parse("/=/").doc_view.should be_true
    end

    it "normalizes the = alias to / and splits into components" do
      Query.parse("/stars=4/").components.should eq ["stars", "4"]
      Query.parse("/date=1920/08/10/").components.should eq ["date", "1920", "08", "10"]
    end

    it "drops empty segments and counts = parity for the view" do
      Query.parse("//a///b/").components.should eq ["a", "b"]
      Query.parse("/=/=/").doc_view.should be_false # two toggles -> back to facets
    end

    it "captures the trailing document name in doc view" do
      p = Query.parse("/year/1920/=/report.pdf")
      p.components.should eq ["year", "1920"]
      p.doc_view.should be_true
      p.doc_name.should eq "report.pdf"
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

    it "stores tags as hierarchical paths (= aliases /, booleans bucket under tag/)" do
      with_store do |fs, root|
        with_file("x") do |f|
          doc = fs.add(f)
          fs.tag(doc, add: ["finance", "stars=4"])
          idx = Index.new(root)
          idx.by_tag("finance").map(&.id).should eq [doc.id] # boolean, under tag/
          idx.by_tag("stars").map(&.id).should eq [doc.id]
          tags = idx.all.find { |r| r.id == doc.id }.not_nil!.tags
          tags.should contain "tag/finance" # boolean -> tag/ bucket
          tags.should contain "stars/4"     # = aliases /
        end
      end
    end

    it "finds by type derived from the blob content" do
      with_store do |fs|
        with_file(JPEG_BYTES) { |f| fs.add(f, "not-an-image.txt") }
        with_file(PDF_BYTES) { |f| fs.add(f, "paper.bin") }
        fs.index.by_type("image").map(&.name).should eq ["not-an-image.txt"]
      end
    end

    it "does not change type when a document is renamed" do
      with_store do |fs|
        with_file(PDF_BYTES) do |f|
          doc = fs.add(f, "paper.pdf")
          fs.index.by_type("application/pdf").map(&.id).should eq [doc.id]
          doc = fs.rename(doc, "paper")
          fs.index.by_type("application/pdf").map(&.id).should eq [doc.id]
        end
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
        with_file("one\n") { |f| fs.add(f, "report.txt") }
        with_file("two\n") { |f| fs.add(f, "report.txt") }
        with_file("three\n") { |f| fs.add(f, "other.txt") }
        fs.index.neighborhood("report.txt", "text/plain").size.should eq 2
      end
    end

    describe "prefix-walk navigation (#walk / #docs / #facets)" do
      # seed: a.pdf {vacation, year=1920}; b.jpg {vacation}; c.txt {year=2020, stars=4}
      it "walks components into constraints + partial with exact value pairing" do
        with_store do |fs|
          a = ""
          with_file(PDF_BYTES) { |f| d = fs.add(f, "a.pdf"); fs.tag(d, add: ["vacation", "year=1920"]); a = d.id }
          with_file(JPEG_BYTES) { |f| fs.add(f, "b.jpg") } # makes type/image a real prefix
          with_file("txt") { |f| d = fs.add(f, "c.txt"); fs.tag(d, add: ["year=2020", "stars=4"]) }
          idx = fs.index

          # year/1920 is one tag: the partial holds it; /year/1920/ == year=1920
          w = idx.walk(["year", "1920"])
          w.valid.should be_true
          idx.docs(w, 100).map(&.id).should eq [a]

          # two tags: year/1920 completes (a constraint), type/image is the partial
          w2 = idx.walk(["year", "1920", "type", "image"])
          w2.constraints.should eq ["year/1920"]
          w2.partial.should eq "type/image"
          idx.docs(w2, 100).should be_empty # a.pdf is not an image

          idx.walk(["nope"]).valid.should be_false # unknown component
        end
      end

      it "renders docs recency-windowed; empty walk = all docs (recents)" do
        with_store do |fs|
          a = b = c = ""
          with_file(PDF_BYTES) { |f| d = fs.add(f, "a.pdf"); fs.tag(d, add: ["vacation", "year=1920"]); a = d.id }
          with_file(JPEG_BYTES) { |f| d = fs.add(f, "b.jpg"); fs.tag(d, add: ["vacation"]); b = d.id }
          with_file("txt") { |f| d = fs.add(f, "c.txt"); fs.tag(d, add: ["year=2020", "stars=4"]); c = d.id }
          idx = fs.index

          idx.docs(idx.walk([] of String), 100).map(&.id).sort.should eq [a, b, c].sort
          idx.docs(idx.walk(["tag", "vacation"]), 100).map(&.id).sort.should eq [a, b].sort
          idx.docs(idx.walk([] of String), 100).first.head_hash.should_not be_nil
        end
      end

      it "enumerates splitting facet keys, then values, with the tag/ bucket" do
        with_store do |fs|
          with_file(PDF_BYTES) { |f| d = fs.add(f, "a.pdf"); fs.tag(d, add: ["vacation", "year=1920"]) }
          with_file(JPEG_BYTES) { |f| d = fs.add(f, "b.jpg"); fs.tag(d, add: ["vacation"]) }
          with_file("txt") { |f| d = fs.add(f, "c.txt"); fs.tag(d, add: ["year=2020", "stars=4"]) }
          idx = fs.index

          top = idx.facets(idx.walk([] of String))
          ["type", "year", "stars", "tag"].each { |k| top.should contain k }
          top.should_not contain "owner" # all docs owner/local -> doesn't split, hidden

          idx.facets(idx.walk(["year"])).should eq ["1920", "2020"]
          idx.facets(idx.walk(["tag"])).should eq ["vacation"]
        end
      end

      it "shows co-facets after completing a tag (not an empty listing)" do
        with_store do |fs|
          with_file("p") { |f| d = fs.add(f, "q1.pdf"); fs.tag(d, add: ["project=acme", "finance", "stars=4"]) }
          with_file("p") { |f| d = fs.add(f, "q2.pdf"); fs.tag(d, add: ["project=acme", "finance", "stars=5"]) }
          with_file("m") { |f| d = fs.add(f, "notes.md"); fs.tag(d, add: ["project=acme", "work"]) }
          idx = fs.index

          # /project/acme/ is a completed tag: enumerate the OTHER splitting keys
          facets = idx.facets(idx.walk(["project", "acme"]))
          facets.should contain "stars"       # q1=4, q2=5 -> splits
          facets.should contain "tag"         # finance vs work -> splits
          facets.should_not contain "project" # already chosen; doesn't split
        end
      end
    end
  end

  describe Check do
    it "reports a torn trailing record as a warning" do
      with_store do |fs, root|
        with_file("body") do |f|
          doc = fs.add(f, "Doc")
          File.open(Log.new(root, doc.id).path, "a") do |io|
            io.print %({"op":"name","name":"half-writ)
          end

          result = Check.new(root).run
          result.errors.should be_empty
          result.warnings.map(&.message).join("\n").should contain "ignored torn trailing record"
        end
      end
    end

    it "reports missing blobs referenced by version claims" do
      with_store do |fs, root|
        with_file("body") do |f|
          doc = fs.add(f, "Doc")
          File.delete(CAS.new(root).path_for(doc.head.not_nil!))

          result = Check.new(root).run
          result.errors.map(&.message).join("\n").should contain "version references missing blob"
        end
      end
    end
  end
end
