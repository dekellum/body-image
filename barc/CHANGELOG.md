## 1.0.0 (TBD)

* Update to the rust 2018 edition, including the changes to pass all 2018 idiom
  lints (anchored paths, anonymous/elided lifetimes).  _This makes this start
  of the 1.x release series dependent on rust 1.31 (currently beta) and thus
  less stable than prior 0.x releases, which will continue to be maintained as
  needed._

* Seperate into its own *barc* crate (see prior history below). This includes
  moving the placeholder `TryFrom` and `TryInto` traits, only used here.

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
* All internal `Mmap` (*mmap* feature) reads have been optimized using the
  concurrent-aware `olio::mem::MemHandle::advise` for `Sequential` access where
  appropriate. As of _olio_ 0.4.0, this is limited to \*nix platforms via
  `libc::posix_madvise`.  This feature comes with compatibility breakage:
  * `BodyImage` and therefore `Dialog` are no-longer `Sync`. They remain
    `Send` with inexpensive `Clone`, so any required changes should be
    minimal.

* Remove dependency on the *failure_derive* crate, and disable the _derive_
  feature of the *failure* crate dependency, by removing all existing use of
  auto-derive `Fail`.  `BarcError` now has manual implementations of `Display`
  and `Fail`.

### body-image 0.3.0 (2018-6-26)
* Upgrade (optional default) _brotli_ to >=2.2.1, <3.

* Minimal rustc version upgraded to (and CI tested at) 1.26.2 for use
  of `impl Trait` feature.

* Remove methods that were deprecated in 0.2.0: `*::try_clone`,
  `BodyImage::prepare`. (#3)

### body-image 0.2.0 (2018-5-8)

* Memory mapping is now an entirely optional, explicitly called,
  default feature:
  * The `BarcReader` previously mapped large (per
    `Tunables::max_body_ram`), uncompressed bodies. Now it uses
    `ReadSlice` for concurrent, direct positioned read access in this
    case.

* Add `BodyImage::from_file` (and `from_read_slice`) conversion
  constructors.

### body-image 0.1.0 (2018-4-17)
* Initial release