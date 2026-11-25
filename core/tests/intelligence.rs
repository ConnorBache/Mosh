use anyhow::{Context, Result};
use chrono::{Datelike, Duration, Local};
use mosh_core::{
    Mosh, SearchOptions,
    tiers::{Tier, Weights},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

struct Fixture {
    mosh: Mosh,
    root: PathBuf,
    db: PathBuf,
    _temporary: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("source");
        fs::create_dir(&root)?;
        let db = temporary.path().join("ledger.sqlite3");
        let mosh = Mosh::open(&db)?;
        Ok(Self {
            mosh,
            root,
            db,
            _temporary: temporary,
        })
    }
    fn write(&self, name: &str, source: &str) -> Result<PathBuf> {
        let path = self.root.join(name);
        fs::write(&path, source)?;
        Ok(path)
    }
    fn scan(&self) -> Result<()> {
        let result = self.mosh.scan(&self.root, &AtomicBool::new(false))?;
        assert_eq!(result.errors, 0, "{result:?}");
        Ok(())
    }
    fn indexed(&self, path: &Path) -> Result<String> {
        Ok(self.mosh.get(path.to_str().context("test path")?)?.id)
    }
}

#[test]
fn tier_floors_follow_shortlist_clues_and_file_size_without_default_rows() -> Result<()> {
    let f = Fixture::new()?;
    let mut paths = Vec::new();
    for n in 0..20 {
        paths.push(f.write(&format!("proposal-{n:02}.txt"), "Review this proposal.")?);
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&paths[1])?
        .set_len(1024 * 1024)?;
    f.scan()?;
    let ids = paths
        .iter()
        .map(|file| f.indexed(file))
        .collect::<Result<Vec<_>>>()?;
    let mut tiers: Vec<Tier> = serde_json::from_value(f.mosh.tier_settings()?["tiers"].clone())?;
    assert_eq!(
        tiers
            .iter()
            .map(|tier| tier.minimum_individuality)
            .collect::<Vec<_>>(),
        [0, 60, 80, 90]
    );
    let initial = f.mosh.file_tier(&ids[0])?;
    assert_eq!(initial["stored_assessment"], false);
    assert_eq!(initial["individuality_score"], 0);
    let raw = rusqlite::Connection::open(&f.db)?;
    assert_eq!(
        raw.query_row("SELECT count(*) FROM file_tiers", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    assert!(f.mosh.override_tier(&ids[0], Some(5), false).is_err());
    f.mosh.override_tier(&ids[0], Some(4), true)?;
    f.mosh.override_tier(&ids[1], Some(4), true)?;
    for (n, id) in ids.iter().enumerate() {
        if n < 10 {
            f.mosh.tag(id, "project-a", true, 1.0)?;
        }
        if n < 13 && n != 9 {
            f.mosh.tag(id, "region-b", true, 1.0)?;
        }
        if n < 8 || n == 9 || (13..16).contains(&n) {
            f.mosh.tag(id, "version-c", true, 1.0)?;
        }
    }
    let small = f.mosh.file_tier(&ids[0])?;
    let large = f.mosh.file_tier(&ids[1])?;
    assert_eq!(small["shortlist"]["pieces_required"], 2);
    assert_eq!(small["shortlist"]["candidates"], 8);
    assert_eq!(small["individuality_score"], 90);
    assert_eq!(small["effective"], 4);
    assert_eq!(large["minimum_individuality"], 100);
    assert_eq!(large["effective"], 3);
    assert_eq!(large["locked"], true);
    let displayed = f.mosh.individuality(&ids[0])?;
    assert_eq!(displayed["score"], small["individuality_score"]);
    assert_eq!(displayed["shortlist"]["pieces_required"], 2);
    tiers[3].minimum_individuality = 95;
    f.mosh.configure_tiers(&tiers, &Weights::default())?;
    assert_eq!(f.mosh.file_tier(&ids[0])?["effective"], 3);
    f.mosh.override_tier(&ids[0], None, false)?;
    assert_eq!(f.mosh.file_tier(&ids[0])?["stored_assessment"], false);
    assert_eq!(
        raw.query_row("SELECT count(*) FROM file_tiers", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn discover_only_cannot_reveal_private_tags_paint_or_source() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir(f.root.join("protected"))?;
    let file = f.write("protected/visible.txt", "bluepassage exemplar")?;
    f.scan()?;
    let id = f.indexed(&file)?;
    let coll = f.indexed(&f.root.join("protected"))?;
    f.mosh.person("visitor", None)?;
    f.mosh.tag(&id, "privateterm", true, 1.0)?;
    f.mosh.set_paint(&id, "minimal", "bluepaint", true)?;
    f.mosh
        .coll_security(&coll, Some(true), Some("visitor"), &["discover".into()])?;
    let visitor = Mosh::as_actor(&f.db, "visitor")?;
    assert_eq!(visitor.get(&id)?.name, "visible.txt");
    assert!(visitor.paint(&id)?.is_none());
    assert!(visitor.tags(&id)?.is_empty());
    assert!(visitor.preview(&id).is_err());
    assert!(visitor.passages(&id).is_err());
    assert!(
        visitor
            .tag_catalog(100)?
            .iter()
            .all(|item| item["name"] != "privateterm")
    );
    for query in ["content:bluepassage", "paint:bluepaint", "tag:privateterm"] {
        assert_eq!(f.mosh.search(query, &SearchOptions::default())?.len(), 1);
        assert!(
            visitor.search(query, &SearchOptions::default())?.is_empty(),
            "{query}"
        );
    }
    f.mosh.coll_security(
        &coll,
        None,
        Some("visitor"),
        &["discover".into(), "content".into()],
    )?;
    assert_eq!(visitor.preview(&id)?, "bluepassage exemplar");
    assert!(visitor.paint(&id)?.is_none());
    Ok(())
}

#[test]
fn registry_covers_older_revisions_and_retracts_stale_source_tags() -> Result<()> {
    let f = Fixture::new()?;
    let source = f.write(
        "old-thread.txt",
        "2025-06-10\nJohn: Matt, can you handle the Syndigo inventory audit?\nMatt: Yes.",
    )?;
    f.scan()?;
    let id = f.indexed(&source)?;
    let tag = "topic:syndigo";
    let created =
        f.mosh
            .maintain_tag(tag, "create", Some("topic"), Some("Syndigo project"), None)?;
    let stable_id = created[0]["id"].clone();
    assert!(f.mosh.tags(&id)?.iter().all(|entry| entry["name"] != tag));
    assert_eq!(f.mosh.queue_backfill(tag, "ext:txt", 30)?["queued"], 1);
    assert_eq!(
        f.mosh.run_agents(1, &AtomicBool::new(false))?["completed"],
        1
    );
    assert_eq!(f.mosh.queue_backfill(tag, "ext:txt", 30)?["queued"], 0);
    assert!(f.mosh.tags(&id)?.iter().any(|entry| entry["name"] == tag));
    f.mosh.tag_dictionary("alias", tag, Some("topic:catalog"))?;
    assert_eq!(
        f.mosh.tag_registry(Some("topic:catalog"), 10)?[0]["id"],
        stable_id
    );
    assert_eq!(
        f.mosh.tag_preview(tag, 10)?["matches"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::write(&source, "The current report is about bicycle maintenance.")?;
    f.scan()?;
    assert!(f.mosh.tags(&id)?.iter().all(|entry| entry["name"] != tag));
    assert!(f.mosh.passages(&id)?.is_empty());
    assert!(
        f.mosh
            .tag_history(tag, 20)?
            .iter()
            .any(|event| event["action"] == "stale" && event["revision"] == 1)
    );
    assert_eq!(f.mosh.queue_backfill(tag, "ext:txt", 30)?["queued"], 1);
    f.mosh.maintain_tag(tag, "retire", None, None, None)?;
    assert!(f.mosh.tag_registry(Some(tag), 20)?[0]["deprecated_at"].is_number());
    f.mosh.maintain_tag(tag, "reactivate", None, None, None)?;
    assert_eq!(f.mosh.tag_registry(Some(tag), 20)?[0]["id"], stable_id);
    Ok(())
}

#[test]
fn remote_inference_reserves_budget_before_work() -> Result<()> {
    use mosh_core::models::{Config, Usage, billed};
    let f = Fixture::new()?;
    let db = rusqlite::Connection::open(&f.db)?;
    let mut config = Config {
        provider: "openai".into(),
        endpoint: String::new(),
        ..Config::default()
    };
    let canary =
        || -> Result<((), Usage)> { panic!("the remote closure ran before budget authorization") };
    assert!(billed(&db, &config, "budget-test", None, "test", 128, canary).is_err());
    config.allow_remote = true;
    config.daily_micros = 7_000;
    config.monthly_micros = 10_000;
    config.input_micros_per_million = 1_000_000;
    config.output_micros_per_million = 1_000_000;
    billed(&db, &config, "budget-test", None, "test", 128, || {
        Ok((
            (),
            Usage {
                input: 3999,
                output: 5,
            },
        ))
    })?;
    assert!(billed(&db, &config, "budget-test", None, "test", 128, canary).is_err());
    assert_eq!(f.mosh.ai_usage()?[0]["cost_micros"], 4004);
    Ok(())
}

#[test]
fn structured_model_output_adds_only_grounded_tags_and_respects_tier_paint() -> Result<()> {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };
    let f = Fixture::new()?;
    let path = f.write(
        "proposal.txt",
        "Ada proposes a Syndigo inventory audit in Chicago.",
    )?;
    let response = json!({"message":{"content":json!({
        "scores":{"importance":70,"information":70,"hotness":70},
        "tags":[{"name":"topic:syndigo","evidence":"Syndigo inventory audit","confidence":0.9},
            {"name":"topic:inventory","evidence":"Syndigo inventory audit","confidence":0.9},
            {"name":"location:chicago","evidence":"in Chicago","confidence":0.9},
            {"name":"location:london","evidence":"in Chicago","confidence":0.99}],
        "events":[{"kind":"assignment","quote":"Ada proposes a Syndigo inventory audit in Chicago.",
            "actor":"Ada","recipient":"Chicago","topic":"inventory audit"}],
        "paint":"Ada proposes an inventory audit for Syndigo in Chicago."
    }).to_string()},"prompt_eval_count":35,"eval_count":50});
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    listener.set_nonblocking(true)?;
    let server = thread::spawn(move || -> Result<()> {
        let start = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && start.elapsed() < Duration::from_secs(10) =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error.into()),
            }
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        assert!(line.starts_with("POST /api/chat "));
        let mut length = 0;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line)? > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse()?;
            }
        }
        let mut request = vec![0; length];
        reader.read_exact(&mut request)?;
        let request: Value = serde_json::from_slice(&request)?;
        assert_eq!(request["model"], "controlled-fixture");
        let body = response.to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )?;
        Ok(())
    });
    let mut cfg = f.mosh.ai_settings()?;
    cfg.endpoint = endpoint;
    cfg.classification = "controlled-fixture".into();
    cfg.timeout_seconds = 5;
    cfg.mode = "light".into();
    f.mosh.configure_ai(&cfg)?;
    let mut tiers: Vec<Tier> = serde_json::from_value(f.mosh.tier_settings()?["tiers"].clone())?;
    tiers[1].paint = Some("minimal".into());
    f.mosh.configure_tiers(&tiers, &Weights::default())?;
    f.scan()?;
    let id = f.indexed(&path)?;
    let run = f.mosh.run_agents(1, &AtomicBool::new(false))?;
    server.join().expect("mock local inference server")?;
    assert_eq!(run["completed"], 1, "{:?}", f.mosh.ai_queue(1)?);
    let assessed = f.mosh.file_tier(&id)?;
    assert_eq!(assessed["assessor"], "ai");
    assert_eq!(assessed["target"], 3);
    assert_eq!(assessed["effective"], 3);
    assert_eq!(assessed["shortlist"]["pieces_required"], 0);
    assert_eq!(assessed["direct_tags"], 6);
    assert_eq!(f.mosh.paint(&id)?.unwrap()["level"], "minimal");
    assert!(f.mosh.passages(&id)?.is_empty());
    assert!(
        f.mosh
            .tags(&id)?
            .iter()
            .all(|row| row["name"] != "location:london")
    );
    assert_eq!(f.mosh.ai_usage()?[0]["input_tokens"], 35);
    Ok(())
}

