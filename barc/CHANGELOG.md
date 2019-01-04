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
