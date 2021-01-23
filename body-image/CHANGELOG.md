## 2.2.1 (2021-1-23)
* Broaden tempfile dependency to include 3.2.

* Add clippy config for primordial MSRV build.rs and for current MSRV.

## 2.2.0 (2021-1-18)
* Upgrade to bytes 1.0 and lift constraint on http dependency (MSRV 1.39.0).

* Upgrade to olio 1.4.0 (MSRV 1.39.0).

## 2.0.1 (2021-1-17)
* Constrain remove_dir_all transitive dep of tempfile to 0.5.2 to preserve MSRV.

* Constrain http to <0.2.3 to avoid bytes duplicates.

* Misc documentation improvements.

## 2.0.0 (2020-1-13)
* `BodySink::save` is deprecated in preference to a new `BodySink::push`
  method, a better name and with copy-avoiding additional generic bounds.

* Move some `Tunables` (and associated `Tuner` setters) that were unused here,
  to `FutioTunables` of the body-image-futio crate, including: `res_timeout` and
  `body_timeout`.

* Improve `Tunables::clone` efficiency by using `Arc<Path>` as representation of
  `temp_dir` and add `Tunables::temp_dir_rc`.

* Remove BodyReader::as_read, (`BodyReader` implements `Read` directly)
  deprecated since 1.1.0

* Upgrade to http 0.2.0 and bytes 0.5.2 (MSRV 1.39.0)

* Upgrade to olio 1.3.0 (MSRV 1.34.0)

* Minimum supported rust version is now 1.39.0 (per above upgrades).

## 1.3.0 (2019-10-1)
* Fix build.rs for `rustc --version` not including git metadata.

* Upgrade to tempfile 3.1.0 and transitively to rand 0.7.0

* Upgrade to olio 1.2.0. `BodyImage` and `Dialog` are again always `Sync`.

* Minimum supported rust version is now 1.32.0 (to match above upgrades).

