use anyhow::Result;
use aqueue::Actor;
use clap::Parser;
use std::fmt::{Display, Formatter};
use std::io::{stdin, stdout, Write};
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::sync::OnceCell;

#[derive(Parser)]
#[clap(version, bin_name = "cargo cleans",about = "Clean up all targets of the current path")]
struct Arg {
    /// The directory that will be cleaned
    #[clap(short, long, default_value = ".", value_name = "DIR")]
    root_dir: String,
    /// Don't ask for confirmation
    #[clap(short = 'y', long = "yes")]
    yes: bool,
    /// Don't clean projects with target dirs modified in the last [DAYS] days
    #[clap(
        short = 'd',
        long = "keep-days",
        value_name = "DAYS",
        default_value_t = 0
    )]
    keep_days: u32,
    /// Don't clean projects with target dir sizes below the specified size
    /// Unit: MB
    #[clap(
        short = 's',
        long = "keep-size",
        value_name = "SIZE:MB",
        default_value_t = 0
    )]
    keep_size: u64,
    /// Just collect the cleanable project dirs but don't attempt to clean anything
    #[clap(long = "dry-run")]
    dry_run: bool,
}

static TARGET_PATH_STORE: OnceCell<Actor<PathInfoStore>> = OnceCell::const_new();

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args();
    // When called using `cargo cleans`, the argument `cleans` is inserted. To fix the arg
    // alignment, one argument is dropped.
    if let Some("cleans") = std::env::args().nth(1).as_deref() {
        args.next();
    }

    let args: Arg = Arg::parse_from(args);
    let scan_root = PathBuf::from(args.root_dir);
    anyhow::ensure!(
        scan_root.exists(),
        "not found root path {}",
        scan_root.to_string_lossy()
    );
    let store = TARGET_PATH_STORE
        .get_or_init(|| async move { PathInfoStore::new(args.keep_days, args.keep_size) })
        .await;
    tokio::spawn(iter_path(scan_root)).await??;
    store.display().await?;
    if args.dry_run {
        println!("Dry run. Not doing any cleanup");
        return Ok(());
    }
    // Confirm cleanup if --yes is not present in the args
    if !args.yes {
        let mut inp = String::new();
        print!("Clean the project directories shown above? (yes/no): ");
        stdout().flush()?;
        stdin().read_line(&mut inp)?;
        let inp = inp.trim().to_lowercase();
        if inp != "yes" {
            println!("Cleanup cancelled");
            return Ok(());
        }
    }
    println!("Starting cleanup...");
    store.clean().await?;
    println!("Done!");
    Ok(())
}

/// Iteration path all members
#[async_recursion::async_recursion]
async fn iter_path(path: PathBuf) -> Result<()> {
    if let Some(filename) = path.file_name() {
        match filename.to_string_lossy().as_ref() {
            ".git" => return Ok(()),
            "target" => {
                let cargo_path = {
                    if let Some(parent) = path.parent() {
                        let mut path = parent.to_path_buf();
                        path.push("Cargo.toml");
                        path
                    } else {
                        return Ok(());
                    }
                };

                if cargo_path.exists() {
                    let last_modified = path.metadata()?.modified()?;
                    let size = iter_file_size(path.clone()).await?;
                    return TARGET_PATH_STORE
                        .get()
                        .unwrap()
                        .add_path_info(PathInfo {
                            path,
                            last_modified,
                            size,
                        })
                        .await;
                }
            }
            _ => {}
        }
    }
    let dirs = match path.read_dir() {
        Ok(dir) => dir,
        Err(_) => return Ok(()),
    };
    for join in dirs
        .into_iter()
        .filter_map(|x| x.ok())
        .filter(|x| x.file_type().is_ok() && x.file_type().unwrap().is_dir())
        .map(|x| tokio::spawn(iter_path(x.path())))
        .collect::<Vec<_>>()
    {
        join.await??;
    }
    Ok(())
}

