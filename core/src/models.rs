use crate::{Mosh, now, tiers};
use anyhow::{Context, Result, ensure};
use reqwest::{
    Url,
    blocking::{Client, RequestBuilder},
    redirect::Policy,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Read,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub provider: String,
    pub endpoint: String,
    pub classification: String,
    pub embedding: String,
    pub reasoning: String,
    pub planning: String,
    pub fallback: String,
    pub allow_remote: bool,
    pub remote_excluded_paths: Vec<String>,
    pub daily_micros: i64,
    pub monthly_micros: i64,
    pub input_micros_per_million: i64,
    pub output_micros_per_million: i64,
    pub context_bytes: usize,
    pub output_tokens: usize,
    pub timeout_seconds: u64,
    pub concurrency: u8,
    pub retries: u8,
    pub mode: String,
    pub reasoning_effort: Option<String>,
    pub latency_ms: u64,
    pub search_mode: String,
    pub max_candidates: usize,
    pub learn: bool,
    pub semantic: bool,
    pub pass_hits: u32,
    pub self_name: String,
    pub background: bool,
    pub idle_seconds: u32,
    pub minimum_free_ram_mb: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "ollama".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            classification: String::new(),
            embedding: String::new(),
            reasoning: String::new(),
            planning: String::new(),
            fallback: String::new(),
            allow_remote: false,
            remote_excluded_paths: vec![],
            daily_micros: 0,
            monthly_micros: 0,
            input_micros_per_million: 0,
            output_micros_per_million: 0,
            context_bytes: 8192,
            output_tokens: 2048,
            timeout_seconds: 90,
            concurrency: 1,
            retries: 2,
            mode: "normal".into(),
            reasoning_effort: None,
            latency_ms: 10_000,
            search_mode: "balanced".into(),
            max_candidates: 20,
            learn: true,
            semantic: true,
            pass_hits: 3,
            self_name: String::new(),
            background: false,
            idle_seconds: 30,
            minimum_free_ram_mb: 512,
        }
    }
}

