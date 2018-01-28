#![allow(dead_code)]

extern crate failure; // #[macro_use]

extern crate futures;
extern crate hyper;
extern crate tokio_core;

use failure::Error;

use std::io::{self, Write};
use futures::{Future, Stream};
use hyper::Client;
use tokio_core::reactor::Core;

fn example() -> Result<(), Error> {
    let mut core = Core::new()?;
    let client = Client::new(&core.handle());

    // hyper::uri::Uri, via std String parse and FromStr
    let uri = "http://gravitext.com".parse()?;
    let work = client.get(uri).and_then(|res| {
        println!("Response: {}", res.status());

        res.body().for_each(|chunk| {
            io::stdout()
                .write_all(&chunk)
                .map_err(From::from)
        })
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
            Err(e) => println!("{:?}", e)
        }
    }
}
