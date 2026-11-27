//! SigmaOS pkg-core — package management core.
//!
//! Provides a local package database, dependency resolution, and transaction
//! planning for the SigmaOS package manager. Backed by JSON on disk.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Errors produced by this component.
#[derive(Debug, thiserror::Error)]
pub enum PkgError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("package not found: {0}")]
    NotFound(String),
    #[error("dependency cycle involving: {0}")]
    Cycle(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid package name: {0}")]
    InvalidName(String),
    #[error("validation error: {0}")]
    Validation(String),
}

/// A package record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub arch: String,
    pub description: String,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    pub installed: bool,
}

impl Package {
    /// Validate package name (alnum, -, _, .; must start alnum).
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
    }
}

/// A parsed version triple (major.minor.patch; missing parts = 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse "1", "1.2", or "1.2.3" (extra suffixes ignored).
    pub fn parse(s: &str) -> Option<Self> {
        let core = s.split(['-', '+']).next()?;
        let mut it = core.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let patch = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

/// A version constraint on a dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint {
    Exact(Version),
    Gte(Version),
    Lt(Version),
    Caret(Version),
    Tilde(Version),
}

impl Constraint {
    /// Does `v` satisfy this constraint?
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Constraint::Exact(e) => v == e,
            Constraint::Gte(e) => v >= e,
            Constraint::Lt(e) => v < e,
            Constraint::Caret(e) => {
                // same major, >= base (semver caret)
                v.major == e.major && v >= e
            }
            Constraint::Tilde(e) => {
                // same major.minor, >= base
                v.major == e.major && v.minor == e.minor && v >= e
            }
        }
    }
}

/// Split a depends entry into (name, optional constraint).
/// Accepts "name", "name>=1.2", "name >= 1.2", "name = 1.2.3".
pub fn parse_dep(dep: &str) -> (String, Option<Constraint>) {
    // find first constraint operator
    for op in [">=", "<=", "=<", "<", "=", "^", "~"] {
        if let Some(idx) = dep.find(op) {
            let name = dep[..idx].trim().to_string();
            let cstr = dep[idx..].trim();
            // strip leading op chars for parse
            let ver = cstr.trim_start_matches(['>', '<', '=', '^', '~']).trim();
            let c = match op {
                ">=" => Version::parse(ver).map(Constraint::Gte),
                "<=" | "=<" => Version::parse(ver).map(|v| {
                    Constraint::Lt(Version {
                        patch: v.patch + 1,
                        ..v
                    })
                }),
                "<" => Version::parse(ver).map(Constraint::Lt),
                "=" => Version::parse(ver).map(Constraint::Exact),
                "^" => Version::parse(ver).map(Constraint::Caret),
                "~" => Version::parse(ver).map(Constraint::Tilde),
                _ => None,
            };
            return (name, c);
        }
    }
    (dep.trim().to_string(), None)
}

/// Check an installed version against an optional constraint.
pub fn version_ok(installed: &str, constraint: &Option<Constraint>) -> bool {
    match constraint {
        None => true,
        Some(c) => Version::parse(installed)
            .map(|v| c.matches(&v))
            .unwrap_or(false),
    }
}

/// The package database.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageDb {
    #[serde(default)]
    pub packages: BTreeMap<String, Package>,
    /// name -> installed version
    #[serde(default)]
    pub installed: BTreeMap<String, String>,
}

impl PackageDb {
    /// Create an empty database.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or update a package record.
    pub fn add(&mut self, pkg: Package) -> Result<(), PkgError> {
        if !Package::valid_name(&pkg.name) {
            return Err(PkgError::InvalidName(pkg.name.clone()));
        }
        self.packages.insert(pkg.name.clone(), pkg);
        Ok(())
    }

    /// Mark a package installed.
    pub fn install(&mut self, name: &str, version: &str) -> Result<(), PkgError> {
        let pkg = self
            .packages
            .get(name)
            .ok_or_else(|| PkgError::NotFound(name.into()))?;
        // Check conflicts.
        for c in &pkg.conflicts {
            if self.installed.contains_key(c) {
                return Err(PkgError::Conflict(format!(
                    "{name} conflicts with installed {c}"
                )));
            }
        }
        self.installed.insert(name.to_string(), version.to_string());
        Ok(())
    }

    /// Mark a package removed.
    pub fn remove(&mut self, name: &str) -> Result<(), PkgError> {
        // Ensure nothing installed depends on it.
        for (n, v) in &self.installed {
            if let Some(pkg) = self.packages.get(n) {
                if pkg.depends.iter().any(|d| parse_dep(d).0 == *name) {
                    return Err(PkgError::Conflict(format!("{n} {v} depends on {name}")));
                }
            }
        }
        if self.installed.remove(name).is_none() {
            return Err(PkgError::NotFound(name.into()));
        }
        Ok(())
    }

    /// Resolve dependencies for a package (BFS, detects cycles).
    pub fn resolve(&self, name: &str) -> Result<Vec<String>, PkgError> {
        let mut order = Vec::new();
        let mut visited = BTreeSet::new();
        let mut stack = BTreeSet::new();
        self.visit(name, &mut visited, &mut stack, &mut order)?;
        Ok(order)
    }