impl Config {
    pub fn endpoint(&self) -> Result<Url> {
        if self.provider == "openai" && self.endpoint.trim().is_empty() {
            return Ok(Url::parse("https://api.openai.com/v1")?);
        }
        let url = Url::parse(&self.endpoint)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid inference endpoint"
        );
        if !is_local(&url) {
            ensure!(url.scheme() == "https", "remote inference requires HTTPS");
        }
        if self.provider == "openai" {
            ensure!(
                url.host_str() == Some("api.openai.com"),
                "use the compatible provider for custom endpoints"
            );
        }
        Ok(url)
    }

    pub fn remote(&self) -> Result<bool> {
        Ok(!is_local(&self.endpoint()?))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.provider.as_str(), "ollama" | "openai" | "compatible"),
            "unknown inference provider"
        );
        ensure!(
            matches!(
                self.mode.as_str(),
                "off" | "light" | "normal" | "deep" | "exhaustive" | "custom"
            ),
            "unknown swarm mode"
        );
        self.endpoint()?;
        ensure!(
            matches!(
                self.search_mode.as_str(),
                "fast" | "balanced" | "exhaustive"
            ),
            "unknown search mode"
        );
        ensure!(
            (1..=8).contains(&self.concurrency)
                && self.retries <= 8
                && (1..=300).contains(&self.timeout_seconds),
            "check concurrency, retries and timeout"
        );
        ensure!(
            (256..=131_072).contains(&self.context_bytes)
                && (32..=8192).contains(&self.output_tokens)
                && (1..=200).contains(&self.max_candidates)
                && self.latency_ms > 0
                && self.pass_hits > 0,
            "invalid retrieval or model limits"
        );
        ensure!(
            [
                self.daily_micros,
                self.monthly_micros,
                self.input_micros_per_million,
                self.output_micros_per_million
            ]
            .iter()
            .all(|v| *v >= 0 && *v <= 1_000_000_000_000),
            "invalid pricing or spending limit"
        );
        ensure!(
            self.reasoning_effort.as_deref().is_none_or(|v| matches!(
                v,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )),
            "invalid reasoning effort"
        );
        ensure!(
            self.self_name.len() <= 120 && !self.self_name.chars().any(char::is_control),
            "invalid identity spelling"
        );
        for model in [
            &self.classification,
            &self.embedding,
            &self.reasoning,
            &self.planning,
            &self.fallback,
        ] {
            ensure!(
                model.len() <= 200 && !model.chars().any(char::is_control),
                "invalid model name"
            );
        }
        Ok(())
    }

    pub fn passes(&self, requested: u8) -> u8 {
        match self.mode.as_str() {
            "off" => 0,
            "light" => 1,
            "normal" => requested.min(2),
            "deep" => requested.min(4),
            "exhaustive" | "custom" => requested,
            _ => 0,
        }
    }

    pub fn background_ready(&self) -> bool {
        if !self.background
            || self.mode == "off"
            || free_memory()
                .is_some_and(|bytes| bytes < u64::from(self.minimum_free_ram_mb) * 1_048_576)
        {
            return false;
        }
        if self.idle_seconds == 0 {
            return true;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::{
                System::SystemInformation::GetTickCount,
                UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
            };
            let mut input = LASTINPUTINFO {
                cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            // Both timestamps are 32-bit Windows ticks and wrap together.
            unsafe {
                GetLastInputInfo(&mut input) != 0
                    && GetTickCount().wrapping_sub(input.dwTime) / 1000 >= self.idle_seconds
            }
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

fn is_local(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    )
}

pub fn config(db: &Connection) -> Result<Config> {
    tiers::setting(db, "intelligence")
}

pub struct Usage {
    pub input: i64,
    pub output: i64,
}

pub trait Provider {
    fn chat(
        &self,
        model: &str,
        prompt: &str,
        input: &str,
        schema: &Value,
    ) -> Result<(Value, Usage)>;
    fn embed(&self, model: &str, input: &str) -> Result<(Vec<f32>, Usage)>;
}

pub struct Http<'a> {
    cfg: &'a Config,
    url: String,
    client: Client,
    remote: bool,
}

impl<'a> Http<'a> {
    pub fn new(cfg: &'a Config) -> Result<Self> {
        cfg.validate()?;
        let endpoint = cfg.endpoint()?;
        let remote = !is_local(&endpoint);
        ensure!(!remote || cfg.allow_remote, "remote inference is disabled");
        let mut client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(cfg.timeout_seconds));
        if !remote {
            client = client.no_proxy();
        }
        Ok(Self {
            cfg,
            url: endpoint.as_str().trim_end_matches('/').into(),
            client: client.build()?,
            remote,
        })
    }

    fn checked_model(&self, model: &str) -> Result<()> {
        ensure!(
            !model.trim().is_empty(),
            "select a model before running inference"
        );
        if self.cfg.provider == "ollama" && !self.remote {
            ensure!(
                !model.to_ascii_lowercase().contains("cloud"),
                "cloud models require an explicitly remote endpoint"
            );
        }
        Ok(())
    }

    fn request(&self, route: &str, body: &Value) -> Result<Value> {
        let mut request = self.client.post(format!("{}{route}", self.url)).json(body);
        if self.cfg.provider != "ollama" {
            let credential = std::env::var("MOSH_AI_API_KEY").ok().or_else(|| {
                (self.cfg.provider == "openai")
                    .then(|| std::env::var("OPENAI_API_KEY").ok())
                    .flatten()
            });
            if self.remote {
                ensure!(
                    credential.is_some(),
                    "set MOSH_AI_API_KEY or OPENAI_API_KEY in the process environment"
                );
            }
            if let Some(token) = credential {
                request = request.bearer_auth(token);
            }
        }
        bounded(request)
    }
}

fn bounded(request: RequestBuilder) -> Result<Value> {
    let response = request
        .send()
        .map_err(|error| anyhow::anyhow!("inference connection failed: {}", error.without_url()))?;
    ensure!(
        response.status().is_success(),
        "inference endpoint returned HTTP {}",
        response.status()
    );
    let mut bytes = Vec::new();
    response.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 2 * 1024 * 1024,
        "inference response exceeded 2 MiB"
    );
    serde_json::from_slice(&bytes).context("inference endpoint did not return JSON")
}

pub fn clipped(value: &str, bytes: usize) -> &str {
    &value[..value.floor_char_boundary(value.len().min(bytes))]
}

