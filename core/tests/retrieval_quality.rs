use anyhow::{Context, Result};
use mosh_core::Mosh;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf, sync::atomic::AtomicBool, time::Instant};

struct Corpus {
    _temp: tempfile::TempDir,
    mosh: Mosh,
    root: PathBuf,
    db: PathBuf,
}
impl Corpus {
    fn create() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("source");
        let db = temp.path().join("ledger.sqlite3");
        fs::create_dir(&root)?;
        let mosh = Mosh::open(&db)?;
        Ok(Self {
            _temp: temp,
            mosh,
            root,
            db,
        })
    }
    fn file(&self, path: &str, source: &str) -> Result<()> {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        Ok(fs::write(path, source)?)
    }
    fn scan(&self) -> Result<()> {
        let report = self.mosh.scan(&self.root, &AtomicBool::new(false))?;
        assert_eq!(report.errors, 0, "{:?}", report.warnings);
        Ok(())
    }
    fn id(&self, path: &str) -> Result<String> {
        Ok(self.mosh.get(self.root.join(path).to_str().unwrap())?.id)
    }
}

fn measure(mosh: &Mosh, cases: &[(&str, &[&str])]) -> Result<Value> {
    let mut correct = 0;
    let mut recall = 0.0;
    let mut reciprocal = 0.0;
    let mut total_ms = 0.0;
    for &(query, wanted) in cases {
        let start = Instant::now();
        let answer = mosh.ask(query, false)?;
        total_ms += start.elapsed().as_secs_f64() * 1000.0;
        let results = answer["results"].as_array().context("missing results")?;
        let matches = results
            .iter()
            .map(|r| r["object"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        if wanted.is_empty() {
            correct += usize::from(matches.is_empty());
            recall += u8::from(matches.is_empty()) as f64;
            reciprocal += u8::from(matches.is_empty()) as f64;
            continue;
        }
        let rank = matches
            .iter()
            .position(|name| wanted.contains(name))
            .map(|n| n + 1);
        correct += usize::from(rank == Some(1));
        reciprocal += rank.map_or(0.0, |rank| 1.0 / rank as f64);
        let found = matches
            .into_iter()
            .take(3)
            .filter(|name| wanted.contains(name))
            .collect::<BTreeSet<_>>();
        recall += found.len() as f64 / wanted.len() as f64;
    }
    let count = cases.len() as f64;
    Ok(
        json!({"queries":cases.len(),"top1":correct as f64/count,"recall_at_3":recall/count,"mrr":reciprocal/count,"mean_ms":total_ms/count}),
    )
}

#[test]
fn labeled_retrieval_and_vocabulary_quality() -> Result<()> {
    let f = Corpus::create()?;
    f.file(
        "billing-a.txt",
        "Component: Billing\nEnvironment: Production\nDeploy checklist for the east region.",
    )?;
    f.file(
        "billing-b.txt",
        "Component: Billing\nEnvironment: Production\nDeploy checklist for the west region.",
    )?;
    f.file(
        "billing-c.txt",
        "Component: Billing\nEnvironment: Staging\nDeploy checklist for the east region.",
    )?;
    f.file(
        "assignment.txt",
        "2030-03-19\nJohn: Matt, can you audit the inventory ledger?\nMatt: Sure.",
    )?;
    f.file(
        "reversed.txt",
        "2030-03-19\nMatt: John, can you audit the warehouse?\nJohn: Sure.",
    )?;
    f.file("copy-one.txt", "Identical release keycard diagram.")?;
    f.file("copy-two.txt", "Identical release keycard diagram.")?;
    f.file("restricted/secret.txt", "Confidential procurement details.")?;
    for n in 0..30 {
        f.file(&format!("unrelated-{n:02}.md"), "Reference documentation.")?;
    }
    f.scan()?;
    let billing = f.id("billing-a.txt")?;
    let assignment = f.id("assignment.txt")?;
    let reverse = f.id("reversed.txt")?;
    f.mosh.tag(&reverse, "assigner:john", true, 1.0)?;
    f.mosh.secret(&f.id("restricted")?, true)?;
    f.mosh.person("evaluator", None)?;
    let viewer = Mosh::as_actor(&f.db, "evaluator")?;
    let hidden_before = measure(&viewer, &[("name:secret.txt", &[])])?;
    assert_eq!(hidden_before["top1"], 1.0);
    f.mosh.duplicates(None, &AtomicBool::new(false))?;
    assert!(
        f.mosh.individuality(&f.id("copy-one.txt")?)?["verified_duplicates"]
            .as_u64()
            .unwrap()
            > 0
    );
    let collision = f.mosh.individuality(&billing)?;
    assert!(collision["distinct_collisions"].as_u64().unwrap() > 0);
    let learning = f.mosh.ai_settings()?;
    let mut measuring = learning.clone();
    measuring.learn = false;
    f.mosh.configure_ai(&measuring)?;
    let tuned: &[(&str, &[&str])] = &[(
        "What did John ask Matt to do on 2030-03-19?",
        &["assignment.txt"],
    )];
    let held_out: &[(&str, &[&str])] = &[
        ("name:copy-two.txt", &["copy-two.txt"]),
        (
            "Find the child with name:reversed.txt ext:txt",
            &["reversed.txt"],
        ),
        ("What did John assign Matt?", &["assignment.txt"]),
        (
            "direct-tag:environment:production",
            &["billing-a.txt", "billing-b.txt"],
        ),
        (
            "Find direct-tag:environment:production name:billing-b.txt",
            &["billing-b.txt"],
        ),
        (
            "Find direct-tag:environment:production name:billing-c.txt",
            &[],
        ),
    ];
    let tuned_before = measure(&f.mosh, tuned)?;
    let held_before = measure(&f.mosh, held_out)?;
    f.mosh.compact()?;
    let before = f.mosh.storage_usage()?;
    f.mosh.configure_ai(&learning)?;
    let miss = f.mosh.ask(tuned[0].0, false)?;
    f.mosh
        .retrieval_feedback(miss["id"].as_i64().unwrap(), &assignment, true, None)?;
    let jobs = f.mosh.run_agents(1, &AtomicBool::new(false))?;
    assert_eq!(jobs["completed"], 1);
    let queue = f.mosh.ai_queue(8)?;
    let trial = queue
        .iter()
        .find(|row| row["object"] == assignment)
        .context("missing evaluated case")?;
    assert_eq!(trial["result"]["applied"], true, "{trial}");
    let advice = f.mosh.vocabulary_advice(&billing)?;
    let proposal = advice["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["tag"] == "environment:production")
        .context("missing reusable environment proposal")?;
    assert_eq!(proposal["reusable"], true);
    assert!(
        f.mosh
            .apply_vocabulary_tag(&billing, "environment:production", false)
            .is_err()
    );
    assert_eq!(
        f.mosh
            .apply_vocabulary_tag(&billing, "environment:production", true)?["changed_files"],
        2
    );
    f.mosh.configure_ai(&measuring)?;
    let tuned_after = measure(&f.mosh, tuned)?;
    let held_after = measure(&f.mosh, held_out)?;
    f.mosh.compact()?;
    let after = f.mosh.storage_usage()?;
    assert_eq!(tuned_before["top1"], 0.0);
    assert_eq!(tuned_after["top1"], 1.0);
    assert_eq!(held_after["top1"], 1.0);
    assert!(held_before["top1"].as_f64().unwrap() < 1.0);
    assert_eq!(measure(&viewer, &[("name:secret.txt", &[])])?["top1"], 1.0);
    println!(
        "{}",
        json!({"tuned_before":tuned_before,"tuned_after":tuned_after,"held_out_before":held_before,"held_out_after":held_after,
        "files":after["files"],"ledger_bytes_before":before["database_bytes"],"ledger_bytes_after":after["database_bytes"],"model_tokens":0})
    );
    Ok(())
}

#[test]
fn feedback_rejects_unhelpful_proposals_without_storing_tags() -> Result<()> {
    let f = Corpus::create()?;
    f.file(
        "candidate.txt",
        "Subsystem: Ledger\nGenerated release candidate",
    )?;
    f.scan()?;
    let id = f.id("candidate.txt")?;
    let miss = f.mosh.ask("name:missing.txt", false)?;
    f.mosh
        .retrieval_feedback(miss["id"].as_i64().unwrap(), &id, true, None)?;
    f.mosh.run_agents(1, &AtomicBool::new(false))?;
    let queue = f.mosh.ai_queue(6)?;
    let rejected = queue
        .iter()
        .find(|job| job["object"] == id)
        .context("no trial")?;
    assert_eq!(rejected["result"]["applied"], false, "{queue:?}");
    assert!(
        f.mosh
            .tag_registry(Some("subsystem:ledger"), 10)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn feedback_rolls_back_a_primary_gain_that_hurts_a_confirmed_control() -> Result<()> {
    let f = Corpus::create()?;
    f.file("first.txt", "placeholder one")?;
    f.file("second.txt", "placeholder two")?;
    f.scan()?;
    let (new, old) = if f.id("first.txt")? < f.id("second.txt")? {
        ("first.txt", "second.txt")
    } else {
        ("second.txt", "first.txt")
    };
    f.file(
        new,
        "2030-03-19\nJohn: Matt, can you audit the warehouse?\nMatt: Yes.",
    )?;
    f.file(
        old,
        "2030-03-20\nJohn: Matt, can you audit the billing ledger?\nMatt: Yes.",
    )?;
    f.scan()?;
    let missing = f.id(new)?;
    let known = f.id(old)?;
    f.mosh.queue_index(&format!("name:{old}"), 1, false)?;
    f.mosh.run_agents(1, &AtomicBool::new(false))?;
    let control = f.mosh.ask("What did John ask Matt to do?", false)?;
    assert_eq!(control["results"][0]["object"]["id"], known);
    f.mosh
        .retrieval_feedback(control["id"].as_i64().unwrap(), &known, true, None)?;
    f.mosh.run_agents(1, &AtomicBool::new(false))?;
    let primary = f
        .mosh
        .ask("What did John ask Matt to do on 2030-03-19?", false)?;
    assert!(primary["results"].as_array().unwrap().is_empty());
    f.mosh
        .retrieval_feedback(primary["id"].as_i64().unwrap(), &missing, true, None)?;
    f.mosh.run_agents(1, &AtomicBool::new(false))?;
    let queue = f.mosh.ai_queue(10)?;
    let trial = queue
        .iter()
        .find(|row| row["object"] == missing)
        .context("missing evaluated change")?;
    assert_eq!(trial["result"]["new_rank"], 1, "{trial}");
    assert_eq!(trial["result"]["regressions"], 1, "{trial}");
    assert_eq!(trial["result"]["applied"], false, "{trial}");
    assert!(f.mosh.passages(&missing)?.is_empty());
    Ok(())
}
