use weave_crdt::{
    get_entity_content, get_entity_status, resolve_entity_conflict, set_entity_conflict,
    update_entity_content, upsert_entity, EntityStateDoc, VersionVector,
};

fn setup() -> EntityStateDoc {
    EntityStateDoc::new_memory().unwrap()
}

fn setup_with_entity(entity_id: &str, name: &str) -> EntityStateDoc {
    let mut state = setup();
    upsert_entity(
        &mut state,
        entity_id,
        name,
        "function",
        "src/lib.rs",
        "hash0",
    )
    .unwrap();
    state
}

// ── Content operations ──

#[test]
fn test_store_and_read_content() {
    let mut state = setup_with_entity("eid1", "my_func");
    update_entity_content(&mut state, "agent-1", "eid1", "fn my_func() { 42 }", "h1").unwrap();

    let status = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(status.content, "fn my_func() { 42 }");
    assert_eq!(status.content_hash, "h1");
    assert_eq!(status.merge_state, "clean");
}

#[test]
fn test_vv_increment_on_content_update() {
    let mut state = setup_with_entity("eid1", "my_func");
    update_entity_content(&mut state, "agent-1", "eid1", "v1", "h1").unwrap();
    update_entity_content(&mut state, "agent-1", "eid1", "v2", "h2").unwrap();

    let status = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(status.version_vector.get("agent-1"), 2);
    assert_eq!(status.version_vector.total(), 2);
}

#[test]
fn test_base_content_tracking() {
    let mut state = setup_with_entity("eid1", "my_func");

    // First update sets content, base_content is empty initially
    update_entity_content(&mut state, "agent-1", "eid1", "version_1", "h1").unwrap();
    let s1 = get_entity_content(&state, "eid1").unwrap();
    // base_content should be empty since the entity had empty content before
    assert_eq!(s1.base_content, "");

    // Second update: base_content should now be set to "version_1"
    update_entity_content(&mut state, "agent-1", "eid1", "version_2", "h2").unwrap();
    let s2 = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(s2.content, "version_2");
    assert_eq!(s2.base_content, "version_1");
}

#[test]
fn test_multiple_agents_content_update() {
    let mut state = setup_with_entity("eid1", "my_func");

    update_entity_content(&mut state, "agent-1", "eid1", "a1_content", "h1").unwrap();
    update_entity_content(&mut state, "agent-2", "eid1", "a2_content", "h2").unwrap();

    let status = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(status.version_vector.get("agent-1"), 1);
    assert_eq!(status.version_vector.get("agent-2"), 1);
    assert_eq!(status.version_vector.total(), 2);
    // Last writer wins in Automerge
    assert_eq!(status.content, "a2_content");
}

#[test]
fn test_content_update_clears_conflict_state() {
    let mut state = setup_with_entity("eid1", "my_func");

    // Set up content first so entity has some state
    update_entity_content(&mut state, "agent-1", "eid1", "orig", "h0").unwrap();

    // Manually set conflict state via the internal API
    set_entity_conflict(
        &mut state,
        "eid1",
        "ours_content",
        "theirs_content",
        "base_content",
        "agent-1",
        "agent-2",
    )
    .unwrap();

    let status = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(status.merge_state, "conflict");

    // Writing new content should clear the conflict
    update_entity_content(&mut state, "agent-1", "eid1", "resolved", "h_resolved").unwrap();
    let status2 = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(status2.merge_state, "clean");
    assert_eq!(status2.content, "resolved");
}

// ── Conflict resolution ──

#[test]
fn test_resolve_entity_conflict() {
    let mut state = setup_with_entity("eid1", "my_func");
    update_entity_content(&mut state, "agent-1", "eid1", "orig", "h0").unwrap();

    // Set conflict
    set_entity_conflict(
        &mut state,
        "eid1",
        "ours_content",
        "theirs_content",
        "base_content",
        "agent-1",
        "agent-2",
    )
    .unwrap();

    // Resolve
    resolve_entity_conflict(&mut state, "agent-3", "eid1", "merged_content", "hm").unwrap();

    let status = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(status.merge_state, "clean");
    assert_eq!(status.content, "merged_content");
    assert!(status.conflict_ours.is_none());
    assert!(status.conflict_theirs.is_none());
    assert_eq!(status.version_vector.get("agent-3"), 1);
}

