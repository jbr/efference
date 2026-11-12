use super::*;

#[test]
fn a_root_hands_out_one_lock() {
    let locks = Locks::default();
    let root = Path::new("/p/.efference/worktrees/quiet-fox");
    assert!(Arc::ptr_eq(&locks.for_root(root), &locks.for_root(root)));
    assert!(!Arc::ptr_eq(
        &locks.for_root(root),
        &locks.for_root(Path::new("/p/.efference/worktrees/tidy-moss"))
    ));
}
