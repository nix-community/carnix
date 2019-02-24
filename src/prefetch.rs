use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use toml::Value;
use std::path::PathBuf;
use std;
use toml;
use std::str::from_utf8;
use std::process::Command;
use std::path::Path;
use serde_json;
use regex::Regex;
use std::borrow::Cow;
use {Error, ErrorKind};
use cache::*;
use krate::*;

impl Crate {

    pub fn prefetch(&self, cache: &mut Cache, source_type: &SourceType) -> Result<Meta, Error> {
        let mut cargo_toml_path = PathBuf::new();
        let prefetch = match *source_type {
            SourceType::Path { ref path, ref workspace_member } => {
                debug!("path: {:?}", path);
                cargo_toml_path.push(path);
                if let Some(ref mem) = *workspace_member {
                    cargo_toml_path.push(mem);
                }
                Prefetch {
                    path: path.to_path_buf(),
                    prefetch: Src::Path {
                        path: path.to_path_buf(),
                        workspace_member: workspace_member.as_ref().map(|x| x.to_path_buf()),
                    }
                }
            }
            SourceType::CratesIO => {
                let prefetch = self.prefetch_path(cache)?;
                cargo_toml_path.push(&prefetch.path);
                prefetch
            },
            SourceType::Git { ref url, ref rev } => {
                let prefetch = self.prefetch_git(url, rev, cache)?;
                cargo_toml_path.push(&prefetch.path);
                prefetch
            },
            _ => panic!("unsupported source {:?}", source_type)
        };

        debug!("src = {:?}", prefetch.prefetch);
        cargo_toml_path.push("Cargo.toml");
        debug!("cargo_toml: {:?}", cargo_toml_path);

        let mut f = std::fs::File::open(&cargo_toml_path)?;
        let mut toml = String::new();
        f.read_to_string(&mut toml).unwrap();
        let mut v:toml::Value = match toml::de::from_str(&toml) {
            Ok(v) => v,
            Err(e) => {
                error!("{:?}: {:?}", cargo_toml_path, e);
                return Err(e.into())
            }
        };
        let v = v.as_table_mut().expect("v not a table");

        let (dependencies, implied, build_dependencies, target_dependencies) = {
            let prefetch_path = match &prefetch.prefetch {
                &Src::Path { ref path, ref workspace_member } => {
                    if let Some(ref ws) = *workspace_member {
                        let mut p = path.to_path_buf();
                        p.push(ws);
                        Cow::Owned(p)
                    } else {
                        Cow::Borrowed(path)
                    }
                },
                _ => Cow::Borrowed(&prefetch.path)
            };
            let (dependencies, implied) = make_dependencies(&prefetch_path, v.get("dependencies"), v.get("features"));
            let (build_dependencies, _) = make_dependencies(&prefetch_path, v.get("build-dependencies"), None);
            debug!("dependencies of {:?} = {:?}", self.name, dependencies);

            let mut target_dependencies = Vec::new();
            if let Some(target) = v.remove("target") {
                let target = if let Value::Table(target) = target {
                    target
                } else {
                    panic!("target not a table")
                };
                debug!("target = {:?}", target);
                for (a, b) in target {
                    debug!("a = {:?}", a);
                    let (dependencies, _) = make_dependencies(&prefetch_path, b.get("dependencies"), None);
                    target_dependencies.push((a, dependencies))
                }
                debug!("target_deps {:?}", target_dependencies);
            }
            (dependencies, implied, build_dependencies, target_dependencies)
        };
        // Grab the authors from Cargo.toml, so we can create the
        // CARGO_PKG_AUTHORS environment variable at build time.
        let edition = v.get("package").and_then(|p| p.get("edition").map(|s| s.to_string()));
        let authors = v.get("package")
            .and_then(|x| x.get("authors"))
            .and_then(|x| x.as_array())
            .map(|x| {
                x.iter()
                    .map(|y| y.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(Vec::new);

        let (default_features, declared_features) = features(v);
        let include = include(v);
        Ok(Meta {
            src: prefetch.prefetch,
            include,
            dependencies,
            declared_dependencies: declared_dependencies(v),
            target_dependencies,
            build_dependencies,
            crate_file: crate_file(v),
            lib_name: lib_name(v),
            proc_macro: is_proc_macro(v),
            plugin: is_plugin(v),
            crate_type: crate_type(v),
            default_features,
            declared_features,
            use_default_features: None,
            features: BTreeSet::new(),
            build: build(v),
            implied_features: implied,
            bins: bins(v),
            authors,
            edition
        })
    }


    fn prefetch_path(&self, cache: &mut Cache) -> Result<Prefetch, Error> {

        debug!("prefetch {:?}", self);
        let version = if self.subpatch.len() > 0 {
            format!("{}.{}.{}{}", self.major, self.minor, self.patch, self.subpatch)
        } else {
            format!("{}.{}.{}", self.major, self.minor, self.patch)
        };
        let url = format!("https://crates.io/api/v1/crates/{}/{}/download", self.name, version);

        let from_cache = cache.get(&url);
        if let Some(ref prefetch) = from_cache {
            if std::fs::metadata(&prefetch.path).is_ok() {
                return Ok(prefetch.clone())
            }
        }

        println!("Prefetching {}-{}", self.name, version);
        debug!("url = {:?}", url);
        let prefetch = Command::new("nix-prefetch-url")
            .args(&[ &url, "--unpack", "--name", &(self.name.clone() + "-" + &version) ][..])
            .output()?;

        let sha256:String = from_utf8(&prefetch.stdout).unwrap().trim().to_string();
        if let Ok(path) = get_path(&prefetch.stderr) {
            let pre = Prefetch {
                prefetch: Src::Crate { sha256 },
                path: Path::new(path).to_path_buf(),
            };
            if from_cache.is_none() {
                cache.insert(&url, pre.clone());
            }
            Ok(pre)
        } else {
            if prefetch.stderr.ends_with(b"HTTP error 404\n") {
                Err(ErrorKind::Prefetch404(self.clone()).into())
            } else {
                Err(ErrorKind::PrefetchFailed(self.clone()).into())
            }
        }
    }




    fn prefetch_git(&self, url: &str, rev: &str, cache: &mut Cache) -> Result<Prefetch, Error> {
        let cached_url = format!("git+{}#{}", url, rev);
        let from_cache = cache.get(&cached_url);
        if let Some(ref prefetch) = from_cache {
            if std::fs::metadata(&prefetch.path).is_ok() {
                return Ok(prefetch.clone())
            }
        }

        println!("Prefetching {} ({})", self.name, cached_url);
        debug!("cached_url = {:?}", cached_url);
        let prefetch = Command::new("nix-prefetch-git")
            .args(&[ "--url", url, "--rev", rev ])
            .output();

        match prefetch {
            Err(e) => {
                error!("error with nix-prefetch-git: {}", e);
                Err(e.into())
            }
            Ok(prefetch) => {

                if prefetch.status.success() {

                    let prefetch_json: GitFetch = serde_json::from_str(from_utf8(&prefetch.stdout).unwrap()).unwrap();
                    let path = get_path(&prefetch.stderr)?;

                    let pre = Prefetch {
                        prefetch: Src::Git(prefetch_json),
                        path: Path::new(path).to_path_buf()
                    };
                    if from_cache.is_none() {
                        cache.insert(&cached_url, pre.clone());
                    }
                    Ok(pre)
                } else {
                    error!("nix-prefetch-git exited with error code {:?}: {:?}", prefetch.status, std::str::from_utf8(&prefetch.stderr));
                    Err(ErrorKind::NixPrefetchGitFailed.into())
                }
            }
        }
    }

}

fn include(v: &BTreeMap<String, toml::Value>) -> Option<Vec<String>> {
    if let Some(inc) = v.get("package") {
        if let Some(inc) = inc.as_table() {
            if let Some(inc) = inc.get("include") {
                if let Some(inc) = inc.as_array() {
                    return Some(inc.into_iter().filter_map(|x| x.as_str().map(|x| x.to_string())).collect())
                }
            }
        }
    }
    None
}

fn crate_file(v: &BTreeMap<String, toml::Value>) -> String {
    if let Some(crate_file) = v.get("lib") {
        let crate_file = crate_file.as_table().unwrap();
        if let Some(lib_path) = crate_file.get("path") {
            lib_path.as_str().unwrap().to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}

fn crate_type(v: &BTreeMap<String, toml::Value>) -> Vec<String> {
    if let Some(crate_file) = v.get("lib") {
        if let Some(crate_file) = crate_file.as_table() {
            if let Some(crate_type) = crate_file.get("crate-type") {
                debug!("crate_type = {:?}", crate_type);
                if let Some(s) = crate_type.as_str() {
                    return vec![s.to_string()]
                } else if let Some(s) = crate_type.as_array() {
                    return s.into_iter().map(|x| x.to_string()).collect()
                }
            }
        }
    }
    Vec::new()
}

fn lib_name(v: &BTreeMap<String, toml::Value>) -> String {
    if let Some(crate_file) = v.get("lib") {
        if let Some(name) = crate_file.get("name") {
            name.as_str().unwrap().to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}


fn bins(v: &mut BTreeMap<String, toml::Value>) -> Vec<Bin> {
    if let Some(toml::Value::Array(bins)) = v.remove("bin") {
        bins.into_iter().map(|mut x| {
            let bin = x.as_table_mut().unwrap();
            Bin {
                name: if let Some(toml::Value::String(s)) = bin.remove("name") { Some(s) } else { None },
                path: if let Some(toml::Value::String(s)) = bin.remove("path") { Some(s) } else { None },
                required_features: if let Some(toml::Value::Array(s)) = bin.remove("required-features") {
                    let mut v = Vec::new();
                    for s in s {
                        if let toml::Value::String(s) = s {
                            v.push(s)
                        }
                    }
                    v
                } else {
                    Vec::new()
                }
            }
        }).collect()
    } else {
        Vec::new()
    }
}

fn is_proc_macro(v: &BTreeMap<String, toml::Value>) -> bool {
    if let Some(crate_file) = v.get("lib") {
        if let Some(&toml::Value::Boolean(proc_macro)) = crate_file.get("proc-macro") {
            proc_macro
        } else {
            false
        }
    } else {
        false
    }
}

fn is_plugin(v: &BTreeMap<String, toml::Value>) -> bool {
    if let Some(crate_file) = v.get("lib") {
        if let Some(&toml::Value::Boolean(plugin)) = crate_file.get("plugin") {
            plugin
        } else {
            false
        }
    } else {
        false
    }
}

fn build(v: &BTreeMap<String, toml::Value>) -> String {
    if let Some(package) = v.get("package") {
        if let Some(build) = package.as_table().unwrap().get("build") {
            return build.as_str().unwrap().to_string()
        }
    }
    String::new()
}

fn features(v: &BTreeMap<String, toml::Value>) -> (Vec<String>, BTreeSet<String>) {
    let mut default_features = Vec::new();
    let mut declared_features = BTreeSet::new();

    if let Some(features) = v.get("features") {
        let features = features.as_table().unwrap();
        if let Some(default) = features.get("default") {
            default_features.extend(
                default.as_array().unwrap().into_iter().map(|x| x.as_str().unwrap().to_string())
            )
        }
        for (f, _) in features.iter() {
            if f != "default" {
                declared_features.insert(f.to_string());
            }
        }
    }
    (default_features, declared_features)
}

fn declared_dependencies(v: &BTreeMap<String, toml::Value>) -> BTreeSet<String> {
    let mut declared_dependencies = BTreeSet::new();
    if let Some(deps) = v.get("dependencies") {
        if let Some(deps) = deps.as_table() {
            for (f, _) in deps.iter() {
                declared_dependencies.insert(f.clone());
            }
        }
    }
    if let Some(deps) = v.get("dev-dependencies") {
        if let Some(deps) = deps.as_table() {
            for (f, _) in deps.iter() {
                declared_dependencies.insert(f.clone());
            }
        }
    }
    declared_dependencies
}

fn get_path(stderr: &[u8]) -> Result<&str, Error> {
    debug!("{:?}", from_utf8(&stderr));
    let path_re = Regex::new("path is (‘|')?([^’'\n]*)(’|')?").unwrap();
    let prefetch_stderr = from_utf8(&stderr).expect("stderr of nix-prefetch-url not utf8");
    let cap = if let Some(cap) = path_re.captures(prefetch_stderr) {
        cap
    } else {
        eprintln!("nix-prefetch-url returned:\n{}", prefetch_stderr);
        return Err(ErrorKind::PrefetchReturnedNothing.into())
    };
    Ok(cap.get(2).unwrap().as_str())
}