#[test]
fn test_resolve_non_conflict_fails() {
    let mut state = setup_with_entity("eid1", "my_func");
    update_entity_content(&mut state, "agent-1", "eid1", "content", "h1").unwrap();

    let result = resolve_entity_conflict(&mut state, "agent-1", "eid1", "new", "h2");
    assert!(result.is_err());
}

// ── Version vector on a content update ──
//
// Was `record_modification`, deleted as a weaker duplicate of
// `update_entity_content` (no `writes` register entry). Repointed at the live
// door; the vector-clock expectations are unchanged.

#[test]
fn test_update_entity_content_uses_vv() {
    let mut state = setup_with_entity("eid1", "my_func");

    update_entity_content(&mut state, "agent-1", "eid1", "fn a() {}", "h1").unwrap();
    update_entity_content(&mut state, "agent-2", "eid1", "fn b() {}", "h2").unwrap();
    update_entity_content(&mut state, "agent-1", "eid1", "fn c() {}", "h3").unwrap();

    let status = get_entity_status(&state, "eid1").unwrap();
    assert_eq!(status.version_vector.get("agent-1"), 2);
    assert_eq!(status.version_vector.get("agent-2"), 1);
    assert_eq!(status.version, 3); // total
}

// ── Migration tests ──

#[test]
fn test_new_doc_has_schema_v2() {
    let mut state = setup();
    // New docs should have v2 structures initialized
    // Verify indirectly by syncing (which uses file_entity_order and file_interstitials)
    // If they don't exist, upsert would fail
    upsert_entity(&mut state, "eid1", "func", "function", "test.rs", "h1").unwrap();
    let status = get_entity_status(&state, "eid1").unwrap();
    assert_eq!(status.merge_state, "clean");
}

#[test]
fn test_upsert_creates_v2_fields() {
    let mut state = setup();
    upsert_entity(&mut state, "eid1", "func", "function", "lib.rs", "h1").unwrap();

    let status = get_entity_status(&state, "eid1").unwrap();
    assert_eq!(status.version, 0);
    assert_eq!(status.merge_state, "clean");
    assert!(status.version_vector.is_empty());

    let content_status = get_entity_content(&state, "eid1").unwrap();
    assert_eq!(content_status.content, "");
    assert_eq!(content_status.base_content, "");
}

#[test]
fn test_migration_preserves_existing_data() {
    let mut state = setup_with_entity("eid1", "my_func");
    update_entity_content(
        &mut state,
        "agent-1",
        "eid1",
        "fn my_func() {}",
        "modified_hash",
    )
    .unwrap();

    let status = get_entity_status(&state, "eid1").unwrap();
    assert_eq!(status.name, "my_func");
    assert_eq!(status.content_hash, "modified_hash");
    assert!(status.version >= 1);
}

#[test]
fn test_migration_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");

    // Create and populate a doc via open (saves to disk)
    {
        let mut state = EntityStateDoc::open(&path).unwrap();
        upsert_entity(
            &mut state,
            "eid1",
            "my_func",
            "function",
            "src/lib.rs",
            "h0",
        )
        .unwrap();
        update_entity_content(&mut state, "agent-1", "eid1", "content_v1", "h1").unwrap();
        state.save().unwrap();
    }

    // Reopen (triggers migrate_if_needed)
    let state2 = EntityStateDoc::open(&path).unwrap();
    let status = get_entity_content(&state2, "eid1").unwrap();
    assert_eq!(status.content, "content_v1");
    assert_eq!(status.version_vector.get("agent-1"), 1);
}

// ── Version vector concurrent detection ──

#[test]
fn test_concurrent_version_vectors_detected() {
    let mut vv_a = VersionVector::new();
    vv_a.increment("agent-1");
    vv_a.increment("agent-1");

    let mut vv_b = VersionVector::new();
    vv_b.increment("agent-2");

    // Neither dominates: concurrent
    assert!(vv_a.partial_cmp(&vv_b).is_none());

    // After merge, both are dominated
    let mut merged = vv_a.clone();
    merged.merge(&vv_b);
    assert_eq!(merged.get("agent-1"), 2);
    assert_eq!(merged.get("agent-2"), 1);
    assert_eq!(merged.total(), 3);
}

// ── Entity status includes new fields ──

#[test]
fn test_entity_status_includes_vv_and_merge_state() {
    let mut state = setup_with_entity("eid1", "my_func");
    update_entity_content(&mut state, "agent-1", "eid1", "content", "h1").unwrap();

    let status = get_entity_status(&state, "eid1").unwrap();
    assert_eq!(status.version_vector.get("agent-1"), 1);
    assert_eq!(status.merge_state, "clean");
    assert_eq!(status.version, 1);
}

