use krate;
error_chain!{
    foreign_links {
        Io(::std::io::Error);
        Utf8(::std::string::FromUtf8Error);
        Toml(::toml::de::Error);
    }
    errors {
        CouldNotTranslateTarget{}
        PrefetchReturnedNothing{}
        Prefetch404(cr: krate::Crate) {
            description("Prefetching a crate returned HTTP error 404. Did you forget to specify `--src`?"),
            display("Prefetching {} returned HTTP error 404. Did you forget to specify `--src`?", cr)
        }
        PrefetchFailed(cr: krate::Crate){
            description("Prefetching a crate failed"),
            display("Prefetching {} failed", cr)
        }
        NoCargoLock {
            description("Cargo.lock could not be found")
        }
        NixPrefetchGitFailed{
            description("nix-prefetch-git failed")
        }
    }
}
