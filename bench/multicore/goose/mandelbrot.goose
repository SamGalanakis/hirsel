import cli;

struct Block { first: i64, bytes: u8[>..] }

fn inside(cr: f64, ci: f64) -> bool {
    var zr = cr;
    var zi = ci;
    for step in 50 {
        let rr = zr * zr;
        let ii = zi * zi;
        if rr + ii > 4.0 { return false; }
        zi = (zr + zr) * zi + ci;
        zr = rr - ii + cr;
    }
    return true;
}

thread_fn render(width: i64, first: i64, last: i64) {
    let inverse = 2.0 / (width as f64);
    var pixels: u8[>..] = [];
    for y in first..last {
        let ci = inverse * (y as f64) - 1.0;
        for x in width / 8 {
            var byte: u8 = 0;
            for bit in 8 {
                let cr = inverse * ((x * 8 + bit) as f64) - 1.5;
                byte = byte << 1;
                if inside(cr, ci) { byte |= 1; }
            }
            pixels.push(byte);
        }
    }
    qput(Block { first: first, bytes: copy(pixels) });
}

fn main() {
    let width = number(1);
    let workers = number(2);
    if width % 8 != 0 { abort("width must be a multiple of eight"); }
    var ids: i64[>..] = [];
    var image: u8[>..] = [];
    image.resize(width * width / 8, 0);
    let start = clock_ns();
    for i in workers {
        ids.push(thread_spawn(render, width, i * width / workers, (i + 1) * width / workers));
    }
    for i in workers {
        let part = qget<Block>();
        let at = part.first * width / 8;
        for byte, j in part.bytes { image[at + j] = byte; }
    }
    for id in ids { thread_wait(id); }
    let elapsed = clock_ns() - start;
    var timing: u8[>..] = [];
    format(timing, "multicore-kernel-ns=", elapsed, "\n");
    write_stderr(timing);
    var header: u8[>..] = [];
    format(header, "P4\n", width, " ", width, "\n");
    write_stdout(header);
    write_stdout(image);
}
