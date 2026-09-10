# C24-02 template cache — minor cleanup, not promoted

Coordinator reopened the complete TemplateStore and re-ran cache/refresh/read_template search (11 matching lines). The worker is correct that resolve always reads disk, writes a duplicate map, and never reads that cached result; list refreshes the map before reading. Removing the map and async lock while keeping eager load validation and transient ordered listings is a valid local cleanup.

No stale result is consumed, no material memory/latency workload is established, and no behavioral fault follows from this unobserved copy. The small lock/map deletion does not meet this audit's materiality threshold for a separate tracked implementation. Retain it as a cleanup note; do not create a caching abstraction or change fresh-file behavior. F19 separately fixes the reachable accepted-form data collision.
