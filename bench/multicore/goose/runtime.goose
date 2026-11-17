import cli;

let LANES = 16;
struct Request { value: i64 }
struct Reply { value: i64 }
extern fn sched_yield() -> i32;

thread_fn leaf(value: i64) {
    while !qput(Reply { value }) { sched_yield(); }
}

thread_fn chain(lane: i64, rounds: i64) {
    for i in rounds { thread_wait(thread_spawn(leaf, lane * rounds + i + 1)); }
}

thread_fn producer(lane: i64, rounds: i64) {
    for i in rounds {
        while !qput(Reply { lane * rounds + i + 1 }) { sched_yield(); }
    }
}

thread_fn echo(rounds: i64) {
    for i in rounds {
        let request = qget<Request>();
        if !qput(Reply { request.value + 1 }) { abort("reply queue full"); }
    }
}

fn main() {
    var name: u8[>..] = [];
    arg(1, name);
    let rounds = number(2);
    var sum = 0;
    if name == "spawn_join" {
        qlimit<Reply>(1);
        for i in rounds {
            thread_wait(thread_spawn(leaf, i + 1));
            sum += qget<Reply>().value;
        }
    } else if name == "ping_pong" {
        qlimit<Request>(1);
        qlimit<Reply>(1);
        let id = thread_spawn(echo, rounds);
        for i in rounds {
            if !qput(Request { i }) { abort("request queue full"); }
            sum += qget<Reply>().value;
        }
        thread_wait(id);
    } else {
        qlimit<Reply>(64);
        var ids: i64[>..] = [];
        if name == "spawn_chain" {
            for i in LANES { ids.push(thread_spawn(chain, i, rounds)); }
        } else if name == "fan_in" {
            for i in LANES { ids.push(thread_spawn(producer, i, rounds)); }
        } else {
            abort("unknown workload");
        }
        for i in LANES * rounds { sum += qget<Reply>().value; }
        for id in ids { thread_wait(id); }
    }
    print(sum);
}