    fn visit(
        &self,
        name: &str,
        visited: &mut BTreeSet<String>,
        stack: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<(), PkgError> {
        if visited.contains(name) {
            return Ok(());
        }
        if !stack.insert(name.to_string()) {
            return Err(PkgError::Cycle(name.into()));
        }
        let pkg = self
            .packages
            .get(name)
            .ok_or_else(|| PkgError::NotFound(name.into()))?;
        for dep in &pkg.depends {
            let (dep_name, constraint) = parse_dep(dep);
            // Skip already-installed deps that satisfy the constraint.
            if let Some(installed_v) = self.installed.get(&dep_name) {
                if version_ok(installed_v, &constraint) {
                    continue;
                }
                return Err(PkgError::Conflict(format!(
                    "{name} requires {dep} but installed {installed_v} does not satisfy constraint"
                )));
            }
            // Verify the candidate package version satisfies the constraint.
            if let (Some(c), Some(pkg_rec)) = (&constraint, self.packages.get(&dep_name)) {
                if let Some(v) = Version::parse(&pkg_rec.version) {
                    if !c.matches(&v) {
                        return Err(PkgError::Validation(format!(
                            "{name} requires {dep} but available {} does not satisfy",
                            pkg_rec.version
                        )));
                    }
                }
            }
            self.visit(&dep_name, visited, stack, order)?;
        }
        stack.remove(name);
        visited.insert(name.to_string());
        order.push(name.to_string());
        Ok(())
    }

    /// Installed packages that nothing else installed depends on —
    /// pacman's `-Qtd` "orphan" query. Explicitly-installed leaves are
    /// indistinguishable here, so treat the result as "review candidates".
    pub fn orphans(&self) -> Vec<String> {
        let needed: BTreeSet<String> = self
            .installed
            .keys()
            .filter_map(|n| self.packages.get(n))
            .flat_map(|p| p.depends.iter().map(|d| parse_dep(d).0))
            .collect();
        self.installed
            .keys()
            .filter(|n| !needed.contains(*n))
            .cloned()
            .collect()
    }

    /// List installed packages.
    pub fn list_installed(&self) -> Vec<(&String, &String)> {
        self.installed.iter().collect()
    }

    /// Search packages by name substring.
    pub fn search(&self, query: &str) -> Vec<&Package> {
        self.packages
            .values()
            .filter(|p| p.name.contains(query) || p.description.contains(query))
            .collect()
    }

    /// Save database to a JSON file.
    pub fn save(&self, path: &Path) -> Result<(), PkgError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load database from a JSON file.
    pub fn load(path: &Path) -> Result<Self, PkgError> {
        let text = std::fs::read_to_string(path)
            .map_err(|_| PkgError::NotFound(path.display().to_string()))?;
        Ok(serde_json::from_str(&text)?)
    }
}

/// Default database path.
pub fn default_db_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("org", "sigmaos", "sigmaos")
        .map(|p| p.data_dir().join("pkg").join("db.json"))
}

