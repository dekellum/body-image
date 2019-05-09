## 1.2.0 (TBD)
* Narrow various dependencies to avoid future MINOR versions, for reliability.
  We may subsequently make PATCH releases which _broaden_ private or public
  dependencies to include new MINOR releases found _compatible_. Rationale:
  Cargo fails to consider MSRV for dependency resolution (rust-lang/rfcs#2495),
  and MSRV bumps are common in MINOR releases, including as planned here.  Also
  binary crates like barc-cli, can't yet ship and use their own Cargo.lock for
  resolution (rust-lang/cargo#5654).

* Add build.rs script to fail fast on an attempt to compile with a rustc below
  MSRV, which remains 1.31.0.

## 1.1.0 (2019-3-6)
* _Error reform_: Remove _failure_ crate dependency, internally replacing
  `failure::Error` with `Box<StdError + Send + Sync + 'static>` (type-aliased
  as `Flaw`).

* Upgrade to body-image 1.1.0, body-image-futio 1.1.0 for improved response
  decoding support and an error for unsupported encodings, for which this CLI
  can now provide better context.

* Upgrade to barc 1.1.0 for `CompressStrategy::set_check_identity` support, to
  avoid double, non-productive compression.

## 1.0.1 (2019-1-4)
* Upgrade log dep to reflect 2018 minimal versions.

## 1.0.0 (2018-12-4)
* Update to the rust 2018 edition, including the changes to pass all 2018 idiom
  lints (anchored paths, anonymous/elided lifetimes).  _This start of the 1.x
  release series has a minimum supported rust version of 1.31.0, and is thus
  potentially less stable than prior 0.x releases, which will continue to be
  maintained as needed._

* Separate into its own *barc-cli* crate (see prior history in the *body-image*
  crate).
