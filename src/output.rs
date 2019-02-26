use std::path::Path;
use toml;
use std;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use regex::Regex;
use std::path::PathBuf;
use itertools::Itertools;
use dirs;
use Error;
use cfg;
use krate::*;
use cache::*;

struct FeatName<'a>(&'a str);
impl<'a> std::fmt::Display for FeatName<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.0.chars().all(|x| {
            (x >= '0' && x <= '9') || (x >= 'a' && x <= 'z') || (x >= 'A' && x <= 'Z') || x == '_'
                || x == '-'
        }) {
            write!(f, "{}", self.0)
        } else {
            write!(f, "\"{}\"", self.0)
        }
    }
}

#[derive(Debug)]
enum CrateType {
    Workspace {
        members: BTreeMap<String, (Crate, PathBuf)>,
        package: Option<(String, (Crate, PathBuf))>,
    },
    Single(String, (Crate, PathBuf))
}

enum Either<A, B> {
    A(A),
    B(B)
}

impl<Item, A:Iterator<Item = Item>, B:Iterator<Item = Item>> Iterator for Either<A, B> {
    type Item = Item;
    fn next(&mut self) -> Option<Self::Item> {
        match *self {
            Either::A(ref mut a) => a.next(),
            Either::B(ref mut b) => b.next(),
        }
    }
}

impl CrateType {
    fn is_workspace(&self) -> bool {
        if let CrateType::Workspace { .. } = *self {
            true
        } else {
            false
        }
    }
    fn get(&self, key: &str) -> Option<&(Crate, PathBuf)> {
        match *self {
            CrateType::Workspace { members: ref x, ref package } => {
                if let Some(x) = x.get(key) {
                    Some(x)
                } else if let Some(ref p) = *package {
                    if p.0 == key {
                        Some(&p.1)
                    } else {
                        None
                    }
                } else {
                    None
                }
            },
            CrateType::Single(ref name, ref cr) if name == key => Some(cr),
            CrateType::Single(_, _) => None,
        }
    }
    fn source_type<P: AsRef<Path>>(&self, src: Option<P>, name: &str) -> SourceType {
        if let Some(&(_, ref path)) = self.get(name) {
            if let Some(ref src) = src {
                if self.is_workspace() {
                    SourceType::Path {
                        path: src.as_ref().to_path_buf(),
                        workspace_member: Some(path.to_path_buf()),
                    }
                } else {
                    SourceType::Path {
                        path: src.as_ref().to_path_buf(),
                        workspace_member: None,
                    }
                }
            } else {
                SourceType::CratesIO
            }
        } else {
            SourceType::None
        }
    }
    fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a String, &'a (Crate, PathBuf))> {
        match *self {
            CrateType::Workspace { ref members, ref package } =>
                Either::A(
                    members.iter().chain(package.iter().map(|&(ref a, ref b)| (a, b)))
                ),
            CrateType::Single(ref name, ref cr) => {
                Either::B(
                    std::iter::once((name, cr))
                )
            }
        }
    }
}

fn workspace_members(base_path: &Path, cargo_toml: &toml::Value) -> Option<CrateType> {
    let package = if let Some(package) = cargo_toml.get("package") {
        let name = package.get("name").unwrap();
        let cra = get_package_version(&package);
        Some((
            name.as_str().unwrap().to_owned(),
            (cra, base_path.to_path_buf()),
        ))
    } else {
        None
    };

    if let Some(ws) = cargo_toml.get("workspace") {
        let mut workspace_members = BTreeMap::new();
        let members = ws.as_table()
            .unwrap()
            .get("members")
            .unwrap()
            .as_array()
            .unwrap();
        for mem in members {
            let mem = mem.as_str().unwrap();
            let mut path = base_path.join(mem);
            path.push("Cargo.toml");
            let mut toml: toml::Value = {
                let mut cargofile = std::fs::File::open(&path).unwrap();
                let mut toml = String::new();
                cargofile.read_to_string(&mut toml).unwrap();
                toml::de::from_str(&toml).unwrap()
            };
            if let Some(package) = toml.as_table_mut().unwrap().remove("package") {
                {
                    let name = package.get("name").unwrap();
                    let cra = get_package_version(&package);
                    debug!("mem path {:?}", path);
                    path.pop();
                    workspace_members.insert(
                        name.as_str().unwrap().to_owned(),
                        (cra, path)
                    );
                }
            }
        }
        return Some(CrateType::Workspace { members: workspace_members, package })
    }

    package.map(|(a, b)| CrateType::Single(a, b))
}

