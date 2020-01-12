## 2.0.0 (TBD)
* Change `Compress` implementations default `check_identity` to true (was
  previously introduced, in 1.1.0, with default false for interoperability).

* Upgrade to body-image 2.0.0 including interface change (e.g. `push`).

* Upgrade to http 0.2.0 and bytes 0.5.2 (MSRV 1.39.0)

* Upgrade to olio 1.3.0 (MSRV 1.34.0)

* Upgrade to tao-log 1.0.0

* Update to flate2 [1.0.4, 1.1) dropping constraint introduced in 1.3.0.

* Add brotli-decompressor >= 2.1.2 min transitive upgrade

* With MSRV update, drop `TryFrom` workarounds (MSRV 1.34.0)

* Minimum supported rust version is now 1.39.0 (per above upgrades).

## 1.3.0 (2019-10-1)
* Fix build.rs for `rustc --version` not including git metadata.

* Upgrade to body-image 1.3.0, olio 1.2.0, tempfile 3.1.0 and transitively to
  rand 0.7.0.

* Minimum supported rust version is now 1.32.0 (to match above upgrades).

* Narrow flate2 dependency to < 1.0.10, to avoid MSRV breakage. flate2
  1.0.10 and 1.0.11 releases have MSRV 1.34.0 (alexcrichton/flate2-rs#207)

## 1.2.0 (2019-5-13)
* Make existing `barc::TryFrom` and `TryInto` compatible with the
  `std::convert` equivalents, as stabilized in rust 1.34.0. Furthermore, when
  built with rust 1.34.0+, uses the `std::convert` equivalent traits,
  re-exported as `barc::TryFrom` and `TryInto`.  These re-exports are
  (silently) deprecated. Use `std::convert` traits directly if your MSRV ≥
  1.34.0. This is graded a MINOR-version compatibly hazard because existing
  trait import paths and conversions should continue to work.

* Convert enum `barc::EncodeWrapper` to a struct with private internals, making
  the implementation dependencies _flate2_ and (optional) _brotli_ completely
  private. Add public constructor functions (`EncodeWrapper::plain`, `gzip`,
  `brotli`) to preserve the ability to implement `CompressStrategy`
  externally. The public `EncodeWrapper` struct change is graded a
  MINOR-version compatibility hazard, as implementing `CompressStrategy`
  externally should be rare with older releases.

* Narrow various dependencies to avoid future MINOR versions, for reliability.
  We may subsequently make PATCH releases which _broaden_ private or public
  dependencies to include new MINOR releases found _compatible_. Rationale:
  Cargo fails to consider MSRV for dependency resolution (rust-lang/rfcs#2495),
  and MSRV bumps are common in MINOR releases, including as planned here.

* Increase minimum brotli version to 3.1.0, based on minimal version testing
  with latest tests.

* Add build.rs script to fail fast on an attempt to compile with a rustc below
  MSRV, which remains 1.31.0.

## 1.1.0 (2019-3-6)
* `GzipCompressStrategy` and `BrotliCompressStrategy` (_brotli_ feature) have
  learned to only compress if a record has a minimum length of compressible
  header and body bytes, after discounting any non-compressible body
  bytes. This is determined via several new `CompressStrategy` trait methods
  and implementation: `min_len`, `check_identity`, and `non_compressible_coef`
  getters; and `is_compressible`, using content-type and meta -decoded headers.

* `CompressStrategy::wrap_encoder` only needs a `MetaRecorded` reference for
  the duration of that call. Make the lifetime more lenient.

* Make the read and write implementation generic over `Read` and `Write` types,
  instead of using `dyn Trait` objects, throughout. These changes are mostly
  internal, but include public utility methods `write_headers` and
  `write_body`. Reference types are used to maintain compatibility.

* _Error reform_: remove _failure_ crate dependency:
  * Drop `Fail` implementation of `BarcError` and add `impl StdError for
    BarcError` (aka `std::error::Error`).
  * `BarcError::InvalidHeader` variant's use of `failure::Error`
    (for private encapsulation) replaced with compatible
    `Box<StdError + Send + Sync + 'static>`, type-aliased as `Flaw`.
  * Add `BarcError::IntoDialog` variant wrapper for `DialogConvertError` (also
    now a `StdError`) for convenience in a mixed error context.

  Since `failure::Fail` offers a blanket implementation for `StdError`, this is
  graded a MINOR-version compatibility hazard. Testing of unmodified dependent
  crates/projects supports this conclusion.

* Upgrade to body-image 1.1.0, for use of `BodyReader` as direct `Read`
  implementation.

* Broaden (optional default feature) _brotli_ dependency to >=2.2.1, <4.

* Add logger implementation as dev-dependency for tests. Improve log and test
  output via _tao-log_ crate macros.

## 1.0.1 (2019-1-4)
* Upgrade log dep to reflect 2018 minimal versions.

## 1.0.0 (2018-12-4)
* Update to the rust 2018 edition, including the changes to pass all 2018 idiom
  lints (anchored paths, anonymous/elided lifetimes).  _This start of the 1.x
  release series has a minimum supported rust version of 1.31.0, and is thus
  potentially less stable than prior 0.x releases, which will continue to be
  maintained as needed._

* Separate into its own *barc* crate (see prior history below). This includes
  moving the placeholder `TryFrom` and `TryInto` traits, only used here.

* As of *flate2* 1.0.6, `GzEncoder` now exceeds 200 bytes is size, so `Box` it in
  `EncodeWrapper` for better stack usage and to avoid a clippy lint.

* Remove `barc::META_*` header constants that were deprecated in 0.5.0.

## History in *body-image*

Previously *barc* was released as a module of the *body-image* crate. Relevent
release history is extracted below:

### body-image 0.5.0 (2018-10-19)
* Provide placeholder `body_image::TryFrom` and blanket `TryInto` (still
  awaiting std stabilization), relocate `barc::Record::try_from(Dialog)` to the
  trait, and add new `TryFrom<barc::Record> for Dialog` for the opposite
  conversion. The relocation is a minor breaking change in that current users
  need to either import `body_image::TryFrom` or start using `try_into`. The
  new `barc::Record` → `Dialog` conversion enables using BARC files as test
  fixtures for `Dialog` processing code.

* Deprecate the `barc::META_*` header constants, replacing with `barc::hname_*`
  helper functions which internally use `HeaderName::from_static`. This is more
  ergonomic and found to be somewhat faster. The *http* crate version minimum
  is now 0.1.6.

* Use `dyn Trait` syntax throughout. Minimum supported rust version is now
  1.27.2.

### body-image 0.4.0 (2018-8-15)
* Remove dependency on the *failure_derive* crate, and disable the _derive_
  feature of the *failure* crate dependency, by removing all existing use of
  auto-derive `Fail`.  `BarcError` now has manual implementations of `Display`
  and `Fail`.

### body-image 0.3.0 (2018-6-26)
* Upgrade (optional default) _brotli_ to >=2.2.1, <3.

* Minimal rustc version upgraded to (and CI tested at) 1.26.2 for use
  of `impl Trait` feature.

### body-image 0.2.0 (2018-5-8)
* Memory mapping is now an entirely optional, explicitly called, default
  feature:
  * The `BarcReader` previously mapped large (per `Tunables::max_body_ram`),
    uncompressed bodies. Now it uses `ReadSlice` for concurrent, direct
    positioned read access in this case.

### body-image 0.1.0 (2018-4-17)
* Initial release