#[test]
fn related_conversations_are_scoped_and_confirmed_miss_improves() -> Result<()> {
    let f = Fixture::new()?;
    let today = Local::now().date_naive();
    let last_week = today - Duration::days(i64::from(today.weekday().num_days_from_monday()) + 3);
    let original = f.write("message-1.txt", &format!("{last_week}\nJohn: Matt, can you handle the Syndigo inventory audit?\nMatt: Sure, I'll start next week.\nCarol: Daniel, can you fix the billing integration?\nDaniel: Sure."))?;
    f.write(
        "message-2.txt",
        &format!("{last_week}\nMatt: John, can you review game engine movement?\nJohn: Yes."),
    )?;
    f.write(
        "message-3.txt",
        "Syndigo inventory audit. John and Matt are on the mailing list.",
    )?;
    f.write(
        "message-4.txt",
        &format!(
            "{today}\nJohn: Matt, the deadline for the Syndigo inventory audit moves to 2030-12-09."
        ),
    )?;
    f.write("pathfinding.rs", "fn walk_to_target() {}")?;
    for n in 0..120 {
        f.write(
            &format!("meeting-{n:03}.txt"),
            "John, Matt and Syndigo appear in the contact directory. No requests.",
        )?;
    }
    let mut cfg = f.mosh.ai_settings()?;
    cfg.self_name = "Matt".into();
    f.mosh.configure_ai(&cfg)?;
    f.scan()?;
    let id = f.indexed(&original)?;
    let question = "What did John assign Matt last week?";
    let miss = f.mosh.ask(question, false)?;
    assert!(miss["results"].as_array().unwrap().is_empty());
    f.mosh
        .retrieval_feedback(miss["id"].as_i64().unwrap(), &id, true, None)?;
    f.mosh.run_agents(1, &AtomicBool::new(false))?;
    let journal = f.mosh.ai_queue(10)?;
    let improvement = &journal[0]["result"];
    assert_eq!(improvement["baseline_rank"], Value::Null, "{journal:?}");
    assert_eq!(improvement["new_rank"], 1, "{journal:?}");
    assert_eq!(f.mosh.file_tier(&id)?["human_accesses"], 1);
    f.mosh.queue_index("name-contains:message", 30, false)?;
    f.mosh.run_agents(20, &AtomicBool::new(false))?;
    let queries = [
        (question, "message-1.txt"),
        ("What was I supposed to start for Syndigo?", "message-1.txt"),
        ("What deadline did we eventually agree on?", "message-4.txt"),
        ("What did John ask Matt to do?", "message-1.txt"),
        (
            "Find the original conversation about the inventory audit.",
            "message-1.txt",
        ),
    ];
    let mut total_ms = 0;
    let mut returned = 0;
    let mut relevant = 0;
    for (question, expected) in queries {
        let answer = f.mosh.ask(question, true)?;
        let rows = answer["results"].as_array().unwrap();
        assert_eq!(
            rows.first().context(question)?["object"]["name"],
            expected,
            "{answer}"
        );
        assert!(rows[0]["citation"]["quote"].as_str().is_some(), "{answer}");
        returned += rows.len();
        relevant += rows
            .iter()
            .filter(|row| row["object"]["name"] == expected)
            .count();
        total_ms += answer["metrics"]["total_ms"].as_u64().unwrap();
    }
    assert_eq!(relevant, 5);
    assert_eq!(returned, 5);
    assert!(
        f.mosh.ask("What did John ask Daniel to do?", false)?["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let baseline = f.mosh.search(
        "content:Syndigo",
        &SearchOptions {
            limit: 200,
            ..Default::default()
        },
    )?;
    assert!(baseline.len() >= 120);
    println!(
        "retrieval: {relevant}/5 first, {relevant}/{returned} precision, mean {:.2} ms; keyword-only candidate count {}; confirmed miss unranked -> rank 1",
        total_ms as f64 / 5.0,
        baseline.len()
    );
    Ok(())
}

#[test]
#[ignore = "run with a temporary Ollama server and the selected local models"]
fn real_ollama_integration() -> Result<()> {
    let f = Fixture::new()?;
    let meaningful = f.write("doc-a.txt", "The dealership repairs hybrid automobiles. Mechanics replace brake pads and inspect tyre wear on every car.")?;
    f.write(
        "doc-b.txt",
        "Combine flour and sugar with butter, then bake the pastry in a hot oven.",
    )?;
    f.write(
        "doc-c.txt",
        "The museum curates ancient sculpture and purchases works on paper.",
    )?;
    let mut cfg = f.mosh.ai_settings()?;
    cfg.endpoint = std::env::var("MOSH_OLLAMA_TEST_URL")?;
    cfg.mode = "deep".into();
    cfg.timeout_seconds = 60;
    cfg.output_tokens = 384;
    f.mosh.configure_ai(&cfg)?;
    let detected = f.mosh.configure_detected_models()?;
    assert_eq!(detected.classification, "smollm2:135m-instruct-q4_0");
    assert_eq!(detected.embedding, "all-minilm:22m");
    f.scan()?;
    let id = f.indexed(&meaningful)?;
    f.mosh
        .set_paint(&id, "minimal", "Mechanic's locked note", true)?;
    let elapsed = Instant::now();
    let jobs = f.mosh.run_agents(10, &AtomicBool::new(false))?;
    let usage = f.mosh.ai_usage()?;
    let queue = f.mosh.ai_queue(10)?;
    println!(
        "real indexing: {:.2?}, jobs {jobs}, queue {queue:?}, usage {usage:?}",
        elapsed.elapsed()
    );
    assert_eq!(jobs["completed"], 3);
    assert_eq!(
        f.mosh.paint(&id)?.unwrap()["body"],
        "Mechanic's locked note"
    );
    assert!(
        usage.iter().any(|entry| entry["operation"] == "enrich"),
        "{usage:?}"
    );
    let result = f.mosh.ask("Where can I get a vehicle serviced?", false)?;
    println!(
        "real query: {}",
        json!({"results":result["results"],"notes":result["notes"],"metrics":result["metrics"]})
    );
    assert_eq!(result["results"][0]["object"]["id"], id, "{result}");
    assert!(
        result["results"][0]["why"]
            .as_array()
            .unwrap()
            .contains(&json!("semantic similarity"))
    );
    Ok(())
}
