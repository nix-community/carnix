use std::path::Path;
use krate::Src;
use std::path::PathBuf;
use Error;
use std;
use std::collections::BTreeMap;
use std::io::{Read, Write};

#[derive(Debug, Clone)]
pub struct Prefetch {
    pub prefetch: Src,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct Cache {
    path: PathBuf,
    cache: String,
    lines: Vec<(usize, usize)>,
    new: BTreeMap<String, Prefetch>,
}

impl Drop for Cache {
    fn drop(&mut self) {
        std::fs::create_dir_all(self.path.parent().unwrap()).unwrap();
        let mut path2 = self.path.clone();
        path2.set_extension("tmp");
        let mut f = std::fs::File::create(&path2).unwrap();
        let mut new_it = self.new.iter().peekable();
        let mut ex_it = self.lines.iter().peekable();
        loop {
            let advance_new = if let Some((a, b)) = new_it.peek() {
                if let Some((c, d)) = ex_it.peek() {
                    let (u, v, w) = self.get_index(*c, *d).unwrap();
                    if u < a {
                        ex_it.next();
                        writeln!(f, "{} {} {}", u, v, w).unwrap();
                        false
                    } else {
                        if let Src::Crate { ref sha256 } = b.prefetch {
                            writeln!(f, "{} {} {}", a, sha256, b.path.to_string_lossy()).unwrap();
                        }
                        true
                    }

                } else {
                    if let Src::Crate { ref sha256 } = b.prefetch {
                        writeln!(f, "{} {} {}", a, sha256, b.path.to_string_lossy()).unwrap();
                    }
                    true
                }
            } else {
                for &(l0, l1) in ex_it {
                    let (u, v, w) = self.get_index(l0, l1).unwrap();
                    writeln!(f, "{} {} {}", u, v, w).unwrap();
                }
                break
            };
            if advance_new {
                new_it.next();
            } else {
                ex_it.next();
            }
        }
        std::fs::rename(&path2, &self.path).unwrap();
    }
}

impl Cache {
    pub fn new<P:AsRef<Path>>(path: P) -> Result<Self, Error> {
        let path = path.as_ref();
        let mut cache = String::new();
        if let Ok(mut f) = std::fs::File::open(path) {
            f.read_to_string(&mut cache)?;
        }
        let mut lines = Vec::new();
        for l in cache.lines() {
            if lines.is_empty() {
                lines.reserve(2 * cache.len() / l.len());
            }
            let l0 = l.as_ptr() as usize - cache.as_ptr() as usize;
            lines.push((l0, l0 + l.len()))
        }
        Ok(Cache {
            path: path.to_path_buf(),
            cache,
            lines,
            new: BTreeMap::new(),
        })
    }

    fn get_index(&self, l0: usize, l1: usize) -> Option<(&str, &str, &str)> {
        let a = self.cache.get(l0..l1).unwrap();
        let mut it = a.split(' ');
        if let (Some(u), Some(v), Some(w)) = (it.next(), it.next(), it.next()) {
            Some((u, v, w))
        } else {
            None
        }
    }

    pub fn get(&mut self, url: &str) -> Option<Prefetch> {
        debug!("cache get {:?}", url);
        if let Some(pre) = self.new.get(url) {
            Some(pre.clone())
        } else {
            let n = match self.lines.binary_search_by(|&(a, b)| {
                let url0 = self.cache.get(a..b).unwrap();
                debug!("{:?} {:?}", url, url0);
                if url0.starts_with(url) {
                    std::cmp::Ordering::Equal
                } else {
                    url0.cmp(url)
                }
            }) {
                Ok(n) => n,
                _ => return None
            };
            debug!("n = {:?}", n);
            let (l0, l1) = self.lines[n];
            self.get_index(l0, l1).map(|(_, b, c)| {
                Prefetch {
                    prefetch: Src::Crate { sha256: b.to_string() },
                    path: Path::new(c).to_path_buf(),
                }
            })
        }
    }

    pub fn insert(&mut self, url: &str, prefetch: Prefetch) {
        self.new.insert(url.to_string(), prefetch);
    }
}
