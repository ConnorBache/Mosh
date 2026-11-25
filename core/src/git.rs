use crate::{Mosh, path_text, prefix};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) fn root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_owned)
}

fn command(repo: &Path, filters: &[String], args: &[&str]) -> Result<Vec<u8>> {
    const LIMIT: u64 = 32 * 1024 * 1024;
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args([
            "--no-pager",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotePath=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    for filter in filters {
        command.arg("-c").arg(filter);
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Git is required to read repository history")?;
    let mut data = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take(LIMIT + 1)
        .read_to_end(&mut data);
    if read.is_err() || data.len() as u64 > LIMIT {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read?;
    ensure!((data.len() as u64) <= LIMIT, "Git output exceeded 32 MiB");
    ensure!(status.success(), "Git read failed ({status})");
    Ok(data)
}

fn read(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    // Working-tree status may otherwise execute custom clean filters from repository config.
    let mut filters = BTreeMap::new();
    let output = command(
        repo,
        &[],
        &[
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(clean|smudge|process|required)$",
        ],
    )
    .unwrap_or_default();
    for name in output
        .split(|c| *c == 0)
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
    {
        if let Some((name, _)) = name.rsplit_once('.') {
            filters.insert(name.to_owned(), ());
        }
    }
    let overrides = filters
        .keys()
        .flat_map(|name| {
            [
                format!("{name}.clean="),
                format!("{name}.smudge="),
                format!("{name}.process="),
                format!("{name}.required=false"),
            ]
        })
        .collect::<Vec<_>>();
    command(repo, &overrides, args)
}

pub(crate) fn refresh(db: &Connection, repo: &Path) -> Result<()> {
    let location = path_text(repo)?;
    let (low, high) = prefix(location);
    let branch = read(repo, &["branch", "--show-current"])
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(location);
    db.execute("INSERT INTO repositories VALUES(?,?,?) ON CONFLICT(path) DO UPDATE SET branch=excluded.branch",
        params![location,name,branch])?;
    db.execute(
        "UPDATE objects SET git_root=?1,git_status=NULL,git_author=NULL,git_committed=NULL
        WHERE missing=0 AND (path=?1 OR (path>=?2 AND path<?3))
        AND (git_root IS NULL OR length(git_root)<=length(?1))",
        params![location, low, high],
    )?;
    let tracked = read(repo, &["ls-files", "-z", "--cached"])?;
    let mut set = db.prepare_cached(
        "UPDATE objects SET git_status=?1 WHERE path=?2 AND path IS NOT NULL AND missing=0
            AND mosh_path_key(path)=mosh_path_key(?2) AND git_root=?3",
    )?;
    for item in tracked.split(|c| *c == 0).filter(|item| !item.is_empty()) {
        if let Ok(path) = std::str::from_utf8(item) {
            set.execute(params![
                "tracked",
                path_text(&repo.join(path.replace('/', std::path::MAIN_SEPARATOR_STR)))?,
                location
            ])?;
        }
    }
    let changed = read(
        repo,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored=matching",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    for item in changed.split(|c| *c == 0).filter(|item| item.len() >= 4) {
        let Ok(path) = std::str::from_utf8(&item[3..]) else {
            continue;
        };
        let status = match &item[..2] {
            b"??" => "untracked",
            b"!!" => "ignored",
            state if state.contains(&b'U') => "conflicted",
            state if state.contains(&b'D') => "deleted",
            state if state.contains(&b'A') => "added",
            _ => "modified",
        };
        let directory = path.ends_with('/');
        let path = repo.join(
            path.trim_end_matches('/')
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        );
        let key = path_text(&path)?;
        set.execute(params![status, key, location])?;
        if directory {
            let (low, high) = prefix(key);
            db.execute("UPDATE objects SET git_status=?1 WHERE path>=?2 AND path<?3 AND git_root=?4 AND git_status IS NULL AND missing=0",
                params![status,low,high,location])?;
        }
    }
    let data = read(
        repo,
        &[
            "log",
            "-250",
            "--first-parent",
            "--name-only",
            "--no-renames",
            "-z",
            "--format=%x1e%an%x1f%ct",
        ],
    )
    .unwrap_or_default();
    let mut update=db.prepare_cached("UPDATE objects SET git_author=?1,git_committed=?2 WHERE path=?3 AND path IS NOT NULL
        AND mosh_path_key(path)=mosh_path_key(?3) AND git_root=?4 AND git_author IS NULL AND missing=0")?;
    for record in data.split(|c| *c == 0x1e) {
        let Ok(record) = std::str::from_utf8(record) else {
            continue;
        };
        let record = record.trim_start_matches(['\n', '\r', '\0']);
        let Some((header, files)) = record.split_once('\0') else {
            continue;
        };
        let Some((author, timestamp)) = header.split_once('\x1f') else {
            continue;
        };
        let Ok(timestamp) = timestamp.trim().parse::<i64>() else {
            continue;
        };
        for path in files
            .trim_start_matches(['\n', '\r', '\0'])
            .split('\0')
            .filter(|s| !s.is_empty())
        {
            let key = repo.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
            update.execute(params![
                author,
                timestamp.saturating_mul(1000),
                path_text(&key)?,
                location
            ])?;
        }
    }
    Ok(())
}

impl Mosh {
    pub fn git_history(&self, reference: &str) -> Result<Vec<Value>> {
        let selected = self.get(reference)?;
        self.require_contents(&selected)?;
        let root = Path::new(
            selected
                .git_root
                .as_deref()
                .context("object is outside a Git repository")?,
        );
        let path = Path::new(
            selected
                .path
                .as_deref()
                .context("object has no physical path")?,
        )
        .strip_prefix(root)?;
        let relative = path_text(path)?.replace(std::path::MAIN_SEPARATOR, "/");
        let mut args = vec![
            "--literal-pathspecs",
            "log",
            "-100",
            "--format=%H%x1f%an%x1f%cn%x1f%aI%x1f%cI%x1f%s",
            "--no-color",
        ];
        if selected.kind == "file" {
            args.push("--follow");
        }
        if !relative.is_empty() {
            args.extend(["--", &relative]);
        }
        let data = read(root, &args)?;
        Ok(String::from_utf8_lossy(&data).lines().filter_map(|line| {
            let f=line.splitn(6,'\x1f').collect::<Vec<_>>();
            (f.len()==6).then(||json!({"commit":f[0],"author":f[1],"committer":f[2],"authored":f[3],"committed":f[4],"subject":f[5]}))
        }).collect())
    }

    pub fn git_diff(&self, reference: &str, revision: Option<&str>) -> Result<String> {
        let selected = self.get(reference)?;
        self.require_contents(&selected)?;
        let root = Path::new(
            selected
                .git_root
                .as_deref()
                .context("object is outside a Git repository")?,
        );
        let path = Path::new(selected.path.as_deref().context("object has no path")?)
            .strip_prefix(root)?;
        let relative = path_text(path)?.replace(std::path::MAIN_SEPARATOR, "/");
        ensure!(
            selected.kind == "file",
            "choose a file to view its Git diff"
        );
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
        ];
        if let Some(revision) = revision {
            ensure!(
                (7..=64).contains(&revision.len())
                    && revision.bytes().all(|b| b.is_ascii_hexdigit()),
                "use a Git commit hash"
            );
            args = vec![
                "--literal-pathspecs",
                "show",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                revision,
            ];
        }
        args.extend(["--", &relative]);
        Ok(String::from_utf8_lossy(&read(root, &args)?).into_owned())
    }
}
