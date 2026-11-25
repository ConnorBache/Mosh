#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mosh_core::{Mosh, default_db};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct Desktop {
    db: PathBuf,
    running: Mutex<Option<Arc<AtomicBool>>>,
}

#[tauri::command]
async fn ledger(
    app: tauri::State<'_, Arc<Desktop>>,
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
        "scan" | "duplicates" | "execute" | "undo" | "ai_run"
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
    let db = app.db.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        Mosh::open(&db)
            .and_then(|mosh| mosh.command(&name, &payload, &cancel))
            .map_err(|error| format!("{error:#}"))
    })
    .await;
    if background {
        *app.running
            .lock()
            .map_err(|_| "desktop task state unavailable")? = None;
    }
    result.map_err(|error| error.to_string())?
}

fn main() {
    let db = std::env::var_os("MOSH_DB")
        .map(PathBuf::from)
        .unwrap_or_else(default_db);
    let state = Arc::new(Desktop {
        db,
        running: Mutex::new(None),
    });
    let worker = Arc::clone(&state);
    tauri::Builder::default()
        .manage(state)
        .setup(move |_| {
            std::thread::Builder::new()
                .name("mosh-agents".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        if worker
                            .running
                            .lock()
                            .map_or(true, |running| running.is_some())
                            || !worker.db.is_file()
                        {
                            continue;
                        }
                        if let Ok(mosh) = Mosh::open(&worker.db)
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
