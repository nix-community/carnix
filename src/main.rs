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

use std::io::{BufWriter, Read};
use clap::{App, Arg, ArgMatches, SubCommand, AppSettings};
use std::process::{Command, Stdio};
use std::path::{Path, PathBuf};

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
    let matches =
        App::new("carnix")
            .version(version)
            .author("pmeunier <pe@pijul.org>")
            .about("Generate a nix derivation set from a cargo registry")
            .subcommand(
                SubCommand::with_name("build").arg(
                    Arg::with_name("include")
                        .short("-I")
                        .help("Forwarded to nix-build")
                        .takes_value(true)
                        .multiple(true),
                ),
            )
            .subcommand(
                SubCommand::with_name("run")
                    .setting(AppSettings::TrailingVarArg)
                    .arg(Arg::with_name("include")
                         .short("-I")
                         .help("Forwarded to nix-build")
                         .takes_value(true)
                         .multiple(true))
                    .arg(Arg::with_name("rest")
                         .help("Pass the remaining arguments to the program")
                         .multiple(true)
                         .last(true)),
            )
            .subcommand(
                SubCommand::with_name("generate-nix")
                    .arg(
                        Arg::with_name("src")
                            .long("--src")
                            .help("Source of the main project")
                            .takes_value(true),
                    )
                    .arg(Arg::with_name("standalone").long("--standalone").help(
                        "Produce a standalone file, which can be built directly with nix-build.",
                    ))
            )
            .get_matches();

    if let Some(matches) = matches.subcommand_matches("generate-nix") {
        let cargo_lock = krate::find_cargo_lock().unwrap();
        let mut cargo_nix = cargo_lock.clone();
        cargo_nix.set_extension("nix");
        let mut nix_file = BufWriter::new(std::fs::File::create(&cargo_nix).unwrap());
        let mut cwd = std::env::current_dir().unwrap();
        if let Err(e) = output::generate_nix(
            &cargo_lock,
            matches.is_present("standalone"),
            if let Some(m) = matches.value_of("src") {
                Some(m)
            } else {
                let mut src = None;
                loop {
                    cwd.push("Cargo.lock");
                    if std::fs::metadata(&cwd).is_ok() {
                        cwd.pop();
                        src = cwd.to_str();
                        break
                    }
                    cwd.pop(); // remove "Cargo.lock"
                    if !cwd.pop() {
                        break
                    }
                }
                src
            },
            &mut nix_file,
        ) {
            eprintln!("{}", e);
            std::process::exit(1)
        }
    } else if let Some(matches) = matches.subcommand_matches("build") {
        build(matches).unwrap();
    } else if let Some(matches) = matches.subcommand_matches("run") {
        let path = build(matches).unwrap();
        let mut bindir = Path::new(path.trim()).join("bin");
        let mut dir = std::fs::read_dir(&bindir).unwrap()
            .filter(|x| if let Ok(ref x) = *x {
                match x.path().extension() {
                    None => true,
                    Some(x) => x == "exe"
                }
            } else {
                false
            });
        if let (Some(bin), None) = (dir.next(), dir.next()) {
            let bin = bin.unwrap();
            let args = if let Some(rest) = matches.values_of("rest") {
                rest.collect()
            } else {
                Vec::new()
            };
            let status = Command::new(&bin.path())
                .args(args)
                .status()
                .unwrap();
            std::process::exit(status.code().unwrap())
        }
    }
}

fn needs_nix_file(current: &mut PathBuf) -> bool {
    current.push("Cargo.nix");
    if let Ok(meta) = std::fs::metadata(&current) {
        current.pop();
        current.push("Cargo.lock");
        if let Ok(lock_meta) = std::fs::metadata(&current) {
            current.pop();
            return meta.modified().unwrap() < lock_meta.modified().unwrap()
        }
        current.pop();
        current.push("Cargo.toml");
        if let Ok(toml_meta) = std::fs::metadata(&current) {
            current.pop();
            return meta.modified().unwrap() < toml_meta.modified().unwrap()
        }
    }
    current.pop();
    true
}

fn build(matches: &ArgMatches) -> Result<String> {
    Command::new("cargo").args(&["generate-lockfile"]).status()?;

    let current = krate::find_cargo_lock()?;
    // current contains the root of the Cargo.lock.
    let mut nix = current.clone();
    nix.pop();
    let needs_nix = needs_nix_file(&mut nix);
    nix.push("Cargo.nix");
    if needs_nix {
        let mut nix_file = BufWriter::new(std::fs::File::create(&nix)?);
        if let Err(e) = output::generate_nix(&current, true, current.parent(), &mut nix_file) {
            eprintln!("{}", e);
            std::process::exit(1)
        }
    }

    let import = format!(
        "(import {}{}).__all",
        if nix.is_relative() { "./" } else { "" },
        &nix.to_string_lossy()
    );
    let mut args = vec!["-E", &import];
    if let Some(i) = matches.values_of("include") {
        for i in i {
            args.push("-I");
            args.push(i);
        }
    }
    let mut child = Command::new("nix-build")
        .args(&args)
        .stdout(Stdio::piped())
        .spawn()?;
    let mut result = String::new();
    child.wait()?;
    child.stdout.unwrap().read_to_string(&mut result)?;
    print!("{}", result);
    Ok(result)
}
