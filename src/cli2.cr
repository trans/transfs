require "./library"

# Interim CLI for the new claim-log core (slice 1: add, list, cat).
# Hand-rolled for now; to be replaced by Jargon-driven commands once the core
# is proven (docs/architecture.md §7). Separate from the legacy `transfs`
# (run.cr/cli.cr) so the old SQL model keeps working during the transition.
module TransFS
  class CLI2
    def initialize(@root : String)
      @lib = Library.new(@root)
    end

    def self.default_root : String
      ENV["TRANSFS_STORE"]? || File.join(Dir.current, "test", "store")
    end

    def usage
      puts <<-USAGE
      Usage: transfs2 [--store DIR] <command> [args]

      Commands:
        add <file> [name]   Archive a file as a new document
        list                List all documents (folded from their logs)
        cat <id>            Print a document's current content
        show <id>           Show a document's folded state
      USAGE
      exit 1
    end

    def run(args : Array(String))
      usage if args.empty?
      case args.shift
      when "add"   then cmd_add(args)
      when "list"  then cmd_list
      when "cat"   then cmd_cat(args)
      when "show"  then cmd_show(args)
      else              usage
      end
    end

    private def cmd_add(args)
      file = args.shift? || usage
      name = args.shift?
      doc = @lib.add(file, name)
      puts "added #{doc.id[0, 12]}  \"#{doc.name}\"  (#{doc.version_count} version, head #{doc.head.try(&.[0, 12])})"
    end

    private def cmd_list
      docs = @lib.documents
      if docs.empty?
        puts "(no documents)"
        return
      end
      docs.sort_by! { |d| d.created_at || Time.unix(0) }
      docs.each do |d|
        puts "#{d.id[0, 12]}  #{(d.name || "(unnamed)").ljust(24)}  v#{d.version_count}  #{d.tags.to_a.join(",")}"
      end
    end

    private def cmd_cat(args)
      id = args.shift? || usage
      doc = @lib.document(id) || abort("no such document: #{id}")
      bytes = @lib.read(doc) || abort("document has no content: #{id}")
      STDOUT.write(bytes)
    end

    private def cmd_show(args)
      id = args.shift? || usage
      doc = @lib.document(id) || abort("no such document: #{id}")
      puts "id:        #{doc.id}"
      puts "name:      #{doc.name}"
      puts "created:   #{doc.created_at}"
      puts "versions:  #{doc.version_count}"
      puts "head:      #{doc.head}"
      puts "tags:      #{doc.tags.to_a.join(", ")}"
    end
  end
end

# Entry point: optional `--store DIR` then the command.
args = ARGV.dup
root = TransFS::CLI2.default_root
if args.first? == "--store"
  args.shift
  root = args.shift? || abort("--store needs a directory")
end
TransFS::CLI2.new(root).run(args)
