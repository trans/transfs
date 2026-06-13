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
        add <file> [name]      Archive a file as a new document
        addversion <id> <file> Add a new version of an existing document
        rename <id> <name>     Set a document's name
        tag <id> [+t] [-t] ... Add (+) and/or remove (-) tags
        list                   List all documents (folded from their logs)
        cat <id>               Print a document's current content
        show <id>              Show a document's folded state
        versions <id>          Show a document's version history
      USAGE
      exit 1
    end

    def run(args : Array(String))
      usage if args.empty?
      case args.shift
      when "add"        then cmd_add(args)
      when "addversion" then cmd_addversion(args)
      when "rename"     then cmd_rename(args)
      when "tag"        then cmd_tag(args)
      when "list"       then cmd_list
      when "cat"        then cmd_cat(args)
      when "show"       then cmd_show(args)
      when "versions"   then cmd_versions(args)
      else                   usage
      end
    end

    # Resolve an id-prefix arg to a document or abort with a clear message.
    private def resolve(id : String) : Document
      @lib.document(id) || abort("no such document: #{id}")
    rescue ex
      abort(ex.message)
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

    private def cmd_addversion(args)
      id = args.shift? || usage
      file = args.shift? || usage
      doc = @lib.add_version(resolve(id), file)
      puts "added version #{doc.head.try(&.[0, 12])} to #{doc.id[0, 12]} (now v#{doc.version_count})"
    end

    private def cmd_rename(args)
      id = args.shift? || usage
      name = args.shift? || usage
      doc = @lib.rename(resolve(id), name)
      puts "renamed #{doc.id[0, 12]} -> \"#{doc.name}\""
    end

    # tag <id> +finance +q2 -draft   (leading + adds, - removes)
    private def cmd_tag(args)
      id = args.shift? || usage
      doc = resolve(id)
      add = [] of String
      del = [] of String
      args.each do |a|
        case a[0]?
        when '+' then add << a[1..]
        when '-' then del << a[1..]
        else          add << a # bare token = add
        end
      end
      usage if add.empty? && del.empty?
      # A tag appearing in both is a no-op; reject it up front (the CLI guard
      # the design calls for, §3 tag catalog).
      if (dup = (add & del)).any?
        abort("tag(s) in both add and remove: #{dup.join(", ")}")
      end
      doc = @lib.tag(doc, add: add, del: del)
      puts "tags: #{doc.tags.to_a.sort.join(", ")}"
    end

    private def cmd_cat(args)
      id = args.shift? || usage
      doc = resolve(id)
      bytes = @lib.read(doc) || abort("document has no content: #{id}")
      STDOUT.write(bytes)
    end

    private def cmd_show(args)
      id = args.shift? || usage
      doc = resolve(id)
      puts "id:        #{doc.id}"
      puts "name:      #{doc.name}"
      puts "created:   #{doc.created_at}"
      puts "versions:  #{doc.version_count}"
      puts "head:      #{doc.head}"
      puts "tags:      #{doc.tags.to_a.sort.join(", ")}"
    end

    private def cmd_versions(args)
      id = args.shift? || usage
      doc = resolve(id)
      if doc.versions.empty?
        puts "(no versions)"
        return
      end
      doc.versions.each_with_index do |v, i|
        marker = (i == doc.versions.size - 1) ? "* " : "  "
        parent = v.parent ? v.parent.not_nil![0, 12] : "(root)"
        puts "#{marker}v#{i + 1}  #{v.hash[0, 12]}  parent=#{parent}  #{v.ts}"
      end
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
