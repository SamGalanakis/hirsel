// Reviewed-by: langs-text-921 2026-11-13; Quick/Merge and source introsort Self match pivots, cutoffs, heap fallback, scratch arrays, retained LCG resets; compile with overflow checks.
// Conversion parity delta reviewed by langs-text-921 2026-11-13: truncate then half-open float bounds, unsigned Fannkuch/matching checked index sites, u32 Mandelbrot counter; approval scoped to this delta.
// Sort::Self scoped comparator carrier delta pending independent review.
// Matches sort.hirsel, including std.list introsort for Sort::Self.
#[allow(dead_code)]
mod support;
fn quick(a: &mut [i32], lo: usize, hi: usize) {
    if hi - lo <= 1 {
        return;
    }
    let pivot = a[lo + (hi - lo) / 2];
    let mut i = lo;
    let mut j = hi - 1;
    while i <= j {
        while a[i] < pivot {
            i += 1;
        }
        while a[j] > pivot {
            j -= 1;
        }
        if i <= j {
            a.swap(i, j);
            i += 1;
            if j > lo {
                j -= 1;
            } else {
                break;
            }
        }
    }
    if j > lo {
        quick(a, lo, j + 1);
    }
    if i < hi {
        quick(a, i, hi);
    }
}
fn merge(a: &mut [i32], temp: &mut [i32], lo: usize, hi: usize) {
    if hi - lo <= 1 {
        return;
    }
    let mid = lo + (hi - lo) / 2;
    merge(a, temp, lo, mid);
    merge(a, temp, mid, hi);
    for k in lo..hi {
        temp[k] = a[k];
    }
    let mut i = lo;
    let mut j = mid;
    let mut k = lo;
    while i < mid && j < hi {
        if temp[i] <= temp[j] {
            a[k] = temp[i];
            i += 1;
        } else {
            a[k] = temp[j];
            j += 1;
        }
        k += 1;
    }
    while i < mid {
        a[k] = temp[i];
        i += 1;
        k += 1;
    }
}
// The Hirsel scoped comparator is a code pointer and a borrowed environment.
// Keep both words and the Order result in the matched source; devirtualization is allowed.
#[derive(Clone, Copy)]
struct Compare {
    call: fn(&(), i32, i32) -> std::cmp::Ordering,
    environment: &'static (),
}
impl Compare {
    fn less(self, a: i32, b: i32) -> bool {
        (self.call)(self.environment, a, b).is_lt()
    }
}
fn compare_i32(_: &(), a: i32, b: i32) -> std::cmp::Ordering {
    a.cmp(&b)
}
fn partition(a: &mut [i32], lo: usize, hi: usize, before: Compare) -> usize {
    let mid = lo + (hi - lo) / 2;
    let last = hi - 1;
    if before.less(a[mid], a[lo]) {
        a.swap(mid, lo);
    }
    if before.less(a[last], a[lo]) {
        a.swap(last, lo);
    }
    if before.less(a[mid], a[last]) {
        a.swap(mid, last);
    }
    let mut store = lo;
    for i in lo..last {
        if before.less(a[i], a[last]) {
            a.swap(i, store);
            store += 1;
        }
    }
    a.swap(store, last);
    store
}
fn sift_down(a: &mut [i32], base: usize, start: usize, n: usize, before: Compare) {
    let mut root = start;
    while root * 2 + 1 < n {
        let mut child = root * 2 + 1;
        if child + 1 < n && before.less(a[base + child], a[base + child + 1]) {
            child += 1;
        }
        if !before.less(a[base + root], a[base + child]) {
            return;
        }
        a.swap(base + root, base + child);
        root = child;
    }
}
fn heapsort(a: &mut [i32], lo: usize, hi: usize, before: Compare) {
    let n = hi - lo;
    let mut i = n / 2;
    while i > 0 {
        i -= 1;
        sift_down(a, lo, i, n, before);
    }
    let mut end = n;
    while end > 1 {
        end -= 1;
        a.swap(lo, lo + end);
        sift_down(a, lo, 0, end, before);
    }
}
fn sort_range(a: &mut [i32], start: usize, end: usize, limit: usize, before: Compare) {
    let mut lo = start;
    let mut hi = end;
    let mut depth = limit;
    while hi - lo > 16 {
        if depth == 0 {
            heapsort(a, lo, hi, before);
            return;
        }
        depth -= 1;
        let p = partition(a, lo, hi, before);
        if p - lo < hi - p {
            sort_range(a, lo, p, depth, before);
            lo = p + 1;
        } else {
            sort_range(a, p + 1, hi, depth, before);
            hi = p;
        }
    }
    if hi - lo < 2 {
        return;
    }
    for i in lo + 1..hi {
        let mut j = i;
        while j > lo && before.less(a[j], a[j - 1]) {
            a.swap(j, j - 1);
            j -= 1;
        }
    }
}
fn library_sort(a: &mut [i32], before: Compare) {
    let mut limit = 0;
    let mut m = a.len();
    while m > 1 {
        limit += 2;
        m /= 2;
    }
    sort_range(a, 0, a.len(), limit, before);
}
fn run(c: &support::Config) -> u32 {
    assert!(matches!(
        c.name.as_str(),
        "Sort::Quick" | "Sort::Merge" | "Sort::Self"
    ));
    let n = c.number("size");
    let mut data = vec![0i32; n];
    let mut g = support::Lcg::new();
    for x in &mut data {
        *x = g.integer(1000000);
    }
    let mut total = 0u32;
    for phase in 0..2 {
        if phase == 1 {
            g.reset();
        }
        let count = if phase == 0 { c.warmup } else { c.iterations };
        for _ in 0..count {
            let at = usize::try_from(g.integer(i32::try_from(n).unwrap())).unwrap();
            total = total.wrapping_add(u32::try_from(data[at]).unwrap());
            let mut t = Vec::new();
            t.extend_from_slice(&data);
            match c.name.as_str() {
                "Sort::Quick" => quick(&mut t, 0, n),
                "Sort::Merge" => {
                    let mut temp = vec![0; n];
                    merge(&mut t, &mut temp, 0, n);
                }
                _ => library_sort(&mut t, Compare { call: compare_i32, environment: &() }),
            }
            let at = usize::try_from(g.integer(i32::try_from(n).unwrap())).unwrap();
            total = total.wrapping_add(u32::try_from(t[at]).unwrap());
        }
    }
    total
}
fn main() {
    support::main(run);
}
