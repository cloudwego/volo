use std::fs;

use git2::{Oid, Repository, Signature};
use tempfile::tempdir;
use volo_build::{ConfigBuilder, util::get_git_path};

fn commit_idl(repo: &Repository, record: &str) -> Result<Oid, git2::Error> {
    let idl = format!("namespace rs example\nstruct {record} {{ 1: required string value }}\n");
    let mut tree = repo.treebuilder(None)?;
    tree.insert("record.thrift", repo.blob(idl.as_bytes())?, 0o100644)?;
    let tree = repo.find_tree(tree.write()?)?;
    let author = Signature::now("Test", "test@example.com")?;
    let reference = format!("refs/heads/{record}");
    repo.commit(Some(&reference), &author, &author, record, &tree, &[])
}

#[test]
fn codegen_follows_git_lock_on_rollback() -> anyhow::Result<()> {
    let dir = tempdir()?;
    let config = dir.path().join("lust.yml");
    let out = dir.path().join("out");
    let upstream = out.join("idl/records.git");
    let url = upstream.to_str().unwrap();
    let repo = Repository::init_bare(&upstream)?;
    let a = commit_idl(&repo, "RecordA")?;
    let b = commit_idl(&repo, "RecordB")?;
    repo.set_head("refs/heads/RecordB")?;
    drop(repo);

    let check = |lock: Oid, expected: &str| -> anyhow::Result<()> {
        fs::write(
            &config,
            format!(
                "\
entries:
  records:
    filename: generated.rs
    protocol: thrift
    touch_all: true
    repos:
      records:
        url: {url}
        ref: HEAD
        lock: {lock}
    services:
      - idl:
          source: git
          repo: records
          path: record.thrift
"
            ),
        )?;
        ConfigBuilder::new(config.clone()).out_dir(&out).write()?;

        let generated = fs::read_to_string(out.join("generated.rs"))?;
        let records: Vec<_> = ["RecordA", "RecordB"]
            .into_iter()
            .filter(|name| generated.contains(name))
            .collect();
        assert_eq!(records, [expected], "lock {lock}");
        Ok(())
    };

    check(a, "RecordA")?;
    check(b, "RecordB")?;
    check(a, "RecordA")?;

    // An older cache can have a marker for B while A is checked out.
    let checkout = out.join("idl/records").join(get_git_path(url)?);
    fs::write(checkout.join(b.to_string()), [])?;
    fs::rename(&upstream, dir.path().join("offline.git"))?;
    check(b, "RecordB")
}
