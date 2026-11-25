use super::*;

fn slots(n: usize) -> Vec<Arc<dyn Slot>> {
    (0..n)
        .map(|_| Arc::new(MemSlot::default()) as Arc<dyn Slot>)
        .collect()
}

fn vfs(slots: &[Arc<dyn Slot>]) -> PoolVfs {
    PoolVfs::new(slots.to_vec()).unwrap()
}

#[test]
fn files_are_created_read_written_and_resized() {
    let pool = vfs(&slots(4));
    let dir = Path::new("/db");
    assert_eq!(
        pool.open(&dir.join("a"), OpenMode::CreateNew)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound,
        "no directory yet"
    );
    pool.create_dir_all(dir).unwrap();
    let file = pool.open(&dir.join("a"), OpenMode::CreateNew).unwrap();
    file.write_all_at(b"hello world", 0).unwrap();
    file.write_all_at(b"W", 6).unwrap();
    let mut buf = [0u8; 11];
    file.read_exact_at(&mut buf, 0).unwrap();
    assert_eq!(&buf, b"hello World");
    assert_eq!(file.len().unwrap(), 11);
    file.set_len(5).unwrap();
    assert_eq!(file.len().unwrap(), 5);
    assert_eq!(file.read_at(&mut buf, 5).unwrap(), 0, "end of file");
    assert_eq!(
        pool.open(&dir.join("a"), OpenMode::CreateNew)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        pool.open(&dir.join("b"), OpenMode::Read)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(pool.list_dir(dir).unwrap(), ["a"]);
    assert!(pool.exists(dir).unwrap() && pool.exists(&dir.join("a")).unwrap());
}

#[test]
fn a_removed_file_keeps_its_slot_while_it_is_open() {
    let pool = vfs(&slots(2));
    pool.create_dir_all(Path::new("/d")).unwrap();
    let old = pool.open(Path::new("/d/old"), OpenMode::CreateNew).unwrap();
    old.write_all_at(b"kept", 0).unwrap();
    pool.remove_file(Path::new("/d/old")).unwrap();
    assert!(!pool.exists(Path::new("/d/old")).unwrap());
    let other = pool.open(Path::new("/d/x"), OpenMode::CreateNew).unwrap();
    other.write_all_at(b"other", 0).unwrap();
    assert_eq!(
        pool.open(Path::new("/d/y"), OpenMode::CreateNew)
            .unwrap_err()
            .kind(),
        io::ErrorKind::StorageFull
    );
    let mut buf = [0u8; 4];
    old.read_exact_at(&mut buf, 0).unwrap();
    assert_eq!(&buf, b"kept", "the removed file's data is intact");
    drop(old);
    assert_eq!(pool.free_slots(), 1, "its slot is free once closed");
    let reused = pool.open(Path::new("/d/y"), OpenMode::CreateNew).unwrap();
    assert_eq!(reused.len().unwrap(), 0, "a reused slot starts empty");
}

#[test]
fn renames_replace_and_the_pool_reopens_from_its_headers() {
    let storage = slots(4);
    let pool = vfs(&storage);
    pool.create_dir_all(Path::new("/db")).unwrap();
    for (name, data) in [("CURRENT", "old"), ("tmp", "new"), ("000001.sst", "run")] {
        pool.open(&Path::new("/db").join(name), OpenMode::CreateNew)
            .unwrap()
            .write_all_at(data.as_bytes(), 0)
            .unwrap();
    }
    pool.rename(Path::new("/db/tmp"), Path::new("/db/CURRENT"))
        .unwrap();
    assert_eq!(pool.free_slots(), 2, "the replaced file's slot is free");
    drop(pool);

    let reopened = vfs(&storage);
    let mut names = reopened.list_dir(Path::new("/db")).unwrap();
    names.sort();
    assert_eq!(names, ["000001.sst", "CURRENT"]);
    let current = reopened
        .open(Path::new("/db/CURRENT"), OpenMode::Read)
        .unwrap();
    let mut buf = vec![0u8; usize::try_from(current.len().unwrap()).unwrap()];
    current.read_exact_at(&mut buf, 0).unwrap();
    assert_eq!(buf, b"new");
    assert_eq!(reopened.free_slots(), 2);
}

#[test]
fn of_two_claims_to_a_path_the_newer_wins() {
    let storage = slots(2);
    write_header(storage[0].as_ref(), 5, Some(Path::new("/db/CURRENT"))).unwrap();
    write_all(storage[0].as_ref(), b"older", HEADER_BYTES).unwrap();
    write_header(storage[1].as_ref(), 9, Some(Path::new("/db/CURRENT"))).unwrap();
    write_all(storage[1].as_ref(), b"newer", HEADER_BYTES).unwrap();
    for order in [storage.clone(), storage.iter().rev().cloned().collect()] {
        let pool = PoolVfs::new(order).unwrap();
        let file = pool.open(Path::new("/db/CURRENT"), OpenMode::Read).unwrap();
        let mut buf = [0u8; 5];
        file.read_exact_at(&mut buf, 0).unwrap();
        assert_eq!(&buf, b"newer");
        assert_eq!(pool.free_slots(), 1);
    }
}

#[test]
fn locks_are_exclusive_and_the_pool_grows() {
    let pool = vfs(&slots(1));
    let guard = pool.lock(Path::new("/db/LOCK")).unwrap();
    assert_eq!(
        pool.lock(Path::new("/db/LOCK")).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(guard);
    pool.lock(Path::new("/db/LOCK")).unwrap();
    pool.add_slots(slots(3)).unwrap();
    assert_eq!((pool.capacity(), pool.free_slots()), (4, 4));
    assert!(write_header(
        &MemSlot::default(),
        1,
        Some(&PathBuf::from("x".repeat(5000)))
    )
    .is_err());
}
