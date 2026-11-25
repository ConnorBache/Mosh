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
        let item = self.find(reference)?;
        self.require_contents(&item)?;
        let text = crate::content::preview(&item);
        #[cfg(windows)]
        if text.is_empty() {
            let mut item = item;
            if self.recover_windows_path(&mut item, "content")? {
                return Ok(crate::content::preview(&item));
            }
        }
        Ok(text)
    }

    pub fn open_in_system(&self, reference: &str) -> Result<()> {
        self.admin()?;
        let mut item = self.get(reference)?;
        ensure!(
            fsmeta::populate_basic(&mut item),
            "the indexed file is no longer at its recorded location"
        );
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
        self.human_access(&item)?;
        Ok(())
    }

    pub fn command(&self, name: &str, payload: &Value, cancel: &AtomicBool) -> Result<Value> {
        let reference = || required(payload, "reference");
        let path = || required(payload, "path").map(Path::new);
        let query = || payload["query"].as_str().unwrap_or("");
        let value = match name {
            "roots" => json!(self.roots()?),
            "live_objects" => {
                let ids: Vec<String> = serde_json::from_value(payload["references"].clone())?;
                json!(
                    ids.iter()
                        .filter_map(|id| self.find(id).ok())
                        .collect::<Vec<_>>()
                )
            }
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
            "storage_usage" => self.storage_usage()?,
            "mosh_info" => json!({"mosh":self.mosh_id()?,"children":self.child_pointers("",None)?}),
            "mosh_route" => json!(self.child_pointers(query(), payload["child"].as_str())?),
            "mosh_attach" => self.register_child(
                path()?,
                Path::new(required(payload, "root")?),
                required(payload, "name")?,
                payload["paint"].as_str().unwrap_or(""),
                &payload
                    .get("tags")
                    .cloned()
                    .map(serde_json::from_value::<Vec<String>>)
                    .transpose()?
                    .unwrap_or_default(),
            )?,
            "mosh_detach" => self.detach_child(required(payload, "child")?)?,
            "mosh_discovery" => self.child_discovery(
                required(payload, "child")?,
                required(payload, "actor")?,
                payload["enabled"] == true,
            )?,
            "mosh_allow" => self.grant_handoff(
                required(payload, "parent")?,
                required(payload, "actor")?,
                payload["enabled"] == true,
            )?,
            "mosh_handoff" => self.child_handoff(
                required(payload, "child")?,
                query(),
                payload["natural"] == true,
            )?,
            "tier" => self.file_tier(reference()?)?,
            "individuality" => self.individuality(reference()?)?,
            "vocabulary_advice" => self.vocabulary_advice(reference()?)?,
            "vocabulary_apply" => self.apply_vocabulary_tag(
                reference()?,
                required(payload, "tag")?,
                payload["confirmed"] == true,
            )?,
            "tier_settings" => self.tier_settings()?,
            "configure_tiers" => self.configure_tiers(
                &serde_json::from_value::<Vec<crate::tiers::Tier>>(payload["tiers"].clone())?,
                &serde_json::from_value(payload["weights"].clone())?,
            )?,
            "tier_override" => self.override_tier(
                reference()?,
                payload["requested"]
                    .as_u64()
                    .map(u8::try_from)
                    .transpose()?,
                payload["locked"].as_bool().unwrap_or(false),
            )?,
            "tier_reassess" => self.reassess_tier(reference()?)?,
            "ai_settings" => serde_json::to_value(self.ai_settings()?)?,
            "configure_ai" => {
                serde_json::to_value(self.configure_ai(&serde_json::from_value(payload.clone())?)?)?
            }
            "model_status" => self.model_status()?,
            "configure_detected_models" => serde_json::to_value(self.configure_detected_models()?)?,
            "queue_index" => self.queue_index(query(), limit(payload), payload["force"] == true)?,
            "queue_backfill" => {
                ensure!(
                    limit(payload) <= 100 || payload["confirmed"] == true,
                    "confirm the large backfill"
                );
                self.queue_backfill(required(payload, "tag")?, query(), limit(payload))?
            }
            "ai_queue" => json!(self.ai_queue(limit(payload))?),
            "ai_usage" => json!(self.ai_usage()?),
            "ai_run" => self.run_agents(limit(payload), cancel)?,
            "ai_policy" => self.ai_policy(
                reference()?,
                payload["mode"].as_str(),
                payload["allow_remote"] == true,
            )?,
            "ask" => self.ask(required(payload, "query")?, payload["answer"] == true)?,
            "feedback" => self.retrieval_feedback(
                payload["run"].as_i64().context("run is required")?,
                reference()?,
                payload["helpful"]
                    .as_bool()
                    .context("helpful is required")?,
                payload["passage"].as_i64(),
            )?,
            "passages" => json!(self.passages(reference()?)?),
            "advisor" => {
                let update = payload["id"].as_i64().zip(payload["state"].as_str());
                json!(self.advisor(update)?)
            }
            "tag_registry" => json!(self.tag_registry(payload["tag"].as_str(), limit(payload))?),
            "tag_history" => json!(self.tag_history(required(payload, "tag")?, limit(payload))?),
            "maintain_tag" => self.maintain_tag(
                required(payload, "tag")?,
                required(payload, "action")?,
                payload["category"].as_str(),
                payload["description"].as_str(),
                payload["replacement"].as_str(),
            )?,
            "tag_preview" => self.tag_preview(required(payload, "tag")?, limit(payload))?,
            "tag_dictionary" => {
                ensure!(
                    !matches!(payload["action"].as_str(), Some("merge" | "delete"))
                        || payload["confirmed"] == true,
                    "confirm the vocabulary change"
                );
                self.tag_dictionary(
                    required(payload, "action")?,
                    required(payload, "tag")?,
                    payload["into"].as_str(),
                )?;
                Value::Null
            }
            "tag_split" => self.split_tag(
                required(payload, "tag")?,
                required(payload, "into")?,
                query(),
                payload["confirmed"] == true,
            )?,
            "coll_security" => self.coll_security(
                reference()?,
                payload["restricted"].as_bool(),
                payload["person"].as_str(),
                &serde_json::from_value::<Vec<String>>(
                    payload["rights"]
                        .as_array()
                        .map(|rights| json!(rights))
                        .unwrap_or(json!([])),
                )?,
            )?,
            _ => anyhow::bail!("unknown Mosh command"),
        };
        Ok(value)
    }
}
