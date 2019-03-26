extern crate version_check;

fn main() {
    match version_check::is_min_version("1.32.0") {
        Some((true, actual_v)) => {
            eprintln!("barc-cli MSRV test passed: {} (actual)", actual_v);
        }
        Some((false, actual_v)) => {
            panic!("barc-cli MSRV is 1.32.0 > {} (this rustc)", actual_v);
        }
        None => {
            panic!("couldn't query version info from rustc");
        }
    }
}
