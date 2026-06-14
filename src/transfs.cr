# TODO: Write documentation for `TransFS`
#
module TransFS
  VERSION = "0.1.0"
end

require "sqlite3"
require "digest/sha256"
require "mime"

# NOTE: fusefs.cr now targets the new claim-log model (Library), not the legacy
# Store, so it is no longer part of this legacy require chain. The legacy
# `transfs` target keeps its non-mount commands until it is retired.
require "./tagfs"
