require "./store"

module TransFS

  # Configuration is tied to ``/etc/transfs.cfg` by default.
  # It constructs a set store entries.
  class Config
    @stores : Hash(String,Store)

    def initialize(config_file : String | Nil = nil)
      @config_file = config_file || "/etc/transfs.cfg"
      @stores = parse(@config_file)
    end

    def parse(config_file : String)
      stores = {} of String => Store
      text = File.read(config_file).strip
      lines = text.split("\n")
      lines.each do |line|
        e = line.strip.split(/\s+/)
        name = e[0]
        path = e[1]
        mntp = e[2]
        stores[name] = Store.new(name, path, mntp)
      end
      stores
    end

    def [](name : String)
      @stores[name]
    end

    def default_store
      @stores[0]
    end

    def test_store
      # can't be Nil!
      #@test_store ||= Store.new("test", "./test/store", "./test/mount")
      Store.new("test", "./test/store", "./test/mount")
    end
  end

end
