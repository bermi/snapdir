#![no_main]
//! Fuzz target for the snapdir-core manifest parser.
//!
//! Invariants checked against arbitrary input:
//!
//! 1. **No panic / no UB.** Neither `ManifestEntry::parse_line` (a single line)
//!    nor `Manifest::parse` (a whole document) may panic, overflow, or trigger
//!    undefined behavior on *any* byte string.
//! 2. **Round-trip stability.** If a document parses into a `Manifest`, then
//!    re-rendering it (`to_string()`) and re-parsing must also succeed and must
//!    produce an equal `Manifest` — `parse -> Display -> parse` is a fixed
//!    point. This guards against the format model drifting such that emitted
//!    output is no longer parseable.

use libfuzzer_sys::fuzz_target;

use snapdir_core::{Manifest, ManifestEntry};

fuzz_target!(|data: &[u8]| {
    // The manifest format is UTF-8 text; non-UTF-8 input is simply not a
    // manifest, so there is nothing to exercise.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // Per-line parsing must never panic on arbitrary lines.
    for line in s.lines() {
        let _ = ManifestEntry::parse_line(line);
    }

    // Whole-document parsing must never panic. If it parses, the emitted text
    // must re-parse to an equal manifest (round-trip stability).
    if let Ok(manifest) = Manifest::parse(s) {
        let rendered = manifest.to_string();
        let reparsed = Manifest::parse(&rendered)
            .expect("a manifest emitted by Display must re-parse without error");
        assert_eq!(
            manifest, reparsed,
            "parse -> Display -> parse must round-trip to an equal manifest"
        );
    }
});