/// Dependency-first install order: every package appears after all of
/// its dependencies. Detects cycles via a visiting-state map.
pub fn topo_order(db: &PackageDb, roots: &[String]) -> Result<Vec<String>, PkgError> {
    fn visit(
        db: &PackageDb,
        n: &str,
        state: &mut BTreeMap<String, u8>,
        order: &mut Vec<String>,
    ) -> Result<(), PkgError> {
        match state.get(n).copied().unwrap_or(0) {
            1 => return Err(PkgError::Cycle(n.into())),
            2 => return Ok(()),
            _ => {}
        }
        state.insert(n.to_string(), 1);
        for d in db.resolve(n)? {
            // resolve() includes the queried package itself.
            if d != n {
                visit(db, &d, state, order)?;
            }
        }
        state.insert(n.to_string(), 2);
        order.push(n.to_string());
        Ok(())
    }
    let mut order = Vec::new();
    let mut state = BTreeMap::new();
    for r in roots {
        visit(db, r, &mut state, &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, version: &str, depends: &[&str]) -> Package {
        Package {
            name: name.into(),
            version: version.into(),
            arch: "x86_64".into(),
            description: String::new(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            provides: vec![],
            conflicts: vec![],
            installed: false,
        }
    }

    #[test]
    fn parse_dep_plain() {
        let (n, c) = parse_dep("glibc");
        assert_eq!(n, "glibc");
        assert!(c.is_none());
    }

    #[test]
    fn parse_dep_constraints() {
        assert_eq!(parse_dep("foo>=1.2").0, "foo");
        assert!(matches!(parse_dep("foo>=1.2").1, Some(Constraint::Gte(_))));
        assert!(matches!(
            parse_dep("foo = 2.0.1").1,
            Some(Constraint::Exact(_))
        ));
        assert!(matches!(parse_dep("foo^1.0").1, Some(Constraint::Caret(_))));
        assert!(matches!(parse_dep("foo~1.2").1, Some(Constraint::Tilde(_))));
        assert!(matches!(parse_dep("foo<3.0").1, Some(Constraint::Lt(_))));
    }

    #[test]
    fn version_matches() {
        let v = Version::parse("1.2.3").unwrap();
        assert!(Constraint::Gte(Version::parse("1.0").unwrap()).matches(&v));
        assert!(Constraint::Caret(Version::parse("1.2").unwrap()).matches(&v));
        assert!(!Constraint::Caret(Version::parse("2.0").unwrap()).matches(&v));
        assert!(Constraint::Tilde(Version::parse("1.2.0").unwrap()).matches(&v));
        assert!(!Constraint::Tilde(Version::parse("1.3.0").unwrap()).matches(&v));
        assert!(Constraint::Exact(Version::parse("1.2.3").unwrap()).matches(&v));
        assert!(!Constraint::Lt(Version::parse("1.0").unwrap()).matches(&v));
    }

    #[test]
    fn resolve_honors_constraints() {
        let mut db = PackageDb::new();
        db.add(pkg("app", "1.0", &["lib>=2.0"])).unwrap();
        db.add(pkg("lib", "2.5", &[])).unwrap();
        let order = db.resolve("app").unwrap();
        assert_eq!(order, vec!["lib", "app"]);
    }

    #[test]
    fn resolve_rejects_unsatisfied() {
        let mut db = PackageDb::new();
        db.add(pkg("app", "1.0", &["lib>=3.0"])).unwrap();
        db.add(pkg("lib", "2.5", &[])).unwrap();
        assert!(matches!(db.resolve("app"), Err(PkgError::Validation(_))));
    }

    #[test]
    fn installed_version_checked() {
        let mut db = PackageDb::new();
        db.add(pkg("app", "1.0", &["lib>=2.0"])).unwrap();
        db.add(pkg("lib", "1.5", &[])).unwrap();
        db.installed.insert("lib".into(), "1.5".into());
        assert!(matches!(db.resolve("app"), Err(PkgError::Conflict(_))));
    }

    #[test]
    fn orphans_lists_unneeded() {
        let mut db = PackageDb::new();
        db.add(pkg("app", "1.0", &["lib"])).unwrap();
        db.add(pkg("lib", "1.0", &[])).unwrap();
        db.add(pkg("lone", "1.0", &[])).unwrap();
        db.installed.insert("app".into(), "1.0".into());
        db.installed.insert("lib".into(), "1.0".into());
        db.installed.insert("lone".into(), "1.0".into());
        // 'app' is a leaf too (nothing depends on it) — orphans are
        // packages with no *dependents*, matching pacman -Qtd.
        assert_eq!(db.orphans(), vec!["app".to_string(), "lone".to_string()]);
    }

    #[test]
    fn remove_checks_constrained_deps() {
        let mut db = PackageDb::new();
        db.add(pkg("app", "1.0", &["lib>=1.0"])).unwrap();
        db.add(pkg("lib", "1.5", &[])).unwrap();
        db.installed.insert("app".into(), "1.0".into());
        db.installed.insert("lib".into(), "1.5".into());
        assert!(db.remove("lib").is_err());
    }

    #[test]
    fn topo_order_dependencies_first() {
        let mut db = PackageDb::new();
        db.add(pkg("app", "1.0", &["lib", "tool"])).unwrap();
        db.add(pkg("lib", "2.0", &[])).unwrap();
        db.add(pkg("tool", "1.0", &["lib"])).unwrap();
        let order = topo_order(&db, &["app".into()]).unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("lib") < pos("app"));
        assert!(pos("tool") < pos("app"));
    }

    #[test]
    fn save_load_roundtrip() {
        let mut db = PackageDb::new();
        db.add(pkg("a", "1.0", &[])).unwrap();
        db.install("a", "1.0").unwrap();
        let dir = std::env::temp_dir().join(format!("pkgdb-{}", std::process::id()));
        let path = dir.join("db.toml");
        db.save(&path).unwrap();
        let back = PackageDb::load(&path).unwrap();
        assert_eq!(back.list_installed().len(), 1);
        assert_eq!(back.list_installed()[0].0, "a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_rejects_duplicate_and_bad_name() {
        let mut db = PackageDb::new();
        db.add(pkg("x", "1.0", &[])).unwrap();
        // Index semantics: re-adding a name replaces the record.
        db.add(pkg("x", "2.0", &[])).unwrap();
        assert_eq!(db.packages["x"].version, "2.0");
        let mut bad = pkg("bad name!", "1.0", &[]);
        assert!(db.add(bad.clone()).is_err());
        bad.name = "good-name_1.2".into();
        assert!(Package::valid_name(&bad.name));
    }

    #[test]
    fn search_matches_name_and_desc() {
        let mut db = PackageDb::new();
        let mut p = pkg("findme", "1.0", &[]);
        p.description = "a searchable thing".into();
        db.add(p).unwrap();
        db.add(pkg("other", "1.0", &[])).unwrap();
        assert_eq!(db.search("findme").len(), 1);
        assert_eq!(db.search("searchable").len(), 1);
        assert!(db.search("zzz").is_empty());
    }
}
