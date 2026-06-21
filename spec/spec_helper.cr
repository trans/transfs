require "spec"
require "../src/library"
require "../src/check"
require "../src/query" # the mount path parser (not pulled in by the core library)

# A scratch store under a temp dir, cleaned up after the block.
def with_store(&)
  dir = File.tempname("transfs-spec")
  Dir.mkdir_p(dir)
  begin
    yield TransFS::Library.new(dir), dir
  ensure
    FileUtils.rm_rf(dir)
  end
end

# Write *content* to a temp file and yield its path.
def with_file(content : String, &)
  path = File.tempname("transfs-src")
  File.write(path, content)
  begin
    yield path
  ensure
    File.delete(path) if File.exists?(path)
  end
end
