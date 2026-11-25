use crate::{Action, Mosh, SearchOptions, fsmeta};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, sync::atomic::AtomicBool};

fn required<'a>(payload: &'a Value, key: &str) -> Result<&'a str> {
    payload[key]
        .as_str()
        .with_context(|| format!("{key} is required"))
}

fn limit(payload: &Value) -> usize {
    payload["limit"].as_u64().unwrap_or(200).min(1000) as usize
}

impl Mosh {
    pub fn preview(&self, reference: &str) -> Result<String> {
        let item = self.get(reference)?;
        Ok(crate::content::preview(&item))
    }

    pub fn open_in_system(&self, reference: &str) -> Result<()> {
        self.admin()?;
        let item = self.get(reference)?;
        let path = Path::new(
            item.path
                .as_deref()
                .context("this object has no filesystem path")?,
        );
        ensure!(
            !item.reparse && !fsmeta::reparse(&fs::symlink_metadata(path)?),
            "use Windows Explorer to inspect the reparse target"
        );
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::UI::{
                Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL,
            };
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            // Null action uses the file association registered with Windows.
            let result = unsafe {
                ShellExecuteW(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    wide.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                )
            };
            ensure!(
                (result as isize) > 32,
                "Windows could not open this file ({})",
                result as isize
            );
        }
        #[cfg(not(windows))]
        {
            std::process::Command::new("xdg-open").arg(path).spawn()?;
        }
        Ok(())
    }

    pub fn command(&self, name: &str, payload: &Value, cancel: &AtomicBool) -> Result<Value> {
        let reference = || required(payload, "reference");
        let path = || required(payload, "path").map(Path::new);
        let query = || payload["query"].as_str().unwrap_or("");
        let value = match name {
            "roots" => json!(self.roots()?),
            "search" => {
                let options = if payload["options"].is_null() {
                    SearchOptions::default()
                } else {
                    serde_json::from_value(payload["options"].clone())?
                };
                json!(self.search(query(), &options)?)
            }
            "scan" => json!(self.scan(path()?, cancel)?),
            "inspect" => self.inspect(reference()?)?,
            "preview" => json!(self.preview(reference()?)?),
            "history" => json!(self.history(payload["reference"].as_str(), limit(payload))?),
            "git_history" => json!(self.git_history(reference()?)?),
            "git_diff" => json!(self.git_diff(reference()?, payload["revision"].as_str())?),
            "tag_catalog" => json!(self.tag_catalog(limit(payload))?),
            "tag_coverage" => self.tag_coverage()?,
            "tag" => {
                self.tag(
                    reference()?,
                    required(payload, "tag")?,
                    payload["add"].as_bool().unwrap_or(true),
                    1.0,
                )?;
                Value::Null
            }
            "paint" => {
                self.set_paint(
                    reference()?,
                    required(payload, "level")?,
                    required(payload, "body")?,
                    payload["locked"].as_bool().unwrap_or(false),
                )?;
                Value::Null
            }
            "collection" => json!(self.collection(
                required(payload, "name")?,
                payload["marriage"].as_bool().unwrap_or(false)
            )?),
            "connect" => {
                self.connect(
                    required(payload, "container")?,
                    reference()?,
                    required(payload, "kind")?,
                    payload["selector"].as_str(),
                )?;
                Value::Null
            }
            "graph" => json!(self.graph(payload["reference"].as_str())?),
            "saved" => json!(self.saved_searches()?),
            "save" => {
                self.saved_search(required(payload, "name")?, query())?;
                Value::Null
            }
            "space" => self.space(path()?)?,
            "duplicates" => self.duplicates(payload["path"].as_str().map(Path::new), cancel)?,
            "open" => {
                self.open_in_system(reference()?)?;
                Value::Null
            }
            "folder" => json!(self.create_folder(reference()?, required(payload, "name")?)?),
            "plan" => {
                let action: Action = serde_json::from_value(payload["action"].clone())?;
                let selected: Vec<String> = if payload["selected"].is_null() {
                    vec![]
                } else {
                    serde_json::from_value(payload["selected"].clone())?
                };
                json!(self.plan(query(), action, payload["argument"].as_str(), &selected)?)
            }
            "plan_status" => self.plan_status(required(payload, "id")?)?,
            "execute" => self.execute_plan(
                required(payload, "id")?,
                payload["confirmed"] == true,
                cancel,
            )?,
            "undo" => self.undo_plan(required(payload, "id")?, payload["confirmed"] == true)?,
            "doctor" => self.doctor()?,
            _ => anyhow::bail!("unknown Mosh command"),
        };
        Ok(value)
    }
}
