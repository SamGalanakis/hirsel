mod compress_merge;
use std::cell::Cell;
use std::cmp::Ordering;
use std::rc::Rc;

struct Item {
    key: usize,
    id: usize,
    drops: Rc<Cell<usize>>,
}
impl Drop for Item {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}
struct Comparison {
    calls: Cell<usize>,
    stop: usize,
}
fn compare(env: &Comparison, a: &Box<Item>, b: &Box<Item>) -> Ordering {
    let n = env.calls.get();
    env.calls.set(n + 1);
    assert_ne!(n, env.stop, "comparison failure");
    a.key.cmp(&b.key)
}
fn items(n: usize, drops: &Rc<Cell<usize>>) -> Vec<Box<Item>> {
    let mut result = Vec::with_capacity(n + 7);
    for id in 0..n {
        result.push(Box::new(Item {
            key: (n - id) % 5,
            id,
            drops: drops.clone(),
        }));
    }
    result
}
#[test]
fn plain_stable_odd_and_even_passes() {
    for n in 0..130 {
        let ranks: Vec<_> = (0..n).map(|i| ((n - i) % 7, 0)).collect();
        let mut xs: Vec<_> = (0..n).collect();
        let mut expected = xs.clone();
        expected.sort_by_key(|&i| ranks[i]);
        compress_merge::indices(&mut xs, &ranks);
        assert_eq!(xs, expected, "length {n}");
    }
}
#[test]
fn boxed_stable_reuses_original_buffer_and_capacity() {
    for n in 0..66 {
        let drops = Rc::new(Cell::new(0));
        let mut xs = items(n, &drops);
        let old_ptr = xs.as_ptr();
        let old_cap = xs.capacity();
        let mut expected: Vec<_> = xs.iter().map(|x| (x.key, x.id)).collect();
        expected.sort_by_key(|x| x.0);
        compress_merge::boxes(
            &mut xs,
            &Comparison {
                calls: Cell::new(0),
                stop: usize::MAX,
            },
            compare,
        );
        assert_eq!(xs.as_ptr(), old_ptr);
        assert_eq!(xs.capacity(), old_cap);
        assert_eq!(
            xs.iter().map(|x| (x.key, x.id)).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(drops.get(), 0);
        drop(xs);
        assert_eq!(drops.get(), n);
    }
}
#[test]
fn every_comparison_failure_restores_owned_permutation() {
    let drops = Rc::new(Cell::new(0));
    let mut xs = items(17, &drops);
    let count = Comparison {
        calls: Cell::new(0),
        stop: usize::MAX,
    };
    compress_merge::boxes(&mut xs, &count, compare);
    let comparisons = count.calls.get();
    drop(xs);
    for stop in 0..comparisons {
        let drops = Rc::new(Cell::new(0));
        let mut xs = items(17, &drops);
        let old_ptr = xs.as_ptr();
        let old_cap = xs.capacity();
        let env = Comparison {
            calls: Cell::new(0),
            stop,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compress_merge::boxes(&mut xs, &env, compare);
        }));
        assert!(result.is_err());
        assert_eq!(xs.as_ptr(), old_ptr);
        assert_eq!(xs.capacity(), old_cap);
        assert_eq!(drops.get(), 0, "failure {stop}");
        let mut ids: Vec<_> = xs.iter().map(|x| x.id).collect();
        ids.sort();
        assert_eq!(ids, (0..17).collect::<Vec<_>>(), "failure {stop}");
        drop(xs);
        assert_eq!(drops.get(), 17);
    }
}