## 1.2.0 (2019-5-13)
* Narrow various dependencies to avoid future MINOR versions, for reliability.
  We may subsequently make PATCH releases which _broaden_ private or public
  dependencies to include new MINOR releases found _compatible_. Rationale:
  Cargo fails to consider MSRV for dependency resolution (rust-lang/rfcs#2495),
  and MSRV bumps are common in MINOR releases, including as planned here.

* Add build.rs script to fail fast on an attempt to compile with a rustc below
  MSRV, which remains 1.31.0.

## 1.1.0 (2019-3-6)
* `BodyImage::write_to` and `BodyImage::read_from` are now generic over `Write`
  and `Read` types, respectively, instead of using `dyn Trait`
  objects. Reference types are used to keep this a compatible change.

* `BodyReader` now directly implements `Read`. The `BodyReader::as_read` method
  (returning `&mut dyn Read`) is now deprecated.

* _Error reform:_ Drop `Fail` implementation of `BodyError` (and its _failure_
  crate dependency) and add `impl std::error::Error for BodyError`.
  Since `failure::Fail` offers a blanket implementation for
  `std::error::Error`, this is graded a MINOR-version compatibility hazard.

* Add `Encoding::Compress` (for error handling consistency with this legacy
  format) and `Encoding::Identity` (as an explicit indicator that any
  Transfer/Content-Encoding was decoded).

## 1.0.1 (2019-1-4)
* Upgrade log dep to reflect 2018 minimal versions.

## 1.0.0 (2018-12-4)
* Update to the rust 2018 edition, including the changes to pass all 2018 idiom
  lints (anchored paths, anonymous/elided lifetimes).  _This start of the 1.x
  release series has a minimum supported rust version of 1.31.0, and is thus
  potentially less stable than prior 0.x releases, which will continue to be
  maintained as needed._

* Split out crates *barc*, *barc-cli* and *body-image-futio* (formally the
  *async* module/feature), keeping only the root module types in this crate.

* Add new `Epilog` public-field struct, `Dialog::new(Prolog, Epilog)` and
  `Dialog::explode(self)` returning `(Prolog, Epilog)`.  This preserves private
  fields of `Dialog` while offering easy to construct/consume public structs.

* Add `Dialog::set_res_body_decoded`, for setting a new response body and
  updated the decoded `Encoding`s list.

* Remove `BodyReader::File` variant which was previously deprecated in 0.4.0.

## Monolithic Releases

Previously the *body-image* crate contained modules *async*, *barc*, and the
BARC CLI binary.  The history below is preserved with some redundancy to the
new crate-specific change logs.

### 0.5.0 (2018-10-19)
* Provide placeholder `body_image::TryFrom` and blanket `TryInto` (still
  awaiting std stabilization), relocate `barc::Record::try_from(Dialog)` to the
  trait, and add new `TryFrom<barc::Record> for Dialog` for the opposite
  conversion. The relocation is a minor breaking change in that current users
  need to either import `body_image::TryFrom` or start using `try_into`. The
  new `barc::Record` → `Dialog` conversion enables using BARC files as test
  fixtures for `Dialog` processing code.

* Mark `BodyImage::from_file` and `BodyImage::from_read_slice` as *unsafe* when
  the default *mmap* feature is enabled, since once `BodyImage::mem_map` is
  called, any concurrent write to the file or other file system modifications
  may lead to *Undefined Behavior*. This is a breaking change as compared with
  0.4.0. (#5)

* Drop `async::RequestRecordable` type alias for the `RequestRecorder<B>`
  trait, which was intended to offer backward compatibility (to 0.3.0), but
  wasn't usable. Type aliases to traits can be *defined* but not really *used*
  in rust to date.

* Deprecate the `barc::META_*` header constants, replacing with `barc::hname_*`
  helper functions which internally use `HeaderName::from_static`. This is more
  ergonomic and found to be somewhat faster. The *http* crate version minimum
  is now 0.1.6.

* Replace use of "deadline" with "timeout", per the deprecation of the former in
  tokio-rs/tokio#558, which was released as tokio 0.1.8 (and tokio-timer 0.2.6)
  patch releases.

* Replace use of `tokio::thread_pool::Builder` with the new
  `tokio::runtime::Builder` façade methods. The former was deprecated and the
  latter added in tokio-rs/tokio#645. This was originally released in tokio
  0.1.9. Subsequently tokio 0.1.10 and 0.1.11 were released as (purely semver)
  bug fixes of the former, so make 0.1.11 the new minimum version.

* Use `dyn Trait` syntax throughout. Minimum supported rust version is now
  1.27.2.

### 0.4.0 (2018-8-15)
* The _client_ module and feature are renamed _async_, made a default feature
  and significantly expanded. See the _async_ module subsection below for
  further details. A deprecated _client_ re-export and feature alias remain
  available.

* New `BodyImage::explode` returning an `ExplodedImage` enum for raw access to
  individual states.

* All internal `Mmap` (*mmap* feature) reads have been optimized using the
  concurrent-aware `olio::mem::MemHandle::advise` for `Sequential` access where
  appropriate. As of _olio_ 0.4.0, this is limited to \*nix platforms via
  `libc::posix_madvise`.  This feature comes with compatibility breakage:
  * `BodyImage` and therefore `Dialog` are no-longer `Sync`. They remain
    `Send` with inexpensive `Clone`, so any required changes should be
    minimal.

* Deprecate the `BodyReader::File(ReadPos)` variant, instead using
  `BodyReader::FileSlice(ReadSlice)` for this case. This variant is an
  additional complexity with no performance (see _olio_ crate benchmarks)
  or other usage advantage.

* Remove dependency on the *failure_derive* crate, and disable the _derive_
  feature of the *failure* crate dependency, by removing all existing use of
  auto-derive `Fail`.  `BodyError` now has manual implementations of `Display`
  and `Fail`.

#### _async_ module
* New `AsyncBodyImage` (symmetric with `AsyncBodySink`) implements
 `futures::Stream` and `hyper::body::Payload` traits. The `Payload` trait
 makes this usable with hyper as the `B` body type of `http::Request<B>`
 (client) or `http::Response<B>` (server). The `Stream` trait is sufficient
 for use via `hyper::Body::with_stream`.

* Alternative new `UniBodyImage` and `UniBodySink` implement the same
  traits with a custom `UniBodyBuf` item buffer type, allowing zero-copy
  including when in `MemMap` state, at the the cost of the adjustments
  required for not using the default `hyper::Body` type. (*mmap* feature
  only)

* Both `Tunables` timeouts are now optional, with the initial
  (res)ponse timeout now defaulting to `None`. Thus by default only
  the full body timeout is used in `request_dialog`.

* Replace the only remaining use of `Box<Future>` with `Either` to avoid
  heap allocation.

* Broaden and improve _async_ module tests and catalog by type as _stub_,
  _server_, _futures_, and (non-default, further limited) _live_.

* New benchmarks of `AsyncBodyImage` and `UniBodyImage` stream transfer of
  8MiB bodies, from states `Ram` (also incl. "pregather", as in prior, and
  per-iteration `gather`), `FsRead` incl. various read buffer sizes, and
  `MemMap` (also incl. "pre-" as in prior).

### 0.3.0 (2018-6-26)
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

### 0.2.0 (2018-5-8)
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

### 0.1.0 (2018-4-17)
* Initial release
