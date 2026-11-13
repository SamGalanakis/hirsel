// Review pending: langs-text-921. No matched ratio before independent review.
// Matches game_of_life.hirsel: flat AoS cells and eight inline u64 neighbor indices.
#[allow(dead_code)]
mod support;
struct Cell {
    alive: bool,
    next_state: bool,
    neighbors: [u64; 8],
}
fn run(c: &support::Config) -> u32 {
    assert_eq!(c.name, "Etc::GameOfLife");
    let w = c.number("w");
    let h = c.number("h");
    let mut cells = Vec::with_capacity(w * h);
    let mut g = support::Lcg::new();
    for y in 0..h {
        for x in 0..w {
            let mut neighbors = [0u64; 8];
            let mut k = 0;
            for dy in -1i64..2 {
                for dx in -1i64..2 {
                    if dx != 0 || dy != 0 {
                        let xx = usize::try_from(
                            (i64::try_from(x).unwrap() + dx + i64::try_from(w).unwrap())
                                % i64::try_from(w).unwrap(),
                        )
                        .unwrap();
                        let yy = usize::try_from(
                            (i64::try_from(y).unwrap() + dy + i64::try_from(h).unwrap())
                                % i64::try_from(h).unwrap(),
                        )
                        .unwrap();
                        neighbors[k] = u64::try_from(yy * w + xx).unwrap();
                        k += 1;
                    }
                }
            }
            cells.push(Cell {
                alive: g.float(1.0).total_cmp(&0.1).is_lt(),
                next_state: false,
                neighbors,
            });
        }
    }
    for _ in 0..c.warmup + c.iterations {
        for i in 0..w * h {
            let mut count = 0u32;
            for j in 0..8 {
                if cells[usize::try_from(cells[i].neighbors[j]).unwrap()].alive {
                    count += 1;
                }
            }
            cells[i].next_state = count == 3 || cells[i].alive && count == 2;
        }
        for cell in &mut cells {
            cell.alive = cell.next_state;
        }
    }
    let mut hash = 2166136261u32;
    let mut alive = 0u32;
    for c in &cells {
        let bit = u32::from(c.alive);
        alive += bit;
        hash = (hash ^ bit).wrapping_mul(16777619);
    }
    hash + alive
}
fn main() {
    support::main(run);
}
