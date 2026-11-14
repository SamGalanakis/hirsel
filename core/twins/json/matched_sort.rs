// Adapted from reviewed LangArena source introsort, matching std.list.sort.
// The Hirsel scoped comparator is a code pointer and a borrowed environment.
// Keep both words and the Order result in the matched source; devirtualization is allowed.

pub struct Compare<'a, E> {
    pub call: fn(&E, u32, u32) -> std::cmp::Ordering,
    pub environment: &'a E,
}
impl<E> Copy for Compare<'_, E> {}
impl<E> Clone for Compare<'_, E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E> Compare<'_, E> {
    fn less(self, a: u32, b: u32) -> bool {
        (self.call)(self.environment, a, b).is_lt()
    }
}
fn partition<E>(a: &mut [u32], lo: usize, hi: usize, before: Compare<'_, E>) -> usize {
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
fn sift_down<E>(a: &mut [u32], base: usize, start: usize, n: usize, before: Compare<'_, E>) {
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
fn heapsort<E>(a: &mut [u32], lo: usize, hi: usize, before: Compare<'_, E>) {
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
fn sort_range<E>(a: &mut [u32], start: usize, end: usize, limit: usize, before: Compare<'_, E>) {
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
pub fn library_sort<E>(a: &mut [u32], before: Compare<'_, E>) {
    let mut limit = 0;
    let mut m = a.len();
    while m > 1 {
        limit += 2;
        m /= 2;
    }
    sort_range(a, 0, a.len(), limit, before);
}
