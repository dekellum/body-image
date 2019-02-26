#!/usr/bin/bash -ve

cargo update -p rand                    --precise 0.6.1
cargo update -p clap                    --precise 2.31.2
cargo update -p fern                    --precise 0.5.5
cargo update -p hyper                   --precise 0.12.1
cargo update -p h2                      --precise 0.1.12
cargo update -p http                    --precise 0.1.6
cargo update -p log                     --precise 0.4.5
cargo update -p bytes                   --precise 0.4.8
cargo update -p futures                 --precise 0.1.21
cargo update -p httparse                --precise 1.2.4
cargo update -p hyper-tls               --precise 0.3.0
cargo update -p memmap                  --precise 0.7.0
cargo update -p olio                    --precise 1.0.0
cargo update -p tokio                   --precise 0.1.14
cargo update -p tokio-threadpool        --precise 0.1.10
cargo update -p tokio-timer             --precise 0.2.8
cargo update -p crossbeam-channel       --precise 0.3.6
cargo update -p crossbeam-utils         --precise 0.6.3
cargo update -p lazy_static             --precise 1.0.2
cargo update -p brotli                  --precise 2.2.1
cargo update -p tempfile                --precise 3.0.2
cargo update -p flate2                  --precise 1.0.1
