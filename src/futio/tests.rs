#[cfg(test)]
mod tests {
    #[cfg(feature = "mmap")]        mod futures;
                                    mod server;

    /// These tests may fail because they depend on public web servers
    #[cfg(feature = "may_fail")]    mod live;
}