/// analyze path child file size
#[async_recursion::async_recursion]
async fn iter_file_size(path: PathBuf) -> Result<u64> {
    match (path.is_file(), path.metadata()) {
        (true, Ok(md)) => Ok(md.len()),
        _ => {
            let dirs = match path.read_dir() {
                Ok(dir) => dir,
                Err(_) => return Ok(0),
            };
            let mut len = 0u64;
            for join in dirs
                .into_iter()
                .filter_map(|x| x.ok())
                .map(|x| tokio::spawn(iter_file_size(x.path())))
                .collect::<Vec<_>>()
            {
                len += join.await??;
            }
            Ok(len)
        }
    }
}

/// target path info
#[derive(Clone)]
struct PathInfo {
    path: PathBuf,
    last_modified: SystemTime,
    size: u64,
}
impl Display for PathInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(Some(filename)) = self.path.parent().map(|x| x.file_name()) {
            write!(
                f,
                "  {} : {}\n      {}, {}",
                filename.to_str().unwrap_or_default(),
                self.path.display(),
                chrono::DateTime::<chrono::Local>::from(self.last_modified)
                    .format("%Y-%m-%d %H:%M"),
                bytefmt::format(self.size)
            )
        } else {
            write!(
                f,
                " {}      {}    {}",
                self.path.display(),
                chrono::DateTime::<chrono::Local>::from(self.last_modified)
                    .format("%Y-%m-%d %H:%M"),
                bytefmt::format(self.size)
            )
        }
    }
}

/// store all target path info and helper analyze
struct PathInfoStore {
    keep_days: u32,
    keep_size: u64,
    projects: Vec<PathInfo>,
    ignores: Vec<PathInfo>,
}
impl PathInfoStore {
    pub fn new(keep_days: u32, keep_size: u64) -> Actor<Self> {
        Actor::new(Self {
            keep_size: keep_size * 1024 * 1024, //to MB
            keep_days,
            projects: vec![],
            ignores: vec![],
        })
    }
    /// push target path info
    fn push(&mut self, info: PathInfo) {
        let days_elapsed = info
            .last_modified
            .elapsed()
            .map_or(0, |x| (x.as_secs() / (60 * 60 * 24)) as u32);
        if info.size <= self.keep_size || days_elapsed < self.keep_days {
            self.ignores.push(info);
        } else {
            self.projects.push(info);
        }
    }
    /// display all target info
    fn display(&self) {
        if !self.ignores.is_empty() {
            println!("Ignoring the following project directories:");
            for ignore in self.ignores.iter() {
                println!("{}", ignore);
            }
        }
        if !self.projects.is_empty() {
            println!("Selected the following project directories for cleaning:");
            for project in self.projects.iter() {
                println!("{}", project);
            }
        }
        let total_size: u64 = self.projects.iter().map(|it| it.size).sum();
        println!(
            "Selected {}/{} projects, total freeable size: {}",
            self.projects.len(),
            self.projects.len() + self.ignores.len(),
            bytefmt::format(total_size)
        );
    }
    ///clean all target
    fn clean(&self) -> Result<()> {
        for p in self.projects.iter() {
            remove_dir_all::remove_dir_all(&p.path)?;
        }
        Ok(())
    }
}
#[async_trait::async_trait]
trait IPathInfoStore {
    /// push target path info
    async fn add_path_info(&self, info: PathInfo) -> Result<()>;
    /// display all target info
    async fn display(&self) -> Result<()>;
    /// clean all target
    async fn clean(&self) -> Result<()>;
}

#[async_trait::async_trait]
impl IPathInfoStore for Actor<PathInfoStore> {
    async fn add_path_info(&self, info: PathInfo) -> Result<()> {
        self.inner_call(|inner| async move {
            inner.get_mut().push(info);
            Ok(())
        })
        .await
    }
    async fn display(&self) -> Result<()> {
        self.inner_call(|inner| async move {
            inner.get().display();
            Ok(())
        })
        .await
    }
    async fn clean(&self) -> Result<()> {
        self.inner_call(|inner| async move { inner.get().clean() })
            .await
    }
}
