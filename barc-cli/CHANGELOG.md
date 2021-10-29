## 2.3.0 (unreleased)
* Minimum supported rust version (MSRV) is now 1.46.0.

* Upgrade barc, body-image and body-image-futio to 2.3.

* Use `strip_prefix` for `--offset` flag, "0x" prefix format.

## 2.2.1 (2021-1-29)
* Use tao-log dependency exclusively, no direct log dep.

## 2.2.0 (2021-1-18)
* Upgrade to body-image and barc 2.2.0 (MSRV 1.39.0).

* Lift prior constraint on http dependency (MSRV 1.39.0).

* Upgrade to body-image-futio 2.2.0 (MSRV 1.45.2).

* Upgrade to hyper 0.14 (MSRV 1.45.2)

* Minimum supported rust version is now 1.45.2, per above upgrades, if any of
  the default features _futio_, _mmap_ or _brotli_ are enabled. Otherwise the
  MSRV remains 1.39.0.

## 2.1.1 (2021-1-17)
* Constrain http to <0.2.3 to avoid bytes duplicates.

* Misc documentation improvements.

## 2.1.0 (2020-2-4)
* Upgrade to body-image-futio 2.1.0.

* Replace fern logger with piccolog.

## 2.0.0 (2020-1-13)
* Upgrade to body-image, barc, and body-image-futio 2.0.0 with interface
  changes (e.g. `FutioTunables`, `TryFrom`).

* Upgrade to http 0.2.0 (MSRV 1.39.0)

* Upgrade to hyper 0.13.1 (MSRV 1.39.0, optional behind futio feature gate)

* With MSRV update, drop `TryFrom` workarounds and use the `std` trait.

* Minimum supported rust version is now 1.39.0 (per above upgrades).

## 1.3.0 (2019-10-1)
* Fix build.rs for `rustc --version` not including git metadata.

* Upgrade to barc, body-image, body-image-futio versions 1.3.0, including
  olio 1.2.0, tempfile 3.1.0, rand 0.7.

* Minimum supported rust version is now 1.32.0 (to match above upgrades).

## 1.2.0 (2019-5-13)
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
