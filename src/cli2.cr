require "jargon"
require "./library"
require "./fusefs"

# Interim CLI for the new claim-log core, now driven by Jargon (>= 0.18).
# Each subcommand's interface is a YAML schema under schemas/, embedded at
# COMPILE TIME via `read_file` so the binary is self-contained. The schema is
# the contract both this CLI and a future GUI consume (docs/architecture.md §7);
# Jargon owns syntax, transfs owns semantic resolution (<id>/query -> document).
#
# Separate from the legacy `transfs` (run.cr/cli.cr) during the transition.
module TransFS
  class CLI2
    # name => embedded YAML schema (compile-time; no runtime file dependency).
    SCHEMAS = {
      "add"        => {{ read_file("#{__DIR__}/../schemas/add.yaml") }},
      "addversion" => {{ read_file("#{__DIR__}/../schemas/addversion.yaml") }},
      "rename"     => {{ read_file("#{__DIR__}/../schemas/rename.yaml") }},
      "tag"        => {{ read_file("#{__DIR__}/../schemas/tag.yaml") }},
      "untag"      => {{ read_file("#{__DIR__}/../schemas/untag.yaml") }},
      "list"       => {{ read_file("#{__DIR__}/../schemas/list.yaml") }},
      "find"       => {{ read_file("#{__DIR__}/../schemas/find.yaml") }},
      "reindex"    => {{ read_file("#{__DIR__}/../schemas/reindex.yaml") }},
      "cat"        => {{ read_file("#{__DIR__}/../schemas/cat.yaml") }},
      "show"       => {{ read_file("#{__DIR__}/../schemas/show.yaml") }},
      "versions"   => {{ read_file("#{__DIR__}/../schemas/versions.yaml") }},
      "mount"      => {{ read_file("#{__DIR__}/../schemas/mount.yaml") }},
    }

    def initialize(@root : String)
      @lib = Library.new(@root)
    end

    def self.default_root : String
      ENV["TRANSFS_STORE"]? || File.join(Dir.current, "test", "store")
    end

    def self.build_cli : Jargon::CLI
      cli = Jargon.new("transfs2")
      SCHEMAS.each { |name, yaml| cli.subcommand(name, yaml: yaml) }
      cli
    end

    # Dispatch a parsed Jargon result to the matching command.
    def dispatch(result : Jargon::Result)
      case result.subcommand
      when "add"        then cmd_add(result)
      when "addversion" then cmd_addversion(result)
      when "rename"     then cmd_rename(result)
      when "tag"        then cmd_tag(result)
      when "untag"      then cmd_untag(result)
      when "list"       then cmd_list
      when "find"       then cmd_find(result)
      when "reindex"    then cmd_reindex
      when "cat"        then cmd_cat(result)
      when "show"       then cmd_show(result)
      when "versions"   then cmd_versions(result)
      when "mount"      then cmd_mount(result)
      else                   abort("unknown command")
      end
    end

    # --- string / array accessors over Jargon::Result ---

    private def str(result, key) : String
      result[key]?.try(&.as_s?) || abort("missing argument: #{key}")
    end

    private def str?(result, key) : String?
      result[key]?.try(&.as_s?)
    end

    private def strings(result, key) : Array(String)
      result[key]?.try(&.as_a?).try(&.map(&.as_s)) || [] of String
    end

    private def resolve(id : String) : Document
      @lib.document(id) || abort("no such document: #{id}")
    rescue ex
      abort(ex.message)
    end

    # --- commands ---

    private def cmd_add(r)
      doc = @lib.add(str(r, "file"), str?(r, "name"))
      puts "added #{doc.id[0, 12]}  \"#{doc.name}\"  (#{doc.version_count} version, head #{doc.head.try(&.[0, 12])})"
    end

    private def cmd_addversion(r)
      doc = @lib.add_version(resolve(str(r, "id")), str(r, "file"))
      puts "added version #{doc.head.try(&.[0, 12])} to #{doc.id[0, 12]} (now v#{doc.version_count})"
    end

    private def cmd_rename(r)
      doc = @lib.rename(resolve(str(r, "id")), str(r, "name"))
      puts "renamed #{doc.id[0, 12]} -> \"#{doc.name}\""
    end

    private def cmd_tag(r)
      doc = resolve(str(r, "id"))
      tags = strings(r, "tags")
      abort("no tags given") if tags.empty?
      doc = @lib.tag(doc, add: tags)
      puts "tags: #{doc.tags.to_a.sort.join(", ")}"
    end

    private def cmd_untag(r)
      doc = resolve(str(r, "id"))
      tags = strings(r, "tags")
      abort("no tags given") if tags.empty?
      doc = @lib.tag(doc, del: tags)
      puts "tags: #{doc.tags.to_a.sort.join(", ")}"
    end

    private def cmd_list
      print_rows(@lib.index.all)
    end

    private def cmd_find(r)
      q = str(r, "query")
      rows =
        case q
        when .starts_with?("tag:")  then @lib.index.by_tag(q[4..])
        when .starts_with?("type:") then @lib.index.by_type(q[5..])
        when .starts_with?("name:") then @lib.index.by_name(q[5..])
        else                             @lib.index.by_name(q)
        end
      print_rows(rows)
    end

    private def cmd_reindex
      @lib.index.rebuild
      puts "reindexed #{@lib.index.all.size} documents"
    end

    private def cmd_mount(r)
      # Absolute mountpoint: libfuse changes the process's cwd once it starts
      # serving, so a relative path would resolve against the wrong directory.
      mp = File.expand_path(str(r, "mountpoint"))
      abort("mountpoint does not exist: #{mp}") unless Dir.exists?(mp)
      # `-o ro` makes the kernel itself reject writes with EROFS (the honest
      # "read-only filesystem" signal) before they reach us — the structural
      # enforcement of "don't edit through the mount" (docs §7).
      FuseSystem.new(@lib).mount(["transfs", "-f", "-o", "ro", mp])
    end

    private def cmd_cat(r)
      doc = resolve(str(r, "id"))
      bytes = @lib.read(doc) || abort("document has no content")
      STDOUT.write(bytes)
    end

    private def cmd_show(r)
      doc = resolve(str(r, "id"))
      puts "id:        #{doc.id}"
      puts "name:      #{doc.name}"
      puts "created:   #{doc.created_at}"
      puts "versions:  #{doc.version_count}"
      puts "head:      #{doc.head}"
      puts "tags:      #{doc.tags.to_a.sort.join(", ")}"
    end

    private def cmd_versions(r)
      doc = resolve(str(r, "id"))
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

    private def print_rows(rows)
      if rows.empty?
        puts "(none)"
        return
      end
      rows.each do |r|
        name = (r.name || "(unnamed)").ljust(24)
        type = (r.type || "").ljust(16)
        puts "#{r.id[0, 12]}  #{name}  #{type}  v#{r.version_count}  #{r.tags.join(",")}"
      end
    end
  end
end

# Entry point: optional `--store DIR`, then Jargon parses the subcommand.
args = ARGV.dup
root = TransFS::CLI2.default_root
if args.first? == "--store"
  args.shift
  root = args.shift? || abort("--store needs a directory")
end
# Absolute store path: the mount's libfuse loop changes cwd while serving, so a
# relative `--store` would break the lazily-opened index and blob reads.
root = File.expand_path(root)

cli = TransFS::CLI2.build_cli
result = cli.run(args)
unless result.valid?
  STDERR.puts result.errors.join("\n")
  exit 1
end
TransFS::CLI2.new(root).dispatch(result)
