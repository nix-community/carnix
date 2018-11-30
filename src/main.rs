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
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate tempdir;
extern crate toml;
extern crate dirs;
extern crate url;

use std::io::{BufWriter, Read, BufRead};
use clap::{App, Arg, ArgMatches, SubCommand, AppSettings};
use std::process::{Command, Stdio};
use std::path::{Path, PathBuf};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
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
                SubCommand::with_name("generate-crates-io")
                    .arg(Arg::with_name("file")
                         .help("Generate a crates-io.nix file from a list of packages")
                         .takes_value(true))
            )
            .subcommand(
                SubCommand::with_name("merge")
                    .arg(Arg::with_name("file")
                         .help("Merge two crates-io.nix files")
                         .takes_value(true))
            )
            .subcommand(
                SubCommand::with_name("build")
                    .arg(Arg::with_name("include")
                         .short("-I")
                         .help("Forwarded to nix-build")
                         .takes_value(true)
                         .multiple(true))
                    .arg(Arg::with_name("release")
                         .help("Compile in release mode"))
                    .arg(Arg::with_name("member")
                         .long("--member")
                         .takes_value(true)
                         .help("Select which derivation to compile")),
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
                         .last(true))
                    .arg(Arg::with_name("release")
                         .help("Compile in release mode"))
                    .arg(Arg::with_name("member")
                         .long("--member")
                         .takes_value(true)
                         .help("Select which derivation to compile")),
            )
            .subcommand(
                SubCommand::with_name("generate-nix")
                    .arg(
                        Arg::with_name("src")
                            .long("--src")
                            .help("Source of the main project")
                            .takes_value(true)
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
        if let Err(e) = output::generate_nix(
            &cargo_lock,
            matches.is_present("standalone"),
            matches.value_of("src"),
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
    } else if let Some(matches) = matches.subcommand_matches("generate-crates-io") {
        let file: Box<std::io::Read> = if let Some(f) = matches.value_of("file") {
            Box::new(std::fs::File::open(f).unwrap())
        } else {
            Box::new(std::io::stdin())
        };
        let mut file = std::io::BufReader::new(file);
        let mut s = String::new();
        let mut crates = BTreeMap::new();

        let mut cache_path = dirs::home_dir().unwrap();
        cache_path.push(".cargo");
        std::fs::create_dir_all(&cache_path).unwrap();
        cache_path.push("nix-cache");
        let mut cache = cache::Cache::new(&cache_path).unwrap();

        loop {
            s.clear();
            match file.read_line(&mut s) {
                Ok(n) if n > 0 => {
                    let mut krate: krate::Crate = s.parse().unwrap();
                    krate.found_in_lock = true;
                    let mut meta = krate.prefetch(&mut cache, &krate::SourceType::CratesIO).unwrap();
                    for (_, dep) in meta.dependencies.iter_mut().chain(meta.build_dependencies.iter_mut()) {
                        dep.cr.found_in_lock = true
                    }
                    for (_, dep) in meta.target_dependencies.iter_mut() {
                        for (_, dep) in dep.iter_mut() {
                            dep.cr.found_in_lock = true
                        }
                    }
                    crates.insert(krate, meta);
                }
                _ => break
            }
        }
        std::mem::drop(cache);
        let names: BTreeSet<_> = crates.iter().map(|(ref x, _)| x.name.clone()).collect();
        output::write_crates_io(&crates, &names).unwrap();
    } else if let Some(matches) = matches.subcommand_matches("merge") {
        let file: Box<std::io::Read> = if let Some(f) = matches.value_of("file") {
            Box::new(std::fs::File::open(f).unwrap())
        } else {
            Box::new(std::io::stdin())
        };
        let mut file = std::io::BufReader::new(file);
        let mut title = String::new();
        let mut contents = String::new();
        let mut s = String::new();
        let mut crates = BTreeMap::new();
        loop {
            let n = file.read_line(&mut s).unwrap();
            if n == 0 {
                break
            }
            debug!("s {:?}", s);
            if s == "# end\n" {
                contents.push_str(&s);
                crates.insert(std::mem::replace(&mut title, String::new()),
                              std::mem::replace(&mut contents, String::new()));
            } else if s.starts_with ("# ") {
                title.push_str(&s);
                contents.push_str(&s);
            } else if !title.is_empty() {
                contents.push_str(&s);
            }
            s.clear();
        }
        println!("{{ lib, buildRustCrate, buildRustCrateHelpers }}:
with buildRustCrateHelpers;
let inherit (lib.lists) fold;
    inherit (lib.attrsets) recursiveUpdate;
in
rec {{");
        for (_, b) in crates.iter() {
            print!("{}", b)
        }
        println!("}}");
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

    let import = if let Some(member) = matches.value_of("member") {
        format!("((import {}{}).{} {{}}).override {{ release = {}; }}",
                if nix.is_relative() { "./" } else { "" },
                &nix.to_string_lossy(),
                member,
                matches.is_present("release"),
        )
    } else {
        format!("map (x: x.override {{ release = {}; }}) (import {}{})._all",
                matches.is_present("release"),
                if nix.is_relative() { "./" } else { "" },
                &nix.to_string_lossy(),
        )
    };
    debug!("{:?}", import);
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
