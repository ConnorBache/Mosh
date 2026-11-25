mod service;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use mosh_core::{Action, Mosh, SearchOptions, Sort, default_db};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Parser)]
#[command(
    version,
    about = "Index and organize ordinary files with a local Ledger",
    after_help = "Examples:\n  mosh scan C:\\Work\n  mosh search 'tag:source-code AND modified:<30d'\n  mosh cleave --query 'ext:log' --dry-run"
)]
struct Cli {
    /// Ledger location. Defaults to the current Windows user's local application data.
    #[arg(long, global = true, env = "MOSH_DB")]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Scan {
        path: PathBuf,
    },
    Watch {
        path: PathBuf,
    },
    Roots,
    Search {
        #[arg(default_value = "")]
        query: String,
        #[arg(long, default_value_t = 200)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long,value_enum,default_value_t=SortBy::Name)]
        sort: SortBy,
        #[arg(long)]
        parent: Option<String>,
    },
    Tags {
        reference: String,
    },
    Tag {
        #[command(subcommand)]
        action: TagCommand,
    },
    Paint {
        reference: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, default_value = "standard")]
        level: String,
        #[arg(long)]
        lock: bool,
    },
    Inspect {
        reference: String,
    },
    Preview {
        reference: String,
    },
    Open {
        reference: String,
    },
    Folder {
        parent: String,
        name: String,
    },
    Duplicates {
        path: Option<PathBuf>,
    },
    Space {
        path: PathBuf,
    },
    Cleave {
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long,value_enum,default_value_t=PlanAction::Manifest)]
        action: PlanAction,
        #[arg(long)]
        argument: Option<String>,
        #[arg(long)]
        select: Vec<String>,
        #[arg(long,conflicts_with_all=["undo","status","dry_run"])]
        execute: Option<String>,
        #[arg(long,conflicts_with_all=["status","dry_run"])]
        undo: Option<String>,
        #[arg(long, conflicts_with = "dry_run")]
        status: Option<String>,
        #[arg(long)]
        confirm: bool,
    },
    Coll {
        #[command(subcommand)]
        action: CollectionCommand,
    },
    Marriage {
        #[command(subcommand)]
        action: CollectionCommand,
    },
    Graph {
        reference: Option<String>,
    },
    Save {
        name: String,
        query: String,
    },
    Saved,
    History {
        reference: Option<String>,
        #[arg(long)]
        git: bool,
    },
    Diff {
        reference: String,
        revision: Option<String>,
    },
    Person {
        id: String,
        #[arg(long)]
        superior: Option<String>,
    },
    Access {
        reference: String,
        person: String,
        permission: String,
    },
    Secret {
        reference: String,
        #[arg(long)]
        disable: bool,
    },
    Ledger {
        #[command(subcommand)]
        action: LedgerCommand,
    },
    Doctor,
    Backup {
        destination: PathBuf,
    },
    Restore {
        source: PathBuf,
        destination: PathBuf,
    },
    Serve {
        #[arg(long, default_value_t = 6017)]
        port: u16,
        /// Identity associated with MOSH_API_TOKEN.
        #[arg(long, default_value = "local")]
        actor: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SortBy {
    Name,
    Modified,
    Size,
    Path,
}
#[derive(Clone, Copy, ValueEnum)]
enum PlanAction {
    Copy,
    Move,
    Quarantine,
    Tag,
    Marriage,
    Manifest,
}
#[derive(Subcommand)]
enum CollectionCommand {
    Create {
        name: String,
    },
    Add {
        container: String,
        target: String,
        #[arg(long)]
        selector: Option<String>,
    },
    Query {
        container: String,
        query: String,
    },
}
#[derive(Subcommand)]
enum TagCommand {
    Add {
        reference: String,
        tag: String,
        #[arg(long, default_value_t = 1.0)]
        confidence: f64,
    },
    Remove {
        reference: String,
        tag: String,
    },
    Catalog {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Coverage,
    Alias {
        tag: String,
        alias: String,
    },
    Rename {
        tag: String,
        name: String,
    },
    Merge {
        from: String,
        into: String,
    },
    Delete {
        tag: String,
    },
    Rule {
        query: String,
        tag: String,
    },
}
#[derive(Subcommand)]
enum LedgerCommand {
    Rebuild,
    /// Reclaim unused database pages after upgrading or clearing the content cache.
    Compact,
    /// Report the content cache's page usage and configured budget.
    TextStatus,
    /// Build an optional content index within a byte budget. Zero clears it.
    TextIndex {
        #[arg(long)]
        max_bytes: i64,
    },
}

fn collection(mosh: &Mosh, action: CollectionCommand, marriage: bool) -> Result<Value> {
    Ok(match action {
        CollectionCommand::Create { name } => json!({"id":mosh.collection(&name,marriage)?}),
        CollectionCommand::Add {
            container,
            target,
            selector,
        } => {
            mosh.connect(
                &container,
                &target,
                if marriage { "reference" } else { "parent" },
                selector.as_deref(),
            )?;
            json!({"ok":true})
        }
        CollectionCommand::Query { container, query } => {
            mosh.collection_query(&container, &query)?;
            json!({"ok":true})
        }
    })
}

fn on_interrupt() -> Result<Arc<AtomicBool>> {
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    Ok(cancel)
}

fn run(cli: Cli) -> Result<Option<Value>> {
    let db = cli.db.unwrap_or_else(default_db);
    let command = match cli.command {
        Command::Serve { port, actor } => {
            service::serve(db, port, actor)?;
            return Ok(None);
        }
        Command::Restore {
            source,
            destination,
        } => {
            return Ok(Some(
                json!({"restored":Mosh::restore(&source,&destination)?}),
            ));
        }
        command => command,
    };
    let mosh = Mosh::open(&db)?;
    let ok = json!({"ok":true});
    let output =
        match command {
            Command::Init => json!({"ledger":mosh.db_path()}),
            Command::Scan { path } => {
                let cancel = on_interrupt()?;
                json!(mosh.scan(&path, &cancel)?)
            }
            Command::Watch { path } => {
                let cancel = on_interrupt()?;
                mosh.watch(&path, &cancel, |report| match report {
                    Ok(report) => println!("{}", json!(report)),
                    Err(error) => eprintln!("{error:#}"),
                })?;
                return Ok(None);
            }
            Command::Roots => json!(mosh.roots()?),
            Command::Search {
                query,
                limit,
                offset,
                sort,
                parent,
            } => {
                let sort = match sort {
                    SortBy::Name => Sort::Name,
                    SortBy::Modified => Sort::Modified,
                    SortBy::Size => Sort::Size,
                    SortBy::Path => Sort::Path,
                };
                json!(mosh.search(
                    &query,
                    &SearchOptions {
                        limit,
                        offset,
                        sort,
                        parent
                    }
                )?)
            }
            Command::Tags { reference } => json!(mosh.tags(&reference)?),
            Command::Tag { action } => match action {
                TagCommand::Add {
                    reference,
                    tag,
                    confidence,
                } => {
                    mosh.tag(&reference, &tag, true, confidence)?;
                    ok
                }
                TagCommand::Remove { reference, tag } => {
                    mosh.tag(&reference, &tag, false, 1.0)?;
                    ok
                }
                TagCommand::Catalog { limit } => json!(mosh.tag_catalog(limit)?),
                TagCommand::Coverage => mosh.tag_coverage()?,
                TagCommand::Alias { tag, alias } => {
                    mosh.tag_dictionary("alias", &tag, Some(&alias))?;
                    ok
                }
                TagCommand::Rename { tag, name } => {
                    mosh.tag_dictionary("rename", &tag, Some(&name))?;
                    ok
                }
                TagCommand::Merge { from, into } => {
                    mosh.tag_dictionary("merge", &from, Some(&into))?;
                    ok
                }
                TagCommand::Delete { tag } => {
                    mosh.tag_dictionary("delete", &tag, None)?;
                    ok
                }
                TagCommand::Rule { query, tag } => {
                    mosh.rule(&query, &tag)?;
                    ok
                }
            },
            Command::Paint {
                reference,
                body,
                level,
                lock,
            } => {
                if body.is_some() || level == "none" {
                    mosh.set_paint(&reference, &level, body.as_deref().unwrap_or(""), lock)?;
                    ok
                } else {
                    json!(mosh.paint(&reference)?)
                }
            }
            Command::Inspect { reference } => mosh.inspect(&reference)?,
            Command::Preview { reference } => json!(mosh.preview(&reference)?),
            Command::Open { reference } => {
                mosh.open_in_system(&reference)?;
                ok
            }
            Command::Folder { parent, name } => json!(mosh.create_folder(&parent, &name)?),
            Command::Duplicates { path } => {
                let cancel = on_interrupt()?;
                mosh.duplicates(path.as_deref(), &cancel)?
            }
            Command::Space { path } => mosh.space(&path)?,
            Command::Cleave {
                query,
                dry_run,
                action,
                argument,
                select,
                execute,
                undo,
                status,
                confirm,
            } => {
                if let Some(id) = execute {
                    let cancel = on_interrupt()?;
                    mosh.execute_plan(&id, confirm, &cancel)?
                } else if let Some(id) = undo {
                    mosh.undo_plan(&id, confirm)?
                } else if let Some(id) = status {
                    mosh.plan_status(&id)?
                } else {
                    ensure!(dry_run, "pass --dry-run to preview a Cleave plan");
                    let action = match action {
                        PlanAction::Copy => Action::Copy,
                        PlanAction::Move => Action::Move,
                        PlanAction::Quarantine => Action::Quarantine,
                        PlanAction::Tag => Action::Tag,
                        PlanAction::Marriage => Action::Marriage,
                        PlanAction::Manifest => Action::Manifest,
                    };
                    json!(mosh.plan(&query, action, argument.as_deref(), &select)?)
                }
            }
            Command::Coll { action } => collection(&mosh, action, false)?,
            Command::Marriage { action } => collection(&mosh, action, true)?,
            Command::Graph { reference } => json!(mosh.graph(reference.as_deref())?),
            Command::Save { name, query } => {
                mosh.saved_search(&name, &query)?;
                ok
            }
            Command::Saved => json!(mosh.saved_searches()?),
            Command::History { reference, git } => {
                if git {
                    json!(mosh.git_history(
                        &reference.context("provide a file or Coll for Git history")?
                    )?)
                } else {
                    json!(mosh.history(reference.as_deref(), 200)?)
                }
            }
            Command::Diff {
                reference,
                revision,
            } => json!(mosh.git_diff(&reference, revision.as_deref())?),
            Command::Person { id, superior } => {
                mosh.person(&id, superior.as_deref())?;
                ok
            }
            Command::Access {
                reference,
                person,
                permission,
            } => {
                mosh.access(&reference, &person, &permission)?;
                ok
            }
            Command::Secret { reference, disable } => {
                mosh.secret(&reference, !disable)?;
                ok
            }
            Command::Ledger {
                action: LedgerCommand::Rebuild,
            } => {
                mosh.rebuild()?;
                ok
            }
            Command::Ledger {
                action: LedgerCommand::Compact,
            } => {
                mosh.compact()?;
                mosh.doctor()?
            }
            Command::Ledger {
                action: LedgerCommand::TextStatus,
            } => mosh.text_status()?,
            Command::Ledger {
                action: LedgerCommand::TextIndex { max_bytes },
            } => {
                let cancel = on_interrupt()?;
                mosh.index_text(max_bytes, &cancel)?
            }
            Command::Doctor => mosh.doctor()?,
            Command::Backup { destination } => {
                mosh.backup(&destination)?;
                json!({"backed_up":destination})
            }
            Command::Restore { .. } | Command::Serve { .. } => unreachable!(),
        };
    Ok(Some(output))
}

fn main() -> std::process::ExitCode {
    match run(Cli::parse()) {
        Ok(Some(value)) => {
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            std::process::ExitCode::SUCCESS
        }
        Ok(None) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mosh: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
