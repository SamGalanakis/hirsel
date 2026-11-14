// Mirrors core/core/list.hirsel sort_stable and Hole: one scratch block,
// Reviewed-by: langs-numeric-920 2026-11-14; BWT2 and persistent-slot Huff2 match core7e93579e merge decisions, carriers, Ordering and restoration; no adapter scan.
// alternating bottom-up passes, right only on Less, and the same cleanup ranges.
// usize slots are plain copies. Box slots use Option's pointer niche for holes;
// ManuallyDrop prevents a second element-drop walk over the emptied scratch.
use std::cmp::Ordering;
use std::mem::ManuallyDrop;

trait Slot {
    fn take(&mut self) -> Self;
}
impl Slot for usize {
    fn take(&mut self) -> Self {
        *self
    }
}
impl<T> Slot for Option<Box<T>> {
    fn take(&mut self) -> Self {
        Option::take(self)
    }
}
trait Before<S> {
    fn order(&self, a: &S, b: &S) -> Ordering;
}
struct IndexCompare<'a> {
    call: fn(&Vec<(usize, usize)>, usize, usize) -> Ordering,
    environment: &'a Vec<(usize, usize)>,
}
impl Before<usize> for IndexCompare<'_> {
    fn order(&self, a: &usize, b: &usize) -> Ordering {
        (self.call)(self.environment, *a, *b)
    }
}
struct BoxCompare<'a, T, E> {
    call: fn(&E, &Box<T>, &Box<T>) -> Ordering,
    environment: &'a E,
}
impl<T, E> Before<Option<Box<T>>> for BoxCompare<'_, T, E> {
    fn order(&self, a: &Option<Box<T>>, b: &Option<Box<T>>) -> Ordering {
        (self.call)(self.environment, a.as_ref().unwrap(), b.as_ref().unwrap())
    }
}
struct Hole<'a, S: Slot> {
    xs: &'a mut Vec<S>,
    scratch: Vec<ManuallyDrop<S>>,
    source_is_xs: bool,
    a: usize,
    mid: usize,
    b: usize,
    end: usize,
    out: usize,
    n: usize,
}
impl<S: Slot> Hole<'_, S> {
    fn from_xs(&mut self, at: usize) {
        let value = self.xs[at].take();
        self.scratch.push(ManuallyDrop::new(value));
    }
    fn to_xs(&mut self, at: usize, out: usize) {
        self.xs[out] = self.scratch[at].take();
    }
}
impl<S: Slot> Drop for Hole<'_, S> {
    fn drop(&mut self) {
        let mut read = 0;
        if self.source_is_xs {
            for i in 0..self.a {
                self.to_xs(read, i);
                read = read.wrapping_add(1);
            }
            for i in self.mid..self.b {
                self.to_xs(read, i);
                read = read.wrapping_add(1);
            }
        } else {
            let mut out = self.out;
            for i in self.a..self.mid {
                self.to_xs(i, out);
                out = out.wrapping_add(1);
            }
            for i in self.b..self.end {
                self.to_xs(i, out);
                out = out.wrapping_add(1);
            }
            for i in self.end..self.n {
                self.to_xs(i, out);
                out = out.wrapping_add(1);
            }
        }
    }
}
fn sort_stable<S: Slot, C: Before<S>>(xs: &mut Vec<S>, before: C) {
    let n = xs.len();
    if n < 2 {
        return;
    }
    let mut g = Hole {
        xs,
        scratch: Vec::with_capacity(n),
        source_is_xs: true,
        a: 0,
        mid: 0,
        b: 0,
        end: 0,
        out: 0,
        n,
    };
    let mut width = 1;
    while width < n {
        let mut lo = 0;
        if g.source_is_xs {
            g.scratch.clear();
        }
        while lo < n {
            g.mid = lo.wrapping_add(width.min(n.wrapping_sub(lo)));
            g.end = g.mid.wrapping_add(width.min(n.wrapping_sub(g.mid)));
            g.a = lo;
            g.b = g.mid;
            if g.source_is_xs {
                while g.a < g.mid && g.b < g.end {
                    let right = before.order(&g.xs[g.b], &g.xs[g.a]) == Ordering::Less;
                    if right {
                        g.from_xs(g.b);
                        g.b = g.b.wrapping_add(1);
                    } else {
                        g.from_xs(g.a);
                        g.a = g.a.wrapping_add(1);
                    }
                    g.out = g.out.wrapping_add(1);
                }
                while g.a < g.mid {
                    g.from_xs(g.a);
                    g.a = g.a.wrapping_add(1);
                    g.out = g.out.wrapping_add(1);
                }
                while g.b < g.end {
                    g.from_xs(g.b);
                    g.b = g.b.wrapping_add(1);
                    g.out = g.out.wrapping_add(1);
                }
            } else {
                while g.a < g.mid && g.b < g.end {
                    let right = before.order(&g.scratch[g.b], &g.scratch[g.a]) == Ordering::Less;
                    if right {
                        g.to_xs(g.b, g.out);
                        g.b = g.b.wrapping_add(1);
                    } else {
                        g.to_xs(g.a, g.out);
                        g.a = g.a.wrapping_add(1);
                    }
                    g.out = g.out.wrapping_add(1);
                }
                while g.a < g.mid {
                    g.to_xs(g.a, g.out);
                    g.a = g.a.wrapping_add(1);
                    g.out = g.out.wrapping_add(1);
                }
                while g.b < g.end {
                    g.to_xs(g.b, g.out);
                    g.b = g.b.wrapping_add(1);
                    g.out = g.out.wrapping_add(1);
                }
            }
            lo = g.end;
        }
        g.source_is_xs = !g.source_is_xs;
        g.a = 0;
        g.mid = 0;
        g.b = 0;
        g.end = 0;
        g.out = 0;
        width = if width >= n.wrapping_sub(width) {
            n
        } else {
            width.wrapping_mul(2)
        };
    }
}
fn compare_indices(pairs: &Vec<(usize, usize)>, a: usize, b: usize) -> Ordering {
    pairs[a].cmp(&pairs[b])
}
pub fn indices(xs: &mut Vec<usize>, pairs: &Vec<(usize, usize)>) {
    sort_stable(
        xs,
        IndexCompare {
            call: compare_indices,
            environment: pairs,
        },
    );
}
// Persistent pointer-sized optional slots model core's temporary Box holes.
// Hole restores every slot; no full-buffer conversion or validation follows it.
pub fn boxes<T, E>(
    xs: &mut Vec<Option<Box<T>>>,
    environment: &E,
    call: fn(&E, &Box<T>, &Box<T>) -> Ordering,
) {
    sort_stable(xs, BoxCompare { call, environment });
}
