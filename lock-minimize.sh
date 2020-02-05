#!/usr/bin/bash -ve

cargo -Z minimal-versions generate-lockfile

# net2 v0.2.32 or 33 fails to build on windows, with older winapi:
# ├── hyper v0.13.1
# ├── mio v0.6.20
# └── miow v0.2.1
#     └── mio v0.6.20 (*)
# error[E0432]: unresolved import `winapi::shared::ws2ipdef::SOCKADDR_IN6_LH`
#   --> C:\Users\appveyor\.cargo\registry\src\github.com-1ecc6299db9ec823\net2-0.2.33\src\sys\windows\mod.rs:38:13
#    |
# 38 |     pub use winapi::shared::ws2ipdef::SOCKADDR_IN6_LH as sockaddr_in6;
#    |             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ no `SOCKADADR_IN6_LH` in `shared::ws2ipdef
cargo update -p winapi:0.3.0 --precise=0.3.6
