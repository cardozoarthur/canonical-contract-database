use canonical_storage::Store;
use std::{fs, panic::catch_unwind};

fn fixture() -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"confirmed first").unwrap();
    store.append(b"confirmed second").unwrap();
    drop(store);
    let manifest = fs::read(dir.path().join("manifest.cbr")).unwrap();
    let segment = fs::read(dir.path().join("segments/00000001.cbr")).unwrap();
    (dir, manifest, segment)
}

#[test]
fn each_authoritative_byte_mutation_is_rejected_and_never_repaired() {
    let (dir, manifest, segment) = fixture();
    for (name, original) in [
        ("manifest.cbr", &manifest),
        ("segments/00000001.cbr", &segment),
    ] {
        let path = dir.path().join(name);
        for offset in 0..original.len() {
            let mut changed = original.clone();
            changed[offset] ^= 1;
            fs::write(&path, &changed).unwrap();
            assert!(
                catch_unwind(|| Store::open(dir.path())).unwrap().is_err(),
                "accepted {name}:{offset}"
            );
            assert!(
                catch_unwind(|| Store::recover(dir.path()))
                    .unwrap()
                    .is_err(),
                "repaired {name}:{offset}"
            );
            assert_eq!(
                fs::read(&path).unwrap(),
                changed,
                "modified {name}:{offset}"
            );
        }
        fs::write(path, original).unwrap();
    }
    assert_eq!(Store::open(dir.path()).unwrap().snapshot().len(), 2);
}

#[test]
fn every_manifest_cut_is_rejected_without_reinitialization() {
    let (dir, manifest, _) = fixture();
    let path = dir.path().join("manifest.cbr");
    for cut in 0..manifest.len() {
        fs::write(&path, &manifest[..cut]).unwrap();
        assert!(
            catch_unwind(|| Store::open(dir.path())).unwrap().is_err(),
            "manifest cut {cut}"
        );
        assert!(
            catch_unwind(|| Store::recover(dir.path()))
                .unwrap()
                .is_err(),
            "manifest recovery cut {cut}"
        );
        assert_eq!(fs::read(&path).unwrap(), manifest[..cut]);
    }
}

#[test]
fn corrupt_and_truncated_checkpoint_is_only_a_discardable_hint() {
    let (dir, _, segment) = fixture();
    let mut store = Store::open(dir.path()).unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let path = dir.path().join("checkpoint.cbr");
    let checkpoint = fs::read(&path).unwrap();
    let mut corpus: Vec<Vec<u8>> = (0..checkpoint.len())
        .map(|cut| checkpoint[..cut].to_vec())
        .collect();
    for offset in 0..checkpoint.len() {
        let mut changed = checkpoint.clone();
        changed[offset] ^= 1;
        corpus.push(changed);
    }
    for changed in corpus {
        fs::write(&path, changed).unwrap();
        let (store, report) = catch_unwind(|| Store::recover(dir.path()))
            .unwrap()
            .unwrap();
        assert_eq!(report.checkpoint_valid, Some(false));
        assert_eq!(store.snapshot().len(), 2);
        assert_eq!(store.snapshot()[1].payload, b"confirmed second");
        assert_eq!(
            fs::read(dir.path().join("segments/00000001.cbr")).unwrap(),
            segment
        );
    }
}

#[test]
fn corrupt_new_generation_never_falls_back_to_an_older_valid_generation() {
    let (dir, _, _) = fixture();
    let mut store = Store::open(dir.path()).unwrap();
    store
        .rewrite(&[b"first rewritten".to_vec(), b"second rewritten".to_vec()])
        .unwrap();
    let active = store.active_segment_path();
    assert_ne!(active, dir.path().join("segments/00000001.cbr"));
    drop(store);
    let manifest_path = dir.path().join("manifest.cbr");
    let manifest = fs::read(&manifest_path).unwrap();
    let segment = fs::read(&active).unwrap();
    for (path, original) in [(&manifest_path, &manifest), (&active, &segment)] {
        for offset in 0..original.len() {
            let mut changed = original.clone();
            changed[offset] ^= 1;
            fs::write(path, &changed).unwrap();
            assert!(
                catch_unwind(|| Store::open(dir.path())).unwrap().is_err(),
                "accepted generation mutation {offset}"
            );
            assert!(
                catch_unwind(|| Store::recover(dir.path()))
                    .unwrap()
                    .is_err(),
                "generation fallback at {offset}"
            );
            assert_eq!(fs::read(path).unwrap(), changed);
        }
        fs::write(path, original).unwrap();
    }
    for cut in 0..manifest.len() {
        fs::write(&manifest_path, &manifest[..cut]).unwrap();
        assert!(
            catch_unwind(|| Store::open(dir.path())).unwrap().is_err(),
            "accepted generation manifest cut {cut}"
        );
    }
    fs::write(manifest_path, manifest).unwrap();
    assert_eq!(
        Store::open(dir.path()).unwrap().snapshot()[1].payload,
        b"second rewritten"
    );
}
