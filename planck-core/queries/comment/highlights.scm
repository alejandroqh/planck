; Planck's additions, loaded before the upstream query (first match wins).
; Upstream tags these as @hint/@info/@warning/@error, which themes don't color.

((tag (name) @keyword)
  (#match? @keyword "^(TODO|FIXME|BUG|HACK|XXX|NOTE|WARN|WARNING|SAFETY|INFO|PERF)$"))
("text" @keyword
  (#match? @keyword "^(TODO|FIXME|BUG|HACK|XXX|NOTE|WARN|WARNING|SAFETY|INFO|PERF)$"))
