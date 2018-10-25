use std::collections::{BTreeMap, BTreeSet};
use toml;
use std::path::{Path, PathBuf};
use regex::Regex;
use std;
use {Error, ErrorKind};

#[derive(Debug, Clone, PartialEq, Eq, Default, PartialOrd, Ord)]
pub struct Crate {
    pub name: String,
    pub major: usize,
    pub minor: usize,
    pub patch: usize,
    pub subpatch: String,
    pub found_in_lock: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Meta {
    pub src: Src,
    pub include: Option<Vec<String>>,
    pub dependencies: BTreeMap<String, Dep>,
    pub declared_dependencies: BTreeSet<String>,
    pub target_dependencies: Vec<(String, BTreeMap<String, Dep>)>,
    pub build_dependencies: BTreeMap<String, Dep>,
    pub crate_file: String,
    pub lib_name: String,
    pub proc_macro: bool,
    pub plugin: bool,
    pub crate_type: Vec<String>,
    pub default_features: Vec<String>,
    pub declared_features: BTreeSet<String>,
    pub use_default_features: Option<bool>,
    pub build: String,
    pub features: BTreeSet<String>,
    pub implied_features: Vec<ConditionalFeature>,
    pub bins: Vec<Bin>,
    pub authors: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Default, Clone)]
pub struct Dep {
    pub cr: Crate,
    pub from_crates_io: bool,
    pub is_optional: bool,
    pub path: Option<PathBuf>,
    pub features: Vec<String>,
    pub default_features: bool,
    pub conditional_features: Vec<ConditionalFeature>,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub enum Src {
    Crate { sha256: String },
    Path { path: PathBuf, workspace_member: Option<PathBuf> },
    Git(GitFetch),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SourceType {
    CratesIO,
    None,
    Git { url: String, rev: String },
    Path { path: PathBuf, workspace_member: Option<PathBuf> }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ConditionalFeature {
    pub feature: String,
    pub dep_feature: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Bin {
    pub path: Option<String>,
    pub name: Option<String>,
    pub required_features: Vec<String>,
}

use std::str::FromStr;
impl FromStr for Crate {
    type Err = ();
    fn from_str(s: &str) -> Result<Crate, Self::Err> {
        let re = Regex::new(r"(\S*)-(\d*)\.(\d*)\.(\d*)(-(\S*))?").unwrap();
        let cap = re.captures(s).unwrap();
        Ok(Crate {
            name: cap.get(1).unwrap().as_str().to_string(),
            major: cap.get(2).unwrap().as_str().parse().unwrap(),
            minor: cap.get(3).unwrap().as_str().parse().unwrap(),
            patch: cap.get(4).unwrap().as_str().parse().unwrap(),
            subpatch: cap.get(6).map(|x| x.as_str().to_string()).unwrap_or(String::new()),
            found_in_lock: true,
        })
    }
}

impl std::fmt::Display for Crate {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        write!(fmt, "{}-{}.{}.{}", self.name, self.major, self.minor, self.patch)?;
        if !self.subpatch.is_empty() {
            write!(fmt, "-{}", self.subpatch)?
        }
        Ok(())
    }
}

pub fn nix_name(name: &str) -> String {
    name.chars().map(|c| match c {
        '-' => '_',
        '.' => '_',
        c => c
    }).collect()
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFetch {
    pub url: String,
    pub rev: String,
    pub date: String,
    pub sha256: String,
    #[serde(rename = "fetchSubmodules")]
    pub fetch_submodules: bool
}

fn make_deps(base_path: &Path, deps: &toml::Value) -> BTreeMap<String, Dep> {
    let mut dependencies = BTreeMap::new();
    for (name, dep) in deps.as_table().unwrap() {
        debug!("name={:?}, dep={:?}", name, dep);
        if let Some(dep) = dep.as_table() {
            let name = name.to_string();
            let enabled_features = if let Some(features) = dep.get("features") {
                features
                    .as_array().unwrap()
                    .into_iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect()
            } else {
                Vec::new()
            };
            let path = if let (Some(path), None) = (dep.get("path"), dep.get("version")) {
                Some(path.as_str().unwrap().to_string())
            } else {
                None
            };
            let mut dep = Dep {
                cr: Crate::default(),
                from_crates_io: false,
                path: if let Some(path) = path {
                    Some(base_path.join(path))
                } else {
                    None
                },
                is_optional: dep.get("optional").map(|x| x.as_bool().unwrap()).unwrap_or(false),
                features: enabled_features,
                default_features: dep.get("default-features").map(|x| x.as_bool().unwrap()).unwrap_or(true),
                conditional_features: Vec::new(),
            };
            dep.cr.name.push_str(&name);
            dependencies.insert(name, dep);
        } else if dep.as_str().is_some() {
            let name = name.to_string();
            let mut dep = Dep {
                cr: Crate::default(),
                from_crates_io: false,
                path: None,
                features: Vec::new(),
                is_optional: false,
                default_features: true,
                conditional_features: Vec::new(),
            };
            dep.cr.name.push_str(&name);
            dependencies.insert(name, dep);
        }
    }
    dependencies
}

pub fn make_dependencies(base_path: &Path, deps: Option<&toml::Value>, features: Option<&toml::Value>) -> (BTreeMap<String, Dep>, Vec<ConditionalFeature>) {
    let mut dependencies = BTreeMap::new();
    let mut cond = Vec::new();
    if let Some(deps) = deps {
        dependencies = make_deps(base_path, deps)
    }

    if let Some(features) = features {
        for (a, b) in features.as_table().unwrap().iter() {
            // println!("a {:?}, b {:?}", a, b);
            for b in b.as_array().unwrap() {
                if let Some(b) = b.as_str() {
                    let mut b = b.split('/');
                    match (b.next(), b.next()) {
                        (Some(c), Some(d)) => {
                            if let Some(dep) = dependencies.get_mut(c) {
                                debug!("conditional: {:?} {:?} {:?}", c, a, d);
                                dep.conditional_features.push(ConditionalFeature {
                                    feature: a.to_string(),
                                    dep_feature: d.to_string()
                                })
                            }
                        }
                        (Some(c), None) => {
                            // println!("conditional: {:?} {:?} {:?}", c, a, d);
                            cond.push(ConditionalFeature {
                                feature: a.to_string(),
                                dep_feature: c.to_string()
                            })
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    (dependencies, cond)
}


pub fn find_cargo_lock() -> Result<PathBuf, Error> {
    let mut current = std::env::current_dir()?;
    loop {
        current.push("Cargo.lock");
        debug!("current = {:?}", current);
        if std::fs::metadata(&current).is_ok() {
            return Ok(current)
        }
        current.pop();
        if !current.pop() {
            return Err(ErrorKind::NoCargoLock.into())
        }
    }
}
