module TransFS

  # Models a File Store.
  #
  class Store
    @name : String
    @fp   : String
    @mp   : String
    @db   : DB::Database | Nil

    DATABASE_FILENAME = "files.db"

    def initialize(name, fp, mp=nil)
      @name = name
      @fp = File.expand_path(fp)
      @mp = File.expand_path(mp)
      @db = nil
    end

    def name
      @name
    end

    # TODO: Is this a bad idea? might it effect concurency?
    #       Or maybe it doesn't actually matter.
    def database
      @db ||= database_handle
    end

    # Concurencey safe.
    def database_handle
      DB.open("sqlite3://#{database_path}")
    end

    def database_path
      File.join(@fp, DATABASE_FILENAME)
    end

    def root
      @fp
    end

    def mountpoint() : String
      @mp
    end
  end

 end
