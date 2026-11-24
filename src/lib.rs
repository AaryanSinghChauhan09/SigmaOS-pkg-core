//! SigmaOS pkg-core — package management core.
//!
//! Provides a local package database, dependency resolution, and transaction
//! planning for the SigmaOS package manager. Backed by JSON on disk.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
            && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
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
                if pkg.depends.iter().any(|d| d == name) {
                    return Err(PkgError::Conflict(format!(
                        "{n} {v} depends on {name}"
                    )));
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
            // Skip already-installed deps.
            if self.installed.contains_key(dep) {
                continue;
            }
            self.visit(dep, visited, stack, order)?;
        }
        stack.remove(name);
        visited.insert(name.to_string());
        order.push(name.to_string());
        Ok(())
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

/// BFS topological ordering helper used by installers.
pub fn topo_order(db: &PackageDb, roots: &[String]) -> Result<Vec<String>, PkgError> {
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue: VecDeque<String> = roots.iter().cloned().collect();
    while let Some(n) = queue.pop_front() {
        if !seen.insert(n.clone()) {
            continue;
        }
        let deps = db.resolve(&n)?;
        for d in deps {
            if !seen.contains(&d) {
                queue.push_back(d);
            }
        }
        order.push(n);
    }
    Ok(order)
}
