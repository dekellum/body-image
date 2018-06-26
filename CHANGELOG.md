## 0.3.0 (TBD)
* Updates to _client_ module for better composability with _tokio_
  applications and fully asynchronous response body support (#2):
  * Upgrade to _hyper_ 0.12.x and _tokio_ (reform, 0.1.x).
  * New `AsyncBodySink` implements `futures::Sink` for `BodySink`,
    supporting fully asynchronous receipt of a `hyper::Body` request
    or response stream.
  * New `request_dialog` function returns `impl Future<Item=Dialog>`
    given a `RequestRecord` and suitable `hyper::Client` reference.
  * Timeout support via _tokio-timer_ and `Tunables::res_timeout` and
    `body_timeout` durations.

* Upgrade (optional default) _brotli_ to >=2.2.1, <3.

* Minimal rustc version upgraded to (and CI tested at) 1.26.2 for use
  of `impl Trait` feature.

* Remove methods that were deprecated in 0.2.0: `*::try_clone`,
  `BodyImage::prepare`. (#3)

* Add thread name/id logging to cli and client integration tests
  (optionally via env TEST_LOG number).

## 0.2.0 (2018-5-8)
* Concurrent `FsRead` readers and infallible `BodyImage::clone`
  support (#1):
  * `BodyReader` uses `ReadPos`, from new _olio_ crate, for the
    `FsRead` state.
  * `BodyImage`, `Dialog` and `Record` now implement infallible
    `Clone::clone`. The `try_clone` methods are deprecated.
  * `BodyImage::prepare` is now no-op, deprecated.

* Memory mapping is now an entirely optional, explicitly called,
  default feature:
  * Previously `try_clone` automatically _upgraded_ clones to `MemMap`,
    and `write_to` created a temporary mapping, both to avoid
    concurrent mutation of `FsRead`. Now `ReadPos` makes this
    unnecessary.
  * The `BarcReader` previously mapped large (per
    `Tunables::max_body_ram`), uncompressed bodies. Now it uses
    `ReadSlice` for concurrent, direct positioned read access in this
    case.

* Add `BodyImage::from_file` (and `from_read_slice`) conversion
  constructors.

## 0.1.0 (2018-4-17)
* Initial release
