#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod workspaces;

use mosh_core::{Mosh, default_db};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tauri::Emitter;
use workspaces::Workspaces;

struct Desktop {
    workspaces: Workspaces,
    running: Mutex<Option<Arc<AtomicBool>>>,
    watch_cancel: Mutex<Arc<AtomicBool>>,
}

fn start_watches(app: &Arc<Desktop>, window: tauri::AppHandle) {
    let cancel = Arc::new(AtomicBool::new(false));
    let previous = std::mem::replace(&mut *app.watch_cancel.lock().unwrap(), cancel.clone());
    previous.store(true, Ordering::Relaxed);
    let (id, db) = app.workspaces.active();
    if !db.is_file() {
        return;
    }
    let app = Arc::clone(app);
    thread::spawn(move || {
        let roots = Mosh::open(&db).and_then(|mosh| {
            app.workspaces.configure_monitor(&mosh)?;
            mosh.registered_roots()
        });
        match roots {
            Ok(roots) => {
                for root in roots {
                    let (app, window, id, db, cancel) = (
                        app.clone(),
                        window.clone(),
                        id.clone(),
                        db.clone(),
                        cancel.clone(),
                    );
                    thread::spawn(move || {
                        let mut last_error = None;
                        let mut root = root;
                        while !cancel.load(Ordering::Relaxed) {
                            let result = Mosh::open(&db).and_then(|mosh| {
                                if id.is_some()
                                    && let Some(current) =
                                        mosh.registered_roots()?.into_iter().next()
                                {
                                    root = current;
                                }
                                let mut initial = true;
                                mosh.watch(&root, &cancel, |report| {
                                    if cancel.load(Ordering::Relaxed) {
                                        return;
                                    }
                                    match report {
                                        Ok(report) => {
                                            let recovered = last_error.take().is_some();
                                            let registered = id.as_ref().and_then(|id| {
                                                let path = mosh.registered_roots().ok()?.into_iter().next()?;
                                                let _ = app.workspaces.note_root(id, &path);
                                                Some(path)
                                            });
                                            if initial || recovered || report.changed > 0
                                                || report.missing > 0 || report.errors > 0 {
                                                let _ = window.emit("mosh-changed", json!({
                                                    "id":id,"root":registered,"warnings":report.warnings
                                                }));
                                            }
                                            initial = false;
                                        }
                                        Err(error) => {
                                            let error = format!("{error:#}");
                                            if last_error.as_ref() != Some(&error) {
                                                let _ = window.emit(
                                                    "mosh-changed",
                                                    json!({"id":id,"warnings":[error]}),
                                                );
                                                last_error = Some(error);
                                            }
                                        }
                                    }
                                })
                            });
                            if let Err(error) = result {
                                let error = format!("{error:#}");
                                if !cancel.load(Ordering::Relaxed)
                                    && last_error.as_ref() != Some(&error)
                                {
                                    let _ = window
                                        .emit("mosh-changed", json!({"id":id,"warnings":[error]}));
                                    last_error = Some(error);
                                }
                            }
                            if !cancel.load(Ordering::Relaxed) {
                                thread::sleep(Duration::from_secs(2));
                            }
                        }
                    });
                }
            }
            Err(error) => {
                let _ = window.emit(
                    "mosh-changed",
                    json!({"id":id,"warnings":[format!("{error:#}")]}),
                );
            }
        }
    });
}

#[tauri::command]
async fn ledger(
    app: tauri::State<'_, Arc<Desktop>>,
    window: tauri::AppHandle,
    name: String,
    payload: Value,
) -> Result<Value, String> {
    let app = Arc::clone(app.inner());
    if name == "cancel" {
        let lock = app
            .running
            .lock()
            .map_err(|_| "desktop task state unavailable")?;
        if let Some(cancel) = &*lock {
            cancel.store(true, Ordering::Relaxed);
        }
        return Ok(Value::Null);
    }
    let background = matches!(
        name.as_str(),
        "scan"
            | "duplicates"
            | "execute"
            | "undo"
            | "ai_run"
            | "workspace_build"
            | "workspace_open"
    );
    let cancel = Arc::new(AtomicBool::new(false));
    if background {
        let mut lock = app
            .running
            .lock()
            .map_err(|_| "desktop task state unavailable")?;
        if lock.is_some() {
            return Err("Another filesystem task is running".into());
        }
        *lock = Some(cancel.clone());
    }
    let (_, db) = app.workspaces.active();
    let restart_watches = matches!(name.as_str(), "workspace_build" | "workspace_open" | "scan");
    let worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let run = || -> anyhow::Result<Value> {
            Ok(match name.as_str() {
                "workspace_list" => json!(worker.workspaces.catalog()),
                "workspace_build" => {
                    let path = payload["path"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("select a directory"))?;
                    let (catalog, report) = worker.workspaces.build(Path::new(path), &cancel)?;
                    json!({"catalog":catalog,"report":report})
                }
                "workspace_open" => json!(worker.workspaces.select(payload["id"].as_str())?),
                _ => Mosh::open(&db)?.command(&name, &payload, &cancel)?,
            })
        };
        run().map_err(|error| format!("{error:#}"))
    })
    .await;
    if background {
        *app.running
            .lock()
            .map_err(|_| "desktop task state unavailable")? = None;
    }
    let result = result.map_err(|error| error.to_string())?;
    if restart_watches && result.is_ok() {
        start_watches(&app, window);
    }
    result
}

fn main() {
    let db = std::env::var_os("MOSH_DB")
        .map(PathBuf::from)
        .unwrap_or_else(default_db);
    let state = Arc::new(Desktop {
        workspaces: Workspaces::open(db).expect("opening the Mosh catalog"),
        running: Mutex::new(None),
        watch_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
    });
    let worker = Arc::clone(&state);
    tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            start_watches(&worker, app.handle().clone());
            std::thread::Builder::new()
                .name("mosh-agents".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        let (_, db) = worker.workspaces.active();
                        if worker
                            .running
                            .lock()
                            .map_or(true, |running| running.is_some())
                            || !db.is_file()
                        {
                            continue;
                        }
                        if let Ok(mosh) = Mosh::open(&db)
                            && mosh
                                .ai_settings()
                                .is_ok_and(|settings| settings.background_ready())
                        {
                            let _ = mosh.run_agents(1, &AtomicBool::new(false));
                        }
                    }
                })?;
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![ledger])
        .run(tauri::generate_context!())
        .expect("starting the Mosh desktop application");
}
