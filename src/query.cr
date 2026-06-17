module TransFS
  # One narrowing predicate parsed from a mount path segment (the query-path
  # grammar, docs/architecture.md §7).
  #
  #   key == nil  => a BARE value, matched across the value space (the type, a
  #                  boolean-tag key, or a tag value).
  #   key != nil  => a PINNED facet `key=value` (a structural column or a tag).
  record Predicate, key : String?, value : String

  # Parses a mount path into an ordered list of predicates. Pure — no FUSE, no
  # DB — so it is unit-testable in isolation and reusable by a future CLI `<q>`
  # resolver. Order is irrelevant to matching (segments commute, AND-of-all) but
  # preserved so the mount can pick out the final segment for leaf detection.
  module Query
    def self.parse(path : String) : Array(Predicate)
      path.split('/').reject(&.empty?).map { |seg| segment(seg) }
    end

    # Classify one segment. The first '=' splits key=value (identical to
    # Index#split_tag, so the navigate-spelling equals the tag-creation
    # spelling). A *leading* '=' (empty key) is the breakdown/enumerate marker —
    # DEFERRED to a later slice — so for now it degrades to a bare value rather
    # than introducing that code path.
    def self.segment(seg : String) : Predicate
      if (i = seg.index('=')) && i > 0
        Predicate.new(seg[0, i], seg[(i + 1)..])
      else
        Predicate.new(nil, seg)
      end
    end
  end
end