impl Provider for Http<'_> {
    fn chat(
        &self,
        model: &str,
        prompt: &str,
        input: &str,
        schema: &Value,
    ) -> Result<(Value, Usage)> {
        self.checked_model(model)?;
        let messages = json!([{"role":"system","content":prompt},{"role":"user","content":input}]);
        let response = if self.cfg.provider == "ollama" {
            self.request("/api/chat",&json!({"model":model,"messages":messages,"format":schema,"stream":false,"think":false,
                "options":{"temperature":0,"num_predict":self.cfg.output_tokens}}))?
        } else {
            let mut body = json!({"model":model,"messages":messages,"response_format":{"type":"json_object"},"max_completion_tokens":self.cfg.output_tokens});
            if self.cfg.provider == "openai" {
                body["store"] = json!(false);
            }
            if let Some(effort) = &self.cfg.reasoning_effort {
                body["reasoning_effort"] = json!(effort);
            }
            self.request("/chat/completions", &body)?
        };
        let (text, input, output) = if self.cfg.provider == "ollama" {
            (
                response["message"]["content"].as_str(),
                response["prompt_eval_count"].as_i64(),
                response["eval_count"].as_i64(),
            )
        } else {
            (
                response["choices"][0]["message"]["content"].as_str(),
                response["usage"]["prompt_tokens"].as_i64(),
                response["usage"]["completion_tokens"].as_i64(),
            )
        };
        let text = text.context("model returned no text")?;
        let usage = Usage {
            input: input.unwrap_or(-1),
            output: output.unwrap_or(-1),
        };
        // Keep accounting data even when the model's text is malformed JSON.
        let result = serde_json::from_str(text).unwrap_or(Value::Null);
        Ok((result, usage))
    }

    fn embed(&self, model: &str, input: &str) -> Result<(Vec<f32>, Usage)> {
        self.checked_model(model)?;
        let response = if self.cfg.provider == "ollama" {
            self.request(
                "/api/embed",
                &json!({"model":model,"input":input,"truncate":true}),
            )?
        } else {
            self.request("/embeddings", &json!({"model":model,"input":input}))?
        };
        let (values, tokens) = if self.cfg.provider == "ollama" {
            (
                &response["embeddings"][0],
                response["prompt_eval_count"].as_i64(),
            )
        } else {
            (
                &response["data"][0]["embedding"],
                response["usage"]["prompt_tokens"].as_i64(),
            )
        };
        let vector: Vec<f32> = serde_json::from_value(values.clone())?;
        ensure!(
            (8..=8192).contains(&vector.len()) && vector.iter().all(|value| value.is_finite()),
            "invalid embedding"
        );
        Ok((
            vector,
            Usage {
                input: tokens.unwrap_or(-1),
                output: 0,
            },
        ))
    }
}

pub fn billed<T>(
    db: &Connection,
    cfg: &Config,
    operation: &str,
    key: Option<i64>,
    model: &str,
    input_bytes: usize,
    call: impl FnOnce() -> Result<(T, Usage)>,
) -> Result<T> {
    let remote = cfg.remote()?;
    let fee = |input: i64, output: i64| {
        ((i128::from(input) * i128::from(cfg.input_micros_per_million)
            + i128::from(output) * i128::from(cfg.output_micros_per_million)
            + 999_999)
            / 1_000_000)
            .min(i128::from(i64::MAX)) as i64
    };
    let estimate = if remote {
        fee(input_bytes as i64 + 4096, cfg.output_tokens as i64)
    } else {
        0
    };
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let time = now();
    if remote {
        ensure!(
            cfg.allow_remote
                && cfg.daily_micros > 0
                && cfg.monthly_micros > 0
                && cfg.input_micros_per_million > 0
                && cfg.output_micros_per_million > 0,
            "remote inference requires opt-in, pricing, and positive daily and monthly budgets"
        );
        let day = time - time.rem_euclid(86_400_000);
        let month = chrono::DateTime::from_timestamp_millis(time)
            .context("invalid time")?
            .format("%Y-%m-01")
            .to_string();
        let month = chrono::NaiveDate::parse_from_str(&month, "%Y-%m-%d")?
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let sum = |since: i64| {
            tx.query_row("SELECT coalesce(sum(coalesce(cost_micros,reserved_micros)),0) FROM ai_usage WHERE started>=?",[since],|r|r.get::<_,i64>(0))
        };
        ensure!(
            sum(day)?.saturating_add(estimate) <= cfg.daily_micros
                && sum(month)?.saturating_add(estimate) <= cfg.monthly_micros,
            "remote spending budget reached"
        );
    }
    tx.execute("INSERT INTO ai_usage(operation,object_key,provider,model,started,reserved_micros) VALUES(?,?,?,?,?,?)",
        params![operation,key,cfg.provider,model,time,estimate])?;
    let usage_id = tx.last_insert_rowid();
    tx.commit()?;
    let started = Instant::now();
    match call() {
        Ok((result, usage)) => {
            let cost = if !remote {
                0
            } else if usage.input < 0 || usage.output < 0 {
                estimate
            } else {
                fee(usage.input, usage.output)
            };
            db.execute("UPDATE ai_usage SET elapsed=?,input=?,output=?,cost_micros=?,outcome='ok' WHERE id=?",
                params![started.elapsed().as_millis() as i64,usage.input,usage.output,cost,usage_id])?;
            Ok(result)
        }
        Err(error) => {
            db.execute("UPDATE ai_usage SET elapsed=?,cost_micros=reserved_micros,outcome='failed' WHERE id=?",params![started.elapsed().as_millis() as i64,usage_id])?;
            Err(error)
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct LocalModel {
    name: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

fn local_models(cfg: &Config) -> Result<Vec<LocalModel>> {
    ensure!(
        cfg.provider == "ollama" && !cfg.remote()?,
        "model discovery requires local Ollama"
    );
    let discovery = Config {
        timeout_seconds: cfg.timeout_seconds.min(5),
        ..cfg.clone()
    };
    let http = Http::new(&discovery)?;
    let tags = bounded(http.client.get(format!("{}/api/tags", http.url)))?;
    let mut models: Vec<LocalModel> = serde_json::from_value(tags["models"].clone())?;
    models.retain(|model| !model.name.to_ascii_lowercase().contains("cloud"));
    Ok(models)
}

pub fn physical_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut stats = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // GlobalMemoryStatusEx fills the fixed-size structure.
        (unsafe { GlobalMemoryStatusEx(&mut stats) } != 0).then_some(stats.ullTotalPhys)
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()?
            .lines()
            .next()?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    }
}

fn free_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut stats = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // GlobalMemoryStatusEx fills the fixed-size structure.
        (unsafe { GlobalMemoryStatusEx(&mut stats) } != 0).then_some(stats.ullAvailPhys)
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()?
            .lines()
            .find(|line| line.starts_with("MemAvailable:"))?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    }
}