#[test]
fn saving_a_legacy_snapshot_persists_its_migration() {
    use automerge::{transaction::Transactable, AutoCommit, ObjType, ROOT};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");
    let mut legacy = AutoCommit::new();
    legacy.put(ROOT, "schema_version", 3).unwrap();
    legacy.put_object(ROOT, "entities", ObjType::Map).unwrap();
    let before = legacy.save();
    std::fs::write(&path, &before).unwrap();

    let mut state = EntityStateDoc::open_for_update(&path).unwrap();
    state.save().unwrap();
    assert_ne!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn saving_unchanged_state_preserves_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");
    let mut state = EntityStateDoc::open_for_update(&path).unwrap();
    state.save().unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(std::time::UNIX_EPOCH).unwrap();
    drop(file);
    let before = path.metadata().unwrap().modified().unwrap();
    state.save().unwrap();
    assert_eq!(path.metadata().unwrap().modified().unwrap(), before);
}

#[test]
fn stale_snapshots_cannot_overwrite_a_saved_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");
    let mut writer = EntityStateDoc::open(&path).unwrap();
    for id in ["first", "second"] {
        let mut stale = EntityStateDoc::open(&path).unwrap();
        upsert_entity(&mut writer, id, id, "function", "a.rs", "hash").unwrap();
        writer.save().unwrap();
        assert!(matches!(
            stale.save(),
            Err(weave_crdt::WeaveError::StateChanged(_))
        ));
        assert!(get_entity_status(&EntityStateDoc::open(&path).unwrap(), id).is_ok());
    }
}

#[test]
fn simultaneous_writers_see_each_others_committed_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");
    let start = std::sync::Barrier::new(4);
    std::thread::scope(|threads| {
        for id in ["a", "b", "c", "d"] {
            let (path, start) = (&path, &start);
            threads.spawn(move || {
                start.wait();
                let mut state = EntityStateDoc::open_for_update(path).unwrap();
                upsert_entity(&mut state, id, id, "function", "a.rs", "hash").unwrap();
                state.save().unwrap();
            });
        }
    });
    let state = EntityStateDoc::open(&path).unwrap();
    for id in ["a", "b", "c", "d"] {
        assert!(get_entity_status(&state, id).is_ok(), "{id}");
    }
}

#[cfg(unix)]
#[test]
fn saving_through_a_symlink_updates_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");
    let alias = dir.path().join("alias.automerge");
    std::os::unix::fs::symlink("state.automerge", &alias).unwrap();
    let mut stale = EntityStateDoc::open(&path).unwrap();
    for (id, open) in [
        (
            "locked",
            EntityStateDoc::open_for_update as fn(&std::path::Path) -> _,
        ),
        ("snapshot", EntityStateDoc::open),
    ] {
        let mut state = open(&alias).unwrap();
        upsert_entity(&mut state, id, id, "function", "a.rs", "hash").unwrap();
        state.save().unwrap();
        assert!(alias.is_symlink());
        assert!(get_entity_status(&EntityStateDoc::open(&path).unwrap(), id).is_ok());
    }
    assert!(path.with_added_extension("lock").exists());
    assert!(!alias.with_added_extension("lock").exists());
    assert!(matches!(
        stale.save(),
        Err(weave_crdt::WeaveError::StateChanged(_))
    ));
}

#[test]
fn writes_respect_state_file_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");
    let mut state = EntityStateDoc::open_for_update(&path).unwrap();
    state.save().unwrap();
    let mut permissions = path.metadata().unwrap().permissions();
    let original_permissions = permissions.clone();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    upsert_entity(&mut state, "new", "new", "function", "a.rs", "hash").unwrap();
    let write = state.save();
    std::fs::set_permissions(&path, original_permissions).unwrap();
    assert!(
        matches!(write, Err(weave_crdt::WeaveError::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert!(get_entity_status(&EntityStateDoc::open(&path).unwrap(), "new").is_err());
}

#[cfg(any(unix, windows))]
#[test]
fn writes_do_not_split_hard_linked_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.automerge");
    let alias = dir.path().join("alias.automerge");
    let mut state = EntityStateDoc::open_for_update(&path).unwrap();
    state.save().unwrap();
    std::fs::hard_link(&path, &alias).unwrap();
    upsert_entity(&mut state, "new", "new", "function", "a.rs", "hash").unwrap();
    assert!(state.save().is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        std::fs::read(&alias).unwrap()
    );
}
