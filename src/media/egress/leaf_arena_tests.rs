use super::*;
use proptest::prelude::*;

#[test]
fn a_reused_slot_does_not_answer_to_the_old_key() {
    let mut arena = LeafArena::with_capacity(1);
    let old = arena.insert_with(|_| "old").unwrap();
    assert_eq!(arena.remove(old), Some("old"));
    let new = arena.insert_with(|_| "new").unwrap();

    assert_eq!(arena.get(new), Some(&"new"));
    assert_eq!(arena.get(old), None);
    assert_eq!(arena.get_mut(old), None);
    assert_eq!(
        arena.remove(old),
        None,
        "a stale remove cannot take the new leaf"
    );
    assert_eq!(arena.get(new), Some(&"new"));
}

#[test]
fn drained_keys_stop_resolving_and_free_their_slots() {
    let mut arena = LeafArena::with_capacity(2);
    let first = arena.insert_with(|_| 1).unwrap();
    let second = arena.insert_with(|_| 2).unwrap();
    assert!(arena.is_full());

    let mut drained: Vec<_> = arena.drain().collect();
    drained.sort_unstable();
    assert_eq!(drained, vec![1, 2]);
    assert!(arena.get(first).is_none() && arena.get(second).is_none());
    assert!(arena.insert_with(|_| 3).is_some() && arena.insert_with(|_| 4).is_some());
    assert!(arena.insert_with(|_| 5).is_none());
}

#[test]
fn a_reserved_slot_is_freed_once_and_filled_only_while_current() {
    let mut arena = LeafArena::with_capacity(1);
    let reserved = arena.reserve().unwrap();
    assert!(arena.is_full());
    assert_eq!(arena.get(reserved), None, "nothing to resolve before fill");
    assert_eq!(arena.remove(reserved), None);
    assert!(!arena.is_full(), "a failed connect frees its slot");
    assert_eq!(arena.remove(reserved), None);
    let again = arena.reserve().unwrap();
    assert!(arena.reserve().is_none(), "freed once, not twice");
    assert_eq!(
        arena.fill(reserved, 7),
        Err(7),
        "a stale reservation cannot fill"
    );
    assert_eq!(arena.fill(again, 8), Ok(()));
    assert_eq!(
        arena.fill(again, 9),
        Err(9),
        "a filled slot is not overwritten"
    );
    assert_eq!(arena.get(again), Some(&8));
}

#[derive(Debug, Clone)]
enum Op {
    Insert,
    /// Reserve a slot, then fill it at once (`true`) or free it unfilled.
    Reserve(bool),
    /// Remove the n-th live key (mod live count).
    Remove(usize),
    /// Use the n-th key ever removed (mod count), which must stay dead.
    UseStale(usize),
}

proptest! {
    /// Through any sequence, live keys resolve to their own value, every
    /// removed key resolves to nothing (get, get_mut, remove), and the
    /// arena never holds more than its capacity.
    #[test]
    fn removed_keys_never_resolve(
        capacity in 1usize..6,
        ops in prop::collection::vec(
            prop_oneof![
                Just(Op::Insert),
                any::<bool>().prop_map(Op::Reserve),
                (0usize..8).prop_map(Op::Remove),
                (0usize..8).prop_map(Op::UseStale),
            ],
            0..200,
        ),
    ) {
        let mut arena = LeafArena::with_capacity(capacity);
        let mut live: Vec<(LeafKey, u32)> = Vec::new();
        let mut dead: Vec<LeafKey> = Vec::new();
        let mut next = 0u32;
        for op in ops {
            match op {
                Op::Insert => {
                    let inserted = arena.insert_with(|_| next);
                    prop_assert_eq!(inserted.is_some(), live.len() < capacity);
                    if let Some(key) = inserted {
                        live.push((key, next));
                    }
                    next += 1;
                }
                Op::Reserve(fill) => {
                    let reserved = arena.reserve();
                    prop_assert_eq!(reserved.is_some(), live.len() < capacity);
                    if let Some(key) = reserved {
                        prop_assert!(arena.get(key).is_none());
                        if fill {
                            prop_assert_eq!(arena.fill(key, next), Ok(()));
                            live.push((key, next));
                        } else {
                            prop_assert_eq!(arena.remove(key), None);
                            dead.push(key);
                        }
                    }
                    next += 1;
                }
                Op::Remove(index) if !live.is_empty() => {
                    let (key, value) = live.remove(index % live.len());
                    prop_assert_eq!(arena.remove(key), Some(value));
                    dead.push(key);
                }
                Op::UseStale(index) if !dead.is_empty() => {
                    let key = dead[index % dead.len()];
                    prop_assert!(arena.get(key).is_none());
                    prop_assert!(arena.get_mut(key).is_none());
                    prop_assert!(arena.remove(key).is_none());
                    prop_assert!(arena.fill(key, next).is_err());
                }
                _ => {}
            }
            prop_assert_eq!(arena.len(), live.len());
            for (key, value) in &live {
                prop_assert_eq!(arena.get(*key), Some(value));
            }
            for key in &dead {
                prop_assert!(arena.get(*key).is_none());
            }
            prop_assert_eq!(arena.iter().count(), live.len());
        }
    }
}

#[test]
fn drain_also_frees_reserved_slots() {
    let mut arena: LeafArena<u8> = LeafArena::with_capacity(2);
    let reserved = arena.reserve().unwrap();
    let filled = arena.insert_with(|_| 1).unwrap();
    assert_eq!(arena.drain().collect::<Vec<_>>(), vec![1]);
    assert_eq!(
        arena.fill(reserved, 2),
        Err(2),
        "a pre-drain reservation is stale"
    );
    assert!(arena.get(filled).is_none());
    assert!(arena.reserve().is_some() && arena.reserve().is_some());
}
