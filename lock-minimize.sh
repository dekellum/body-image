#!/usr/bin/bash -ve

cargo -Z minimal-versions generate-lockfile
cargo update -p miniz-sys --precise 0.1.11
cargo update -p unicase --precise 2.1.0
cargo update -p parking_lot_core --precise 0.6.2
cargo update -p brotli-decompressor --precise 2.1.2
