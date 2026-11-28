//! CLI for sigmaos-pkg-core.

use clap::{Parser, Subcommand};
use sigmaos_pkg_core::{default_db_path, Package, PackageDb};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sigmaos-pkg-core", about = "SigmaOS package management core")]
struct Cli {
    /// Path to package database JSON (default: user data dir).
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List installed packages.
    List,
    /// Search the package database.
    Search { query: String },
    /// Show dependency resolution order for a package.
    Deps { name: String },
    /// Add a package record (JSON on stdin is not supported; use file).
    Add { file: PathBuf },
    /// Mark a package installed.
    Install { name: String, version: String },
    /// Remove an installed package.
    Remove { name: String },
    /// List installed packages nothing depends on (orphan query).
    Orphans,
    /// List packages providing a name (apt-cache whatprovides).
    WhatProvides { name: String },
    /// Show database statistics.
    Stats,
    /// Verify installed dependency constraints (apt-get check analogue).
    Check,
    /// List installed packages that depend on a package.
    Rdepends { name: String },
    /// Print database path.
    Path,
}

fn load_db(db: &Option<PathBuf>) -> Result<PackageDb, Box<dyn std::error::Error>> {
    match db.clone().or_else(default_db_path) {
        Some(p) if p.exists() => Ok(PackageDb::load(&p)?),
        Some(p) => {
            let d = PackageDb::new();
            d.save(&p)?;
            Ok(d)
        }
        None => Err("cannot determine db path".into()),
    }
}

fn save_db(db: &PackageDb, path: &Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let p = path.clone().or_else(default_db_path).ok_or("no db path")?;
    db.save(&p)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let cli = Cli::parse();
    let mut db = load_db(&cli.db)?;
    match cli.command {
        Commands::List => {
            for (n, v) in db.list_installed() {
                println!("{n} {v}");
            }
        }
        Commands::Search { query } => {
            for p in db.search(&query) {
                println!("{} {} — {}", p.name, p.version, p.description);
            }
        }
        Commands::Deps { name } => {
            for d in db.resolve(&name)? {
                println!("{d}");
            }
        }
        Commands::Add { file } => {
            let text = std::fs::read_to_string(file)?;
            let pkg: Package = serde_json::from_str(&text)?;
            db.add(pkg)?;
            save_db(&db, &cli.db)?;
        }
        Commands::Install { name, version } => {
            db.install(&name, &version)?;
            save_db(&db, &cli.db)?;
            println!("installed {name} {version}");
        }
        Commands::Remove { name } => {
            db.remove(&name)?;
            save_db(&db, &cli.db)?;
            println!("removed {name}");
        }
        Commands::WhatProvides { name } => {
            for n in db.what_provides(&name) {
                println!("{n}");
            }
        }
        Commands::Stats => {
            let (inst, total) = db.stats();
            println!("installed: {}  records: {}", inst, total);
        }
        Commands::Orphans => {
            for n in db.orphans() {
                println!("{n}");
            }
        }
        Commands::Check => {
            let v = db.check();
            if v.is_empty() {
                println!("ok: all dependencies satisfied");
            } else {
                for line in &v {
                    println!("{line}");
                }
                std::process::exit(1);
            }
        }
        Commands::Rdepends { name } => {
            for n in db.rdepends(&name) {
                println!("{n}");
            }
        }
        Commands::Path => {
            println!(
                "{}",
                cli.db
                    .clone()
                    .or_else(default_db_path)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}
