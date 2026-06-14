module TransFS

  class CLI
    @config : TransFS::Config | Nil

    #STORE_ROOT = "./test/store_root"

    def initialize()
      @config = nil
    end

    def usage
      puts "Usage:"
      puts "  transfs add <file> [tags...]"
      puts "  transfs tag <hash> <tags...>"
      puts "  transfs list-tags"
      puts "  transfs list-files [tag]"
      exit 1
    end

    # Run command line interface.
    #
    def run
      # TODO: accept config file argument (useful for testing)
      # TODO: how to handle store name as an argument?
      # NOTE: For now we read config from /etc/transfs.cfg
      #       But we are using a test store for now
      config  = Config.new()
      store   = config.test_store

      command = ARGV.shift? || usage

      case command
      when "add"
        filepath = ARGV.shift? || usage
        tags = ARGV
        hash = TransFS.add_file(store, filepath, tags)
        puts "Added #{filepath} as #{hash.hexstring}"

      when "tag"
        hex = ARGV.shift? || usage
        tags = ARGV
        hash = hex.hexbytes
        TransFS.tag_file(store, hash, tags)
        puts "Tagged #{hex} with: #{tags.join(", ")}"

      when "taglist"
        TransFS.tags(store).each{ |tag| puts tag }

      when "filelist"
        tag = ARGV.shift?
        TransFS.files(store, tag).each { |tag| puts tag }

      when "info"
        puts "Store Name: #{store.name}"
        puts "Location: #{store.root}"
        puts "Database: #{store.database_path}"
        puts "Mount Point: #{store.mountpoint}"

      when "mount"
        # The mount moved to the new claim-log model — use `transfs2 mount`.
        STDERR.puts "mount has moved to the new model: run `transfs2 mount`"
        exit 1

      else
        usage
      end
    end
  end # CLI

end # TransFS
