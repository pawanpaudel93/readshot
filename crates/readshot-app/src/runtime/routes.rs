//! The message lists `update_message` routes to each sub-handler, as
//! pattern macros so the dispatcher stays short while its `match` stays
//! exhaustive: a variant missing from every list is still a compile error.
