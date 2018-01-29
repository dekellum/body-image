#![allow(dead_code)]

extern crate failure; // #[macro_use]
extern crate futures;
extern crate hyper;
extern crate tokio_core;
extern crate tempfile;

use failure::Error;

use std::io::Write;
use futures::{Future, Stream};
use futures::future::err as f_err;
use hyper::Client;
use hyper::client::FutureResponse;
use tokio_core::reactor::Core;
use tempfile::tempfile;

fn example() -> Result<(), Error> {
    let mut core = Core::new()?;
    let client = Client::new(&core.handle());

    // hyper::uri::Uri, via std String parse and FromStr
    let uri = "http://gravitext.com".parse()?;

    let fr: FutureResponse = client.get(uri); // FutureResponse
    let work = fr.and_then(|res| {
        // FnOnce(Response) -> IntoFuture<Error=hyper::Error>
        println!("Response: {}", res.status());
        println!("Headers:\n{}", res.headers());
        let f: Box<Future<Item=(), Error=hyper::Error> + Send> =
            match tempfile() {
                Ok(mut tfile) => {
                    Box::new(res.body().for_each( move |chunk| {
                        println!("chunk!");
                        tfile.write_all(&chunk).map_err(From::from)
                    }))
                }
                Err(e) => {
                    Box::new(f_err::<(),_>(hyper::Error::Io(e)))
                }
            };
        f
    });

    core.run(work)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::example;

    #[test]
    fn text_example() {
        match example() {
            Ok(_) => println!("ok"),
            Err(e) => panic!("Error from work: {:?}", e)
        }
    }
}
