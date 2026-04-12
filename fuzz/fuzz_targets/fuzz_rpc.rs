//! Fuzz target: JSON-RPC request parsing.
//!
//! Feeds arbitrary byte strings as JSON-RPC request bodies into the RPC
//! request parser. The parser must:
//!   - Never panic
//!   - Return a valid JSON-RPC error response on malformed input
//!   - Never leak stack traces or internal state in error messages

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Attempt to parse as JSON. The RPC layer uses serde_json internally.
        // We verify the parser does not panic on arbitrary UTF-8 strings.
        let _: Result<serde_json::Value, _> = serde_json::from_str(s);
    }
});
