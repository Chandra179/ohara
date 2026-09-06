//! Document registry: milestone state, dedup-hash lookups, re-crawl scheduling
//! (§5 `documents`, §7.5). Access functions land with the control-store build step
//! (§15 step 2) alongside the enqueue → SCRAPE flow that needs them.
