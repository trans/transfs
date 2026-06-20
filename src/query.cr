module TransFS
  # A parsed mount path (the query-path grammar, docs/architecture.md §7).
  #
  #   components : the tag-walk components (facet-view segments), with the `=`
  #                alias normalized to `/` and split — e.g. `/stars=4/` => ["stars","4"].
  #   doc_view   : true if an odd number of lone `=` toggles appeared (render documents).
  #   doc_name   : in doc view, the trailing segment naming a specific document leaf.
  record Parsed, components : Array(String), doc_view : Bool, doc_name : String?

  # Pure path parser — no FUSE, no DB, so it is unit-testable in isolation. The
  # tag-tree boundary detection (grouping components into tags) needs the index and
  # lives in `Index#walk`; this only separates the lone-`=` view toggles from the
  # walk components and the trailing document name.
  module Query
    def self.parse(path : String) : Parsed
      components = [] of String
      doc_view = false
      doc_name = nil
      path.split('/').reject(&.empty?).each do |seg|
        if seg == "="
          doc_view = !doc_view # a lone `=` toggles the view; `cd ..` / extra `=` flip back
          doc_name = nil
        elsif doc_view
          doc_name = seg # a document name within the current doc listing (last wins)
        else
          # facet view: `=` is an alias for `/`, so normalize then split into components
          seg.gsub('=', '/').split('/').reject(&.empty?).each { |c| components << c }
        end
      end
      Parsed.new(components, doc_view, doc_name)
    end
  end
end
