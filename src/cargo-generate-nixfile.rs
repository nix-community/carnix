#[macro_use]
extern crate clap;
extern crate env_logger;
#[macro_use]
extern crate error_chain;
extern crate itertools;
#[macro_use]
extern crate log;
#[macro_use]
extern crate nom;
extern crate regex;
extern crate rusqlite;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate tempdir;
extern crate toml;
extern crate dirs;

use std::io::BufWriter;
use clap::{App, Arg, SubCommand};
use std::process::Command;

mod error;
pub use error::*;
mod cache;
mod prefetch;
mod krate;
mod cfg;
mod output;

fn main() {
    env_logger::init();
    let version = crate_version!();
    let matches = App::new("cargo-generate-nixfile")
        .version(version)
        .author("pmeunier <pe@pijul.org>")
        .about("Generate a nix derivation set from a cargo registry")
        .after_help("Example compilation:\n  $ cargo generate-nixfile --src ./. --standalone\n  $ nix-build -E \"(import ./Cargo.nix).__all\"")
        .subcommand(SubCommand::with_name("generate-nixfile")
                    .arg(
                        Arg::with_name("src")
                            .long("--src")
                            .help("Source of the main project, the default is to fetch from crates.io.")
                            .takes_value(true),
                    )
                    .arg(
                        Arg::with_name("standalone")
                            .long("--standalone")
                            .help("Produce a standalone file, which can be built directly with nix-build."),
                    )
        )
        .get_matches();
    if let Some(matches) = matches.subcommand_matches("generate-nixfile") {
        Command::new("cargo")
            .args(&["generate-lockfile"])
            .status()
            .unwrap();
        let cargo_lock = krate::find_cargo_lock().unwrap();
        let mut cargo_nix = cargo_lock.clone();
        cargo_nix.set_extension("nix");
        let mut nix_file = BufWriter::new(std::fs::File::create(&cargo_nix).unwrap());
        if let Err(e) = output::generate_nix(
            &cargo_lock,
            matches.is_present("standalone"),
            matches.value_of("src"),
            &mut nix_file,
        ) {
            eprintln!("{}", e);
            std::process::exit(1)
        }
    }
}