impl Mosh {
    pub fn ai_settings(&self) -> Result<Config> {
        self.admin()?;
        config(&self.db)
    }
    pub fn configure_ai(&self, cfg: &Config) -> Result<Config> {
        self.admin()?;
        cfg.validate()?;
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let previous = config(&tx)?;
        tiers::set_setting(&tx, "intelligence", cfg)?;
        if cfg.mode != "off"
            && (!cfg.classification.is_empty() || !cfg.embedding.is_empty())
            && (previous.classification != cfg.classification
                || previous.embedding != cfg.embedding
                || previous.provider != cfg.provider
                || previous.endpoint != cfg.endpoint
                || previous.mode == "off")
        {
            tx.execute("INSERT INTO ai_jobs(kind,object_key,revision,priority,queued)
                SELECT 'enrich',k.key,o.revision,coalesce(f.target*10+f.predicted_hotness,10),?
                FROM objects o JOIN object_keys k ON k.object=o.id LEFT JOIN file_tiers f ON f.object_key=k.key
                WHERE o.missing=0 AND o.reparse=0
                ON CONFLICT(kind,object_key,revision,scope) DO UPDATE SET state='queued',ready=0,attempts=0,error=NULL
                WHERE ai_jobs.state IN ('done','failed','obsolete')",[now()])?;
        }
        tx.commit()?;
        self.ai_settings()
    }
    pub fn model_status(&self) -> Result<Value> {
        self.admin()?;
        let cfg = config(&self.db)?;
        let (models, error) = match local_models(&cfg) {
            Ok(v) => (v, None),
            Err(e) => (vec![], Some(e.to_string())),
        };
        let ram = physical_memory();
        let suggested = if ram.is_some_and(|n| n >= 12 * 1024 * 1024 * 1024) {
            "qwen2.5:1.5b"
        } else {
            "qwen2.5:0.5b"
        };
        Ok(
            json!({"models":models.iter().map(|model| &model.name).collect::<Vec<_>>(),"capabilities":models,
                "error":error,"ram_bytes":ram,"logical_processors":std::thread::available_parallelism().ok().map(|v|v.get()),
            "suggested_chat":suggested,"suggested_embedding":"all-minilm:22m","credential_configured":std::env::var_os("MOSH_AI_API_KEY").is_some() ||
                (cfg.provider=="openai" && std::env::var_os("OPENAI_API_KEY").is_some())}),
        )
    }
    pub fn configure_detected_models(&self) -> Result<Config> {
        self.admin()?;
        let mut cfg = config(&self.db)?;
        let models = local_models(&cfg)?;
        let choose = |kind: &str| {
            models
                .iter()
                .find(|model| {
                    model
                        .capabilities
                        .iter()
                        .any(|capability| capability == kind)
                })
                .map(|model| model.name.clone())
        };
        let chat = choose("completion");
        let embedding = choose("embedding");
        ensure!(
            chat.is_some() || embedding.is_some(),
            "no installed models reported completion or embedding support"
        );
        if cfg.classification.is_empty()
            && let Some(chat) = chat
        {
            cfg.classification = chat;
        }
        if cfg.embedding.is_empty()
            && let Some(embedding) = embedding
        {
            cfg.embedding = embedding;
        }
        self.configure_ai(&cfg)
    }
}
