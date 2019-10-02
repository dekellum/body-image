#!/usr/bin/bash -ve

cargo -Z minimal-versions generate-lockfile

# due to:
# semver v0.1.0
# └── rustc_version v0.1.0
#     [build-dependencies]
#     └── unicase v2.0.0
#         ├── hyperx v0.15.0
#         │   └── body-image-futio v1.3.0 (/home/david/src/body-image/body-image-futio)
#         └── mime v0.3.13
#             └── hyperx v0.15.0 (*)
cargo update -p unicase --precise 2.1.0

# Issue: https://github.com/Amanieu/parking_lot/issues/181
# PR: https://github.com/Amanieu/parking_lot/pull/182
# (minimally awaits parking_lot 0.9.1 release)
# parking_lot_core v0.6.0
# └── parking_lot v0.9.0
#     └── tokio-reactor v0.1.10
#         ├── body-image-futio v1.3.0 (/home/david/src/body-image/body-image-futio)
#         ├── hyper v0.12.20
#         │   ├── body-image-futio v1.3.0 (/home/david/src/body-image/body-image-futio) (*)
#         │   └── hyper-tls v0.3.0
#         │       └── body-image-futio v1.3.0 (/home/david/src/body-image/body-image-futio) (*)
#         ├── tokio v0.1.22
#         │   ├── body-image-futio v1.3.0 (/home/david/src/body-image/body-image-futio) (*)
#         │   └── hyper v0.12.20 (*)
#         └── tokio-tcp v0.1.3
#             └── hyper v0.12.20 (*)
#             [dev-dependencies]
#             └── body-image-futio v1.3.0 (/home/david/src/body-image/body-image-futio) (*)
cargo update -p parking_lot_core --precise 0.6.2

# test failure due to:
# brotli-decompressor v2.1.0
# └── brotli v3.1.0
#    └── body-image-futio v1.3.0 (/home/david/src/body-image/body-image-futio)
cargo update -p brotli-decompressor --precise 2.1.2
