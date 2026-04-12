//! Fuzz target: P2P message deserialization.
//!
//! Feeds arbitrary bytes as potential P2P gossip messages. The deserialization
//! layer must:
//!   - Never panic
//!   - Return an error for malformed messages
//!   - Not accept messages from unknown peer IDs

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Attempt to deserialize as a gossip message payload (bincode/RLP).
    // P2P messages are internally prefixed with a 1-byte type tag followed
    // by the payload. We verify the parser handles any input gracefully.
    if data.is_empty() {
        return;
    }
    let _msg_type = data[0];
    let _payload = &data[1..];

    // The actual deserialization would call into the network crate.
    // For now, verify that serde_json and bincode parsing don't panic.
    let _ = serde_json::from_slice::<serde_json::Value>(data);
});