pub fn generate_nix<W: Write, P: AsRef<Path>, Q: AsRef<Path>>(
    lockfile: P,
    standalone: bool,
    src: Option<Q>,
    nix_file: W,
) -> Result<BTreeMap<Crate, Meta>, Error> {
    let mut cr = lockfile.as_ref().to_path_buf();
    let mut lock: toml::Value = {
        let mut lockfile = std::fs::File::open(&cr).unwrap();
        let mut toml = String::new();
        lockfile.read_to_string(&mut toml).unwrap();
        toml::de::from_str(&toml).unwrap()
    };

    let mut packages = if let Some(package) = lock.as_table_mut().unwrap().remove("package") {
        if let toml::Value::Array(package) = package {
            package
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    cr.set_extension("toml");

    let cargo_toml: toml::Value = {
        let mut cargofile = std::fs::File::open(&cr).unwrap();
        let mut toml = String::new();
        cargofile.read_to_string(&mut toml).unwrap();
        toml::de::from_str(&toml).unwrap()
    };

    let (mut deps, _) = {
        make_dependencies(
            cr.parent().unwrap(),
            cargo_toml.get("dependencies"),
            cargo_toml.get("features"),
        )
    };

    let base_path = cr.parent().unwrap();

    let workspace_members =
        workspace_members(base_path, &cargo_toml).expect("neither workspace nor regular crate");
    debug!("ws members {:?}", workspace_members);

    debug!("main deps: {:?} {:?}", cr, deps);

    // Loading the cache (a map between package versions and Nix store paths).
    let mut cache_path = dirs::home_dir().unwrap();
    cache_path.push(".cargo");
    std::fs::create_dir_all(&cache_path).unwrap();
    cache_path.push("nix-cache");
    let mut cache = Cache::new(&cache_path)?;


    // Compute the meta-information for all packages in the lock file.
    debug!("base_path: {:?}", base_path);

    let main_meta = {
        let root = if let Some(root) = lock.as_table().unwrap().get("root") {
            Some(root)
        } else if let Some(name) = cargo_toml.get("package").and_then(|package| package.as_table().and_then(|package| package.get("name"))) {
            packages.iter().find(|x| x.get("name").unwrap() == name)
        } else {
            None
        };

        if let Some(root) = root {
            let main_cra = get_package_version(&root);

            debug!("name = {:?}", main_cra.name);
            if let Some(dep) = deps.get(&main_cra.name) {
                debug!("dep = {:?}", dep);
            }

            let path = workspace_members.source_type(src.as_ref(), &main_cra.name);
            let mut meta = match main_cra.prefetch(&mut cache, &path) {
                Ok(meta) => meta,
                Err(e) => {
                    std::mem::drop(cache);
                    return Err(e)
                }
            };
            if let Some(inc) = cargo_toml
                .get("package")
                .unwrap()
                .as_table()
                .unwrap()
                .get("include")
            {
                meta.include = Some(
                    inc.as_array()
                        .unwrap()
                        .into_iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect(),
                )
            }
            if let Some(ref src) = src {
                meta.src = Src::Path {
                    path: src.as_ref().to_path_buf(),
                    workspace_member: None,
                }
            }
            Some(meta)
        } else {
            None
        }
    };

    let mut all_packages = match fixpoint(
        &mut cache,
        src,
        &mut packages,
        &mut deps,
        &workspace_members,
        main_meta.as_ref(),
    ) {
        Ok(pack) => pack,
        Err(e) => {
            std::mem::drop(cache);
            return Err(e)
        }
    };

    // Make sure we save the cache.
    std::mem::drop(cache);

    // Adding the root crate, the one we're compiling.
    if let Some(mut main_meta) = main_meta {
        if let Some(root) = lock.as_table_mut().unwrap().get("root") {
            debug!("root = {:?}", root);
            let main_cra = get_package_version(&root);
            // Add the crates' dependencies.
            if let Some(deps) = root.get("dependencies") {
                update_deps(&main_cra, deps, &mut main_meta);
            }
            if let Some(deps) = root.get("build-dependencies") {
                update_deps(&main_cra, deps, &mut main_meta);
            }
            all_packages.insert(main_cra.clone(), main_meta);
        }
    }

    output(standalone, workspace_members, &all_packages, base_path, nix_file)?;
    Ok(all_packages)
}

fn cargo_lock_source_type(package: &toml::Value) -> SourceType {
    if let Some(source) = package.get("source") {
        let source = source.as_str().unwrap();
        if source == "registry+https://github.com/rust-lang/crates.io-index" {
            return SourceType::CratesIO
        }

        if source.starts_with("git+") {
            return parse_git(source)
        }
    }
    SourceType::None
}

fn local_source_type<P:AsRef<Path>>(
    crate_type: &CrateType,
    src: Option<P>,
    main_meta: Option<&Meta>,
    deps: &BTreeMap<String, Dep>,
    name: &str,
) -> SourceType {
    debug!("src = {:?}", src.as_ref().map(|x| x.as_ref()));
    debug!("crate_type: {:?}", crate_type);
    let loc = crate_type.source_type(src.as_ref(), name);
    debug!("loc: {:?}", loc);
    if let SourceType::None = loc {
        if let Some(ref dep) = deps.get(name) {
            if let Some(ref p) = dep.path {
                debug!("path {:?}", p);
                return SourceType::Path {
                    path: p.to_path_buf(),
                    workspace_member: None,
                };
            }
        } else if let Some(ref main_meta) = main_meta {
            for &(_, ref deps) in main_meta.target_dependencies.iter() {
                if let Some(ref dep) = deps.get(name) {
                    if let Some(ref p) = dep.path {
                        debug!("path {:?}", p);
                        return SourceType::Path {
                            path: p.to_path_buf(),
                            workspace_member: None,
                        };
                    }
                }
            }
        }
    }
    loc
}

fn resolve_replaces(packages: &mut Vec<toml::Value>) -> Vec<toml::Value> {
    // resolve all "replace = "
    let mut replace = BTreeMap::new();
    for pack in packages.iter() {
        if let (Some(repl), Some(source)) = (pack.get("replace"), pack.get("source")) {
            replace.insert(
                repl.as_str().unwrap().to_string(),
                source.as_str().unwrap().to_string(),
            );
        }
    }
    let mut fix = Vec::new();
    for mut pack in packages.drain(..) {
        if pack.get("replace").is_none() {
            let s = format!(
                "{} {}",
                pack.get("name").unwrap().as_str().unwrap(),
                pack.get("version").unwrap().as_str().unwrap()
            );
            if let Some(repl) = replace.get(&s) {
                let mut pack = pack.as_table_mut().unwrap();
                pack.insert("source".to_string(), toml::Value::String(repl.clone()));
                debug!("replaced {:?}", pack);
            }
            fix.push(pack)
        }
    }
    fix
}

fn fixpoint<P: AsRef<Path>>(
    cache: &mut Cache,
    src: Option<P>,
    packages: &mut Vec<toml::Value>,
    deps: &mut BTreeMap<String, Dep>,
    crate_type: &CrateType,
    main_meta: Option<&Meta>,
) -> Result<BTreeMap<Crate, Meta>, Error> {
    let mut all_packages = BTreeMap::new();

    // Here we need to compute a fixpoint of dependencies to resolve
    // all sources. This is because local sources are sometimes
    // referenced not only from this package's Cargo.toml, but from
    // this package's (transitive) dependencies, in which case the
    // only way to know where they are is to go down the dependency
    // tree (because they are not in the Cargo.lock).
    let mut packages_fixpoint = resolve_replaces(packages);
    let mut unknown_packages = packages; // empty at this point.

    while !packages_fixpoint.is_empty() {
        let mut at_least_one_resolved = false;
        for package in packages_fixpoint.drain(..) {
            let cra = get_package_version(&package);

            // We first try to resolve the source type using the Cargo.lock.
            let mut source_type = cargo_lock_source_type(&package);
            debug!("name = {:?} {:?}", cra, source_type);

            // Then try from local paths.
            if let SourceType::None = source_type {
                source_type = local_source_type(crate_type, src.as_ref(), main_meta, &deps, &cra.name)
            }

            // Finally, we prefetch the package (if available), and
            // set its source field, if the package is local.
            let meta = {
                debug!("path = {:?}", source_type);
                if source_type != SourceType::None {
                    match cra.prefetch(cache, &source_type) {
                        Ok(mut prefetch) => Some(prefetch),
                        Err(e) => return Err(e)
                    }
                } else {
                    None
                }
            };

            debug!("meta = {:?}", meta);
            if let Some(mut meta) = meta {
                // If we've successfully retrieved the package's
                // meta-information (i.e. parsed the part of our
                // Cargo.lock related to that package, and the
                // package's own Cargo.toml), we might have learned
                // exact versions of the package's dependencies from
                // the Cargo.lock, and we need to update them.
                at_least_one_resolved = true;
                for (a, b) in meta.dependencies
                    .iter()
                    .chain(meta.build_dependencies.iter())
                {
                    debug!("inserting dep for {:?}: {:?}", a, b);
                    deps.insert(a.to_string(), b.clone());
                }

                if let Some(deps) = package.get("dependencies") {
                    update_deps(&cra, deps, &mut meta)
                }
                if let Some(deps) = package.get("build-dependencies") {
                    update_deps(&cra, deps, &mut meta)
                }
                if let Some(target) = package.get("target") {
                    for package in target.as_array().unwrap().iter() {
                        if let Some(deps) = package.get("dependencies") {
                            update_deps(&cra, deps, &mut meta)
                        }
                    }
                }

                all_packages.insert(cra, meta);
            } else {
                // Else, add the package back to the fixpoint. If we
                // have learned about other dependencies during this
                // pass of the fixpoint, we might have updated `deps`,
                // so we might be able to resolve this package later.
                if all_packages.get(&cra).is_none() {
                    unknown_packages.push(package)
                } else {
                    error!(
                        "Several packages with the same name and version number {:?}",
                        cra
                    )
                }
            }
        }
        if !at_least_one_resolved {
            error!("Could not resolve some sources, unknown packages:");
            for package in unknown_packages.iter() {
                if let Some(name) = package.get("name") {
                    error!("  - {}", name.as_str().unwrap());
                } else {
                    error!("  - {:?}", package)
                }
            }
            std::process::exit(1)
        }
        std::mem::swap(&mut packages_fixpoint, &mut unknown_packages)
    }

    Ok(all_packages)
}

pub fn write_crates_io(all_packages: &BTreeMap<Crate, Meta>, names: &BTreeSet<String>) -> Result<(), Error> {
    let mut extra_crates_io = std::io::BufWriter::new(std::fs::File::create("crates-io.nix")?);
    extra_crates_io.write_all(b"{ lib, buildRustCrate, buildRustCrateHelpers }:
with buildRustCrateHelpers;
let inherit (lib.lists) fold;
    inherit (lib.attrsets) recursiveUpdate;
in
rec {\n
")?;
    for (cra, meta) in all_packages.iter() {
        if let Src::Crate { .. } = meta.src {
            cra.output_package(Path::new(""), &mut extra_crates_io, 2, &meta, &names, "")?;
        }
    }
    extra_crates_io.write_all(b"}\n")?;
    Ok(())
}

fn output<W: Write>(
    standalone: bool,
    workspace_members: CrateType,
    all_packages: &BTreeMap<Crate, Meta>,
    root_prefix: &Path,
    mut nix_file: W,
) -> Result<(), Error> {
    // And output.
    let mut command_line = String::new();
    for arg in std::env::args() {
        if !command_line.is_empty() {
            command_line.push_str(" ");
        }
        command_line.push_str(&arg);
    }
    nix_file.write_all(
        format!(
            "# Generated by carnix {}: {}\n",
            env!("CARGO_PKG_VERSION"),
            command_line
        ).as_bytes(),
    )?;

    if standalone {
        nix_file.write_all(b"with import <nixpkgs> {};\n")?;
    } else {
        nix_file.write_all(b"{ lib, buildPlatform, buildRustCrate, buildRustCrateHelpers, cratesIO, fetchgit }:\n")?;
    }
    nix_file.write_all(b"with buildRustCrateHelpers;\nlet inherit (lib.lists) fold;\n    inherit (lib.attrsets) recursiveUpdate;\n")?;
    if standalone {
        nix_file.write_all(b"  cratesIO = (callPackage ./crates-io.nix { });\n")?;
    }
    nix_file.write_all(b"in\n")?;
    let mut names = BTreeSet::new();
    for (cra, _) in all_packages.iter() {
        names.insert(cra.name.clone());
    }

    write_crates_io(&all_packages, &names)?;
    {
        // Write a list of packages and versions, to be used by
        // `carnix generate-crates-io`.
        let mut f = std::fs::File::create("crates-io.list")?;
        for (cra, meta) in all_packages.iter() {
            if let Src::Crate { .. } = meta.src {
                writeln!(f, "{}", cra)?;
            }
        }
    }
    nix_file.write_all(b"rec {\n")?;
    nix_file.write_all(b"  crates = cratesIO")?;
    let mut is_first = true;
    for (cra, meta) in all_packages.iter() {
        if let Src::Crate { .. } = meta.src {
        } else {
            if is_first {
                nix_file.write_all(b" // rec {\n")?;
                is_first = false;
            }
            cra.output_package(root_prefix, &mut nix_file, 4, &meta, &names, "cratesIO.")?;
        }
    }
    if is_first {
        nix_file.write_all(b";\n")?;
    } else {
        // If we wrote at least one line, close the opened attribute set.
        nix_file.write_all(b"\n  };\n\n")?;
    }


    let mut all = b"[ ".to_vec();
    for (_, &(ref cra, _)) in workspace_members.iter() {

        let name = nix_name(&cra.name);
        let version = format!(
            "{}.{}.{}{}",
            cra.major,
            cra.minor,
            cra.patch,
            cra.subpatch
        );
        writeln!(
            nix_file,
            "  {} = crates.crates.{}.\"{}\" deps;",
            name,
            name,
            version,
        )?;

        write!(&mut all, "({} {{}}) ", name)?;
    }

    write!(&mut all, "]")?;

    nix_file.write_all(b"  __all = ")?;
    nix_file.write_all(&all)?;
    nix_file.write_all(b";\n")?;

    for (cra, meta) in all_packages.iter() {
        cra.output_package_call(&mut nix_file, 2, &meta)?;
    }
    nix_file.write_all(b"}\n")?;
    Ok(())
}

/// Add the dependencies from Cargo.lock.
fn update_deps(cra: &Crate, deps: &toml::Value, meta: &mut Meta) {
    let dep_re = Regex::new(r"^(\S*) (\d*)\.(\d*)\.(\d*)([\+\-]\S*)?(.*)?").unwrap();
    let mut deps_names = BTreeSet::new();
    for dep in deps.as_array().unwrap() {
        let dep = dep.as_str().unwrap();
        let cap = dep_re.captures(&dep).unwrap();
        deps_names.insert(cap.get(1).unwrap().as_str());
        debug!("updating deps for {:?}: {:?}", cra.name, dep);
        let (a, b, c, d) = {
            (
                cap.get(2).unwrap().as_str().parse().unwrap(),
                cap.get(3).unwrap().as_str().parse().unwrap(),
                cap.get(4).unwrap().as_str().parse().unwrap(),
                cap.get(5)
                    .map(|x| x.as_str().to_string())
                    .unwrap_or(String::new()),
            )
        };
        let from_crates_io = if let Some(source) = cap.get(6) {
            source.as_str() == " (registry+https://github.com/rust-lang/crates.io-index)"
        } else {
            false
        };
        let name = cap.get(1).unwrap().as_str().to_string();
        debug!("update name = {:?}", name);
        if let Some(entry) = meta.dependencies.get_mut(&name) {
            debug!("meta.dependencies");
            entry.cr.major = a;
            entry.cr.minor = b;
            entry.cr.patch = c;
            entry.cr.subpatch = d.clone();
            entry.cr.name = name.clone();
            entry.cr.found_in_lock = true;
            entry.from_crates_io = from_crates_io;
        }
        if let Some(entry) = meta.build_dependencies.get_mut(&name) {
            debug!("meta.build_dependencies");
            entry.cr.major = a;
            entry.cr.minor = b;
            entry.cr.patch = c;
            entry.cr.subpatch = d.clone();
            entry.cr.name = name.clone();
            entry.cr.found_in_lock = true;
            entry.from_crates_io = from_crates_io;
        }
        for &mut (_, ref mut deps) in meta.target_dependencies.iter_mut() {
            if let Some(entry) = deps.get_mut(&name) {
                debug!("meta.target_dependencies");
                entry.cr.major = a;
                entry.cr.minor = b;
                entry.cr.patch = c;
                entry.cr.subpatch = d.clone();
                entry.cr.name = name.clone();
                entry.cr.found_in_lock = true;
                entry.from_crates_io = from_crates_io;
            }
        }
    }
}

fn get_package_version(package: &toml::Value) -> Crate {
    let mut name = package
        .as_table()
        .unwrap()
        .get("name")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let version = package.get("version").unwrap().as_str().unwrap();
    name.push_str("-");
    name.push_str(version);
    name.parse().unwrap()
}

impl Crate {
    pub fn output_package_call<W: Write>(
        &self,
        mut w: W,
        n_indent: usize,
        meta: &Meta,
    ) -> Result<(), Error> {
        let mut indent = String::new();
        for _ in 0..n_indent {
            indent.push(' ');
        }
        let version = format!(
            "{}.{}.{}{}",
            self.major,
            self.minor,
            self.patch,
            self.subpatch
        );
        // debug!("output_package_call {:?}", full_name);
        let nix_name_ = nix_name(&self.name);

        write!(w, "{}deps.{}.\"{}\" = {{",
               indent,
               nix_name_,
               version)?;
        let mut is_first = true;
        let mut h = BTreeSet::new();
        for deps in std::iter::once(&meta.dependencies)
            .chain(std::iter::once(&meta.build_dependencies))
            .chain(meta.target_dependencies.iter().map(|&(_, ref y)| y))
        {
            for (_, dep) in deps.iter().filter(|&(_, dep)| dep.cr.found_in_lock) {

                if h.contains(&dep.cr) {
                    continue
                }
                debug!("outputting dep = {:?}", dep);
                if is_first {
                    writeln!(w, "")?;
                }
                is_first = false;
                writeln!(w,"{}  {} = \"{}.{}.{}{}\";",
                         indent,
                         nix_name(&dep.cr.name),
                         dep.cr.major, dep.cr.minor, dep.cr.patch,
                         dep.cr.subpatch
                )?;
                h.insert(&dep.cr);
            }
        }
        if !is_first {
            writeln!(w, "{}}};", indent)?;
        } else {
            writeln!(w, "}};")?;
        }
        Ok(())
    }

    pub fn output_package_features<W: Write>(
        &self,
        mut w: W,
        n_indent: usize,
        meta: &Meta,
        prefix: &str,
    ) -> Result<(), Error> {

        let mut indent = String::new();
        for _ in 0..n_indent {
            indent.push(' ');
        }

        let version =
            format!(
                "{}.{}.{}{}",
                self.major, self.minor, self.patch, self.subpatch
            );

        writeln!(
            w,
            "{}features_.{}.\"{}\" = deps: f: updateFeatures f (rec {{",
            indent, nix_name(&self.name), version
        )?;
        let mut output_features = BTreeMap::new();
        let nix_name_ = nix_name(&self.name);
        let full_name_default = format!("{}.\"{}\".default", nix_name_, version);
        if meta.use_default_features == Some(false) {
        } else {
            let e = output_features
                .entry(full_name_default.clone())
                .or_insert(Vec::new());
            e.push(format!("(f.{}.\"{}\".default or true)", nix_name_, version))
        }
        if !meta.implied_features.is_empty() {
            for feat in meta.implied_features.iter() {
                let dep = format!("{}.\"{}\".\"{}\"", nix_name_, version, FeatName(&feat.dep_feature));
                let mut e = output_features.entry(dep).or_insert(Vec::new());
                e.push(format!(
                    "(f.{}.\"{}\".{} or false)",
                    nix_name_,
                    version,
                    FeatName(&feat.feature)
                ));
                e.push(format!(
                    "({}.\"{}\".\"{}\" or false)",
                    nix_name_,
                    version,
                    FeatName(&feat.feature)
                ));
            }
        }

        let mut seen = BTreeSet::new();
        let mut default = BTreeMap::new();
        for deps in std::iter::once(&meta.dependencies)
            .chain(std::iter::once(&meta.build_dependencies))
            .chain(meta.target_dependencies.iter().map(|&(_, ref y)| y))
        {
            for (_, dep) in deps.iter().filter(|&(_, dep)| dep.cr.found_in_lock) {
                debug!("outputting dep = {:?}", dep);
                let dep_name = format!(
                    "{}.\"${{deps.{}.\"{}\".{}}}\"",
                    nix_name(&dep.cr.name),
                    nix_name_,
                    version,
                    nix_name(&dep.cr.name),
                );
                for feat in dep.features.iter() {
                    let mut e = output_features.entry(format!("{}.\"{}\"", dep_name, feat)).or_insert(Vec::new());
                    e.push("true".to_string());
                }

                if !dep.conditional_features.is_empty() {
                    for feat in dep.conditional_features.iter() {
                        if !seen.contains(&(&dep.cr.name, &feat.feature)) {
                            let dep_name = format!(
                                "{}.\"{}\"",
                                dep_name,
                                FeatName(&feat.dep_feature),
                            );
                            let mut e = output_features.entry(dep_name).or_insert(Vec::new());
                            e.push(format!(
                                "({}.\"{}\".\"{}\" or false)",
                                nix_name_,
                                version,
                                FeatName(&feat.feature)
                            ));
                            e.push(format!(
                                "(f.\"{}\".\"{}\".\"{}\" or false)",
                                nix_name_,
                                version,
                                FeatName(&feat.feature)
                            ));
                            seen.insert((&dep.cr.name, &feat.feature));
                        }
                    }
                }
                if !dep.default_features {
                    if default.get(&dep_name).is_none() {
                        default.insert(dep_name, false);
                    }
                } else {
                    default.insert(dep_name, true);
                }
            }
        }
        for (dep_name, default) in default.iter() {
            let dep = format!("{}.default", dep_name);
            let mut e = output_features.entry(dep).or_insert(Vec::new());
            e.push(format!("{}", *default));
        }

        let mut current_prefix = "";
        let mut current_attrs = Vec::new();

        for (a, b) in output_features.iter() {

            if !b.is_empty() {
                if current_prefix.is_empty() || !a.starts_with(current_prefix) {
                    // change current prefix
                    if !current_prefix.is_empty() {
                        // close previous prefix
                        if current_attrs.len() == 1 {
                            writeln!(w, "{}  {}.{}", indent, current_prefix.trim_right_matches('.'), current_attrs.pop().unwrap())?;
                        } else if current_attrs.len() > 1 {
                            writeln!(w, "{}  {} = fold recursiveUpdate {{}} [", indent, current_prefix.trim_right_matches('.'))?;
                            for x in current_attrs.drain(..) {
                                writeln!(w, "{}    {{ {} }}", indent, x)?;
                            }
                            writeln!(w, "{}  ];", indent)?;
                        }
                    }
                    // open new prefix
                    if let Some(i) = a.find('.') {
                        current_prefix = a.split_at(i+1).0;
                    }
                }
            }

            if b.len() == 0 {
            } else if b.len() == 1 {

                if b[0] == "true" || a == &full_name_default {
                    current_attrs.push(format!("{} = {};", a.trim_left_matches(current_prefix), b[0]))
                } else if b[0] != "false" {
                    current_attrs.push(format!("{} = (f.{} or false) || {};", a.trim_left_matches(current_prefix), a, b[0]))
                } else if a.ends_with(".default") {
                    // b[0] == false here
                    current_attrs.push(format!("{} = (f.{} or false);", a.trim_left_matches(current_prefix), a))
                }
            } else {
                let mut x = format!("{} =\n{}      (f.{} or false)", a.trim_left_matches(current_prefix), indent, a);
                for bb in b.iter() {
                    x.push_str(&format!(" ||\n{}      {}", indent, bb));
                }
                x.push_str(&format!(";"));
                current_attrs.push(x)
            }
        }
        if !current_prefix.is_empty() {
            // close previous prefix
            if current_attrs.len() == 1 {
                writeln!(w, "{}  {}.{}", indent, current_prefix.trim_right_matches('.'), current_attrs.pop().unwrap())?;
            } else if current_attrs.len() > 1 {
                writeln!(w, "{}  {} = fold recursiveUpdate {{}} [", indent, current_prefix.trim_right_matches('.'))?;
                for x in current_attrs.drain(..) {
                    writeln!(w, "{}    {{ {} }}", indent, x)?;
                }
                writeln!(w, "{}  ];", indent)?;
            }
        }

        write!(w, "{}}}) [", indent)?;
        let mut at_least_one = false;
        for (_, deps) in std::iter::once(("", &meta.dependencies))
            .chain(std::iter::once(("_build", &meta.build_dependencies)))
            .chain(meta.target_dependencies.iter().map(|&(_, ref y)| ("", y)))
        {
            for (_, dep) in deps.iter().filter(|&(_, c)| {
                c.cr.found_in_lock && c.cr.name.len() > 0
            }) {
                at_least_one = true;
                write!(
                    w,
                    "\n{}  ({}features_.{}.\"${{deps.\"{}\".\"{}\".\"{}\"}}\" deps)",
                    indent,
                    if dep.from_crates_io { prefix } else { "" },
                    nix_name(&dep.cr.name),
                    nix_name_,
                    version,
                    nix_name(&dep.cr.name),
                )?;
            }
        }
        if at_least_one {
            writeln!(w, "\n{}];\n\n", indent)?;
        } else {
            writeln!(w, "];\n\n")?;
        }
        Ok(())
    }

    pub fn output_package<W: Write>(
        &self,
        root_prefix: &Path,
        mut w: W,
        n_indent: usize,
        meta: &Meta,
        all_packages: &BTreeSet<String>,
        prefix: &str,
    ) -> Result<(), Error> {
        let mut indent = String::new();
        for _ in 0..n_indent {
            indent.push(' ');
        }

        let nix_name_ = nix_name(&self.name);
        let version =
            format!(
                "{}.{}.{}{}",
                self.major, self.minor, self.patch, self.subpatch
            );

        writeln!(w, "# {}-{}\n", self.name, version)?;
        write!(w, "{}crates.{}.\"{}\" = deps: {{ features?(features_.{}.\"{}\" deps {{}}) }}: buildRustCrate {{\n",

               indent,
               nix_name(&self.name),
               version,
               nix_name(&self.name),
               version,
        )?;

        writeln!(w, "{}  crateName = \"{}\";", indent, self.name)?;

        writeln!(w, "{}  version = \"{}\";", indent, version)?;
        if let Some(ref descr) = meta.description {
            writeln!(w, "{}  description = \"{}\";", indent, descr)?;
        }
        writeln!(
            w,
            "{}  authors = [ {} ];",
            indent,
            meta.authors.iter().map(|s| format!("\"{}\"", s)).join(" ")
        )?;

        if let Some(ref edition) = meta.edition {
            writeln!(w, "{}  edition = {};", indent, edition)?;
        }

        match meta.src {
            Src::Crate { ref sha256 } => {
                writeln!(w, "{}  sha256 = \"{}\";", indent, sha256)?;
            }
            Src::Path {
                ref path,
                ref workspace_member,
            } => {
                debug!("path = {:?}", path);
                let path_canon = path.canonicalize()?;
                let path = if let Ok(path) = path_canon.strip_prefix(root_prefix) {
                    path
                } else {
                    &path
                };
                let workspace_member =
                    workspace_member.as_ref().map(|x| {
                        if let Ok(ws) = x.strip_prefix(root_prefix) {
                            ws
                        } else {
                            &x
                        }
                    });
                let s = path.to_string_lossy();

                let mut filter_source = String::new();
                debug!("fsmeta {:?}", path);
                if let Ok(fsmeta) = std::fs::metadata(&path_canon) {
                    debug!("fsmeta = {:?}", fsmeta);
                    if fsmeta.is_dir() {
                        if let Some(ref include) = meta.include {
                            filter_source.push_str("include [ ");
                            for file in include.iter() {
                                filter_source.push_str("\"");
                                filter_source.push_str(file);
                                filter_source.push_str("\"");
                                filter_source.push_str(" ");
                            }
                            filter_source.push_str("] ");
                        } else if let Some(ref ws) = workspace_member {
                            filter_source.push_str("include [ \"Cargo.toml\" \" ");
                            filter_source.push_str(&ws.to_string_lossy());
                            filter_source.push_str("\" ] ");
                        } else {
                            filter_source.push_str("exclude [ \".git\" \"target\" ] ");
                        }
                    }
                }
                if s.is_empty() {
                    filter_source.push_str("./.");
                } else {
                    if !s.contains("/") {
                        filter_source.push_str("./");
                    }
                    filter_source.push_str(&path.to_string_lossy());
                }

                writeln!(w, "{}  src = {};", indent, filter_source)?;

                if let Some(ref ws) = workspace_member {
                    writeln!(
                        w,
                        "{}  workspace_member = \"{}\";",
                        indent,
                        ws.to_string_lossy()
                    )?;
                }
            }
            Src::Git(ref git) => {
                writeln!(w, "{}  src = fetchgit {{", indent)?;
                writeln!(w, "{}     url = {:?};", indent, git.url)?;
                writeln!(w, "{}     rev = {:?};", indent, git.rev)?;
                writeln!(w, "{}     sha256 = {:?};", indent, git.sha256)?;
                if !git.fetch_submodules {
                    writeln!(w, "{}     fetchSubmodules = false;", indent)?;
                }
                writeln!(w, "{}  }};", indent)?;
            }
        }
        if meta.crate_file.len() > 0 {
            writeln!(w, "{}  libPath = \"{}\";", indent, meta.crate_file)?;
        }
        if meta.lib_name.len() > 0 && meta.lib_name != self.name {
            writeln!(w, "{}  libName = \"{}\";", indent, meta.lib_name)?;
        }
        if meta.proc_macro {
            writeln!(w, "{}  procMacro = {};", indent, meta.proc_macro)?;
        }
        if meta.plugin {
            writeln!(w, "{}  plugin = {};", indent, meta.plugin)?;
        }
        if !meta.crate_type.is_empty() {
            write!(w, "{}  type = [ ", indent)?;
            for t in &meta.crate_type {
                write!(w, "{} ", t)?;
            }
            writeln!(w, "];")?;
        }
        if meta.bins.len() > 0 {
            writeln!(w, "{}  crateBin =", indent)?;
            for (i, bin) in meta.bins.iter().enumerate() {
                if i != 0 {
                    write!(w, " ++\n")?;
                }
                if bin.required_features.is_empty() {
                    write!(w, "{}    [{{ ", indent)?;
                } else {
                    write!(w, "{}    (if ", indent)?;
                    for (i, r) in bin.required_features.iter().enumerate() {
                        if i != 0 {
                            write!(w, "&& ")?;
                        }
                        write!(w, "features.\"{}\".\"{}\".\"{}\" ", nix_name_, version, r)?;
                    }
                    write!(w, "then [{{ ")?;
                }
                if let Some(ref name) = bin.name {
                    write!(w, " name = \"{}\"; ", name)?;
                }
                if let Some(ref path) = bin.path {
                    write!(w, " path = \"{}\"; ", path)?;
                }
                if bin.required_features.is_empty() {
                    write!(w, "}}]")?;
                } else {
                    write!(w, "}} ] else [])")?;
                }
            }
            writeln!(w, ";")?;
        }
        if meta.build.len() > 0 {
            writeln!(w, "{}  build = \"{}\";", indent, meta.build)?;
        }



        let mut is_first = true;
        let mut has_feature_deps = false;
        if !meta.dependencies.is_empty() {
            is_first = false;
            write!(w, "{}  dependencies =", indent)?;
            has_feature_deps |= print_deps(
                &mut w,
                &indent,
                &nix_name_,
                &version,
                prefix,
                meta.dependencies
                    .iter()
                    .filter(|&(_, c)| {
                        c.cr.found_in_lock && c.cr.name.len() > 0
                            && all_packages.contains(&c.cr.name)
                    })
                    .map(|x| x.1),
            )?;
        }
        if !meta.target_dependencies.is_empty() {
            for &(ref target, ref dep) in meta.target_dependencies.iter() {
                if is_first {
                    write!(w, "{}  dependencies = (", indent)?;
                } else {
                    write!(w, "\n{}    ++ (", indent)?;
                }
                debug!("target = {:?}", target);
                let parsed = cfg::parse_target(target)?;
                write!(w, "if ")?;
                cfg::to_nix(&mut w, &parsed)?;
                write!(w, " then")?;
                has_feature_deps |= print_deps(
                    &mut w,
                    &indent,
                    &nix_name_,
                    &version,
                    prefix,
                    dep.iter()
                        .filter(|&(_, c)| {
                            c.cr.found_in_lock && c.cr.name.len() > 0
                                && all_packages.contains(&c.cr.name)
                        })
                        .map(|x| x.1),
                )?;
                write!(w, " else [])")?;
                is_first = false;
            }
        }
        if !meta.dependencies.is_empty() || !meta.target_dependencies.is_empty() {
            writeln!(w, ";")?;
        }

        if !meta.build_dependencies.is_empty() {
            write!(w, "\n{}  buildDependencies =", indent)?;
            print_deps(
                &mut w,
                &indent,
                &nix_name_,
                &version,
                prefix,
                meta.build_dependencies
                    .iter()
                    .filter(|&(_, c)| {
                        c.cr.found_in_lock && c.cr.name.len() > 0
                            && all_packages.contains(&c.cr.name)
                    })
                    .map(|x| x.1),
            )?;
            writeln!(w, ";")?;
        }
        if !meta.declared_features.is_empty() || has_feature_deps {
            write!(
                w,
                "{}  features = mkFeatures (features.\"{}\".\"{}\" or {{}});\n",
                indent,
                nix_name_,
                version
            )?;
        }
        writeln!(w, "{}}};", indent)?;


        self.output_package_features(&mut w, n_indent, meta, prefix)?;
        writeln!(w, "# end")?;
        Ok(())
    }
}

fn print_deps<'a, W: Write, I: Iterator<Item = &'a Dep>>(
    mut w: W,
    indent: &str,
    nix_name_: &str,
    version: &str,
    prefix: &str,
    deps: I,
) -> Result<bool, std::io::Error> {
    writeln!(w, " mapFeatures features ([")?;
    let mut at_least_one = false;
    let mut feature_deps = Vec::new();
    for i in deps {
        at_least_one = true;
        debug!("print_deps i {:?}", i);
        if i.is_optional {
            feature_deps.push(i)
        } else {
            writeln!(
                w,
                "{}    ({}crates.\"{}\".\"${{deps.\"{}\".\"{}\".\"{}\"}}\" deps)",
                indent,
                if i.from_crates_io { prefix } else { "" },
                nix_name(&i.cr.name),
                nix_name_,
                version,
                nix_name(&i.cr.name),
            )?;
        }
    }

    if at_least_one {
        write!(w, "{}  ]", indent)?;
    } else {
        write!(w, "]")?;
    }
    for i in feature_deps.iter() {
        let i_name = nix_name(&i.cr.name);
        write!(
            w,
            "\n{}    ++ (if features.{}.\"{}\".{} or false then [ ({}crates.{}.\"${{deps.\"{}\".\"{}\".{}}}\" deps) ] else [])",
            indent,
            nix_name_,
            version,
            FeatName(&i.cr.name),
            if i.from_crates_io { prefix } else { "" },
            i_name,
            nix_name_,
            version,
            i_name,
        )?;
    }
    write!(w, ")")?;
    Ok(!feature_deps.is_empty())
}
