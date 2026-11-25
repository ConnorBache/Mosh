use anyhow::Result;
use mosh_core::Mosh;
use std::{fs, sync::atomic::AtomicBool};

#[test]
fn nested_moshes_keep_boundaries_and_authorize_each_handoff() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("parent");
    let child_root = root.join("child");
    let grand_root = child_root.join("grandchild");
    fs::create_dir_all(&grand_root)?;
    fs::write(root.join("parent.txt"), "parent-source")?;
    let child_file = child_root.join("child.txt");
    fs::write(&child_file, "child-only-topic")?;
    fs::write(grand_root.join("grand.txt"), "grandchild-secret-topic")?;
    let parent_path = fixture.path().join("parent.sqlite3");
    let child_path = fixture.path().join("child.sqlite3");
    let grand_path = fixture.path().join("grand.sqlite3");
    let parent = Mosh::open(&parent_path)?;
    let child = Mosh::open(&child_path)?;
    let grand = Mosh::open(&grand_path)?;
    let cancel = AtomicBool::new(false);
    parent.scan(&root, &cancel)?;
    assert_eq!(parent.doctor()?["directory_documents"], 2);
    child.scan(&child_root, &cancel)?;
    grand.scan(&grand_root, &cancel)?;
    let remembered = parent.get(child_file.to_str().unwrap())?.id;
    parent.tag(&remembered, "remembered", true, 1.0)?;
    let p = parent.mosh_id()?;
    let c = child.mosh_id()?;
    let g = grand.mosh_id()?;
    child.register_child(
        &grand_path,
        &grand_root,
        "Observatory",
        "",
        &["astronomy".into()],
    )?;
    parent.register_child(
        &child_path,
        &child_root,
        "Operations",
        "Routing notes",
        &["maintenance".into()],
    )?;
    assert_eq!(parent.doctor()?["directory_documents"], 0);
    assert!(
        parent
            .register_child(&grand_path, &grand_root, "Overlap", "", &[])
            .is_err()
    );
    assert!(
        grand
            .register_child(&parent_path, &root, "Cycle", "", &[])
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
    assert!(parent.scan(&child_root, &cancel).is_err());
    assert!(parent.refresh_path(&child_file, &cancel).is_err());
    let report = parent.scan(&root, &cancel)?;
    assert_eq!(report.scanned, 2);
    assert!(
        parent
            .search("kind:file", &Default::default())?
            .iter()
            .all(|o| o.name == "parent.txt")
    );
    assert!(parent.preview(child_file.to_str().unwrap()).is_err());
    assert!(parent.child_handoff(&c, "kind:file", false).is_err());
    for mosh in [&parent, &child, &grand] {
        mosh.person("reader", None)?;
    }
    let reader = Mosh::as_actor(&parent_path, "reader")?;
    assert!(reader.child_pointers("maintenance", None)?.is_empty());
    assert!(
        reader.ask("maintenance", false)?["child_suggestions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    parent.child_discovery(&c, "reader", true)?;
    let visible = reader.child_pointers("maintenance", None)?;
    assert_eq!(visible.len(), 1);
    assert!(visible[0].get("root").is_none() && visible[0].get("attachment").is_none());
    assert!(reader.child_handoff(&c, "kind:file", false).is_err());
    child.grant_handoff(&p, "reader", true)?;
    let results = reader.child_handoff(&c, "kind:file", false)?;
    assert_eq!(results["results"].as_array().unwrap().len(), 1);
    assert_eq!(results["results"][0]["object"]["name"], "child.txt");
    assert!(
        results["results"][0]["qualified_id"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{c}:"))
    );
    assert!(reader.child_handoff(&g, "kind:file", false).is_err());
    grand.grant_handoff(&c, "local", true)?;
    assert_eq!(
        child.child_handoff(&g, "kind:file", false)?["results"][0]["object"]["name"],
        "grand.txt"
    );
    drop(child);
    let moved = fixture.path().join("moved.sqlite3");
    fs::rename(&child_path, &moved)?;
    assert!(reader.child_handoff(&c, "kind:file", false).is_err());
    assert!(parent.scan(&child_root, &cancel).is_err());
    parent.register_child(
        &moved,
        &child_root,
        "Operations",
        "",
        &["maintenance".into()],
    )?;
    assert_eq!(
        reader.child_handoff(&c, "kind:file", false)?["results"][0]["object"]["name"],
        "child.txt"
    );
    let moved_dir = root.join("relocated");
    fs::rename(&child_root, &moved_dir)?;
    assert!(parent.scan(&moved_dir, &cancel).is_err());
    assert!(parent.scan(&root, &cancel)?.scanned == 2);
    assert!(
        parent
            .get(moved_dir.join("child.txt").to_str().unwrap())
            .is_err()
    );
    fs::rename(&moved_dir, &child_root)?;
    parent.detach_child(&c)?;
    parent.scan(&root, &cancel)?;
    assert_eq!(parent.get(child_file.to_str().unwrap())?.id, remembered);
    assert!(
        parent
            .tags(&remembered)?
            .iter()
            .any(|tag| tag["name"] == "remembered")
    );
    Ok(())
}
