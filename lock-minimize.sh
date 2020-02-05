#!/usr/bin/bash -ve

cargo -Z minimal-versions generate-lockfile

# failing on windows currently:
# net2 v0.2.32
# ├── hyper v0.13.1
# │   ├── body-image-futio v2.1.0 (/home/david/src/body-image/body-image-futio)
# │   └── hyper-tls v0.4.0
# │       └── body-image-futio v2.1.0 (/home/david/src/body-image/body-image-futio) (*)
# ├── mio v0.6.20
# │   └── tokio v0.2.6
# │       ├── blocking-permit v1.2.0
# │       │   └── body-image-futio v2.1.0 (/home/david/src/body-image/body-image-futio) (*)
# │       ├── body-image-futio v2.1.0 (/home/david/src/body-image/body-image-futio) (*)
# │       ├── h2 v0.2.1
# │       │   └── hyper v0.13.1 (*)
# │       ├── hyper v0.13.1 (*)
# │       ├── hyper-tls v0.4.0 (*)
# │       ├── tokio-tls v0.3.0
# │       │   └── hyper-tls v0.4.0 (*)
# │       └── tokio-util v0.2.0
# │           └── h2 v0.2.1 (*)
# │       [dev-dependencies]
# │       └── body-image-futio v2.1.0 (/home/david/src/body-image/body-image-futio) (*)
# └── miow v0.2.1
#     └── mio v0.6.20 (*)
cargo update -p net2 --precise 0.2.33
