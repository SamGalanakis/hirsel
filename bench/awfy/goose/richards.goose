// AWFY Richards from the JavaScript reference. Task and packet IDs index arenas.
namespace richards;
import std;
const IDLE: i64 = 0;
const WORKER: i64 = 1;
const HANDLER_A: i64 = 2;
const HANDLER_B: i64 = 3;
const DEVICE_A: i64 = 4;
const DEVICE_B: i64 = 5;

struct Packet {
    link: i64,
    identity: i64,
    is_work: bool,
    datum: i64,
    data: i64[4]
}

enum Action {
    Device { pending: i64 },
    Handler { work: i64, device: i64 },
    Idle { control: i64, count: i64 },
    Worker { destination: i64, count: i64 }
}

struct TaskControl {
    link: i64,
    identity: i64,
    priority: i64,
    input: i64,
    packet_pending: bool,
    waiting: bool,
    holding: bool,
    action: Action
}

root struct Scheduler {
    packets: Packet[>..],
    tasks: TaskControl[>..],
    table: i64[6],
    head: i64,
    current: i64,
    current_identity: i64,
    queue_count: i64,
    hold_count: i64
}

fn append(s: Scheduler&, p: i64, queue: i64) -> i64 {
    s.packets[p].link = -1;
    if queue < 0 { return p; }
    var last = queue;
    while s.packets[last].link >= 0 {
        last = s.packets[last].link;
    }
    s.packets[last].link = p;
    return queue;
}

fn create_packet(s: Scheduler&, link: i64, identity: i64, is_work: bool) -> i64 {
    let index = s.packets.len;
    s.packets.push(Packet { link: link, identity: identity, is_work: is_work, datum: 0, data: [0; 4] });
    return index;
}

fn create_task(s: Scheduler&, identity: i64, priority: i64, input: i64,
               waiting: bool, action: Action) {
    let index = s.tasks.len;
    s.tasks.push(TaskControl { link: s.head, identity: identity, priority: priority, input: input,
        packet_pending: input >= 0, waiting: waiting, holding: false, action: action });
    s.head = index;
    s.table[identity] = index;
}

fn hold_self(s: Scheduler&) -> i64 {
    s.hold_count += 1;
    s.tasks[s.current].holding = true;
    return s.tasks[s.current].link;
}

fn mark_waiting(s: Scheduler&) -> i64 {
    s.tasks[s.current].waiting = true;
    return s.current;
}

fn release(s: Scheduler&, identity: i64) -> i64 {
    let t = s.table[identity];
    s.tasks[t].holding = false;
    return if s.tasks[t].priority > s.tasks[s.current].priority { t } else { s.current };
}

fn queue_packet(s: Scheduler&, p: i64) -> i64 {
    let t = s.table[s.packets[p].identity];
    s.queue_count += 1;
    s.packets[p].link = -1;
    s.packets[p].identity = s.current_identity;
    let input = s.tasks[t].input;
    if input < 0 {
        s.tasks[t].input = p;
        s.tasks[t].packet_pending = true;
        if s.tasks[t].priority > s.tasks[s.current].priority {
            return t;
        }
    } else {
        let extended = append(s, p, input);
        s.tasks[t].input = extended;
    }
    return s.current;
}

fn run_device(s: Scheduler&, t: i64, message: i64, pending: i64) -> i64 {
    if message >= 0 {
        let p = message;
        s.tasks[t].action = Action.Device { pending: p };
        return hold_self(s);
    }
    if pending >= 0 {
        let p = pending;
        s.tasks[t].action = Action.Device { pending: -1 };
        return queue_packet(s, p);
    }
    return mark_waiting(s);
}

fn run_handler(s: Scheduler&, t: i64, message: i64,
               work_in: i64, device_in: i64) -> i64 {
    var work = work_in;
    var device = device_in;
    if message >= 0 {
        let p = message;
        if s.packets[p].is_work {
            work = append(s, p, work);
        } else {
            device = append(s, p, device);
        }
    }
    s.tasks[t].action = Action.Handler { work: work, device: device };
    if work < 0 { return mark_waiting(s); }
    let work_packet = work;
    let n = s.packets[work_packet].datum;
    if n >= 4 {
        s.tasks[t].action = Action.Handler { work: s.packets[work_packet].link, device: device };
        return queue_packet(s, work_packet);
    }
    if device < 0 { return mark_waiting(s); }
    let device_packet = device;
    s.tasks[t].action = Action.Handler { work: work, device: s.packets[device_packet].link };
    s.packets[device_packet].datum = s.packets[work_packet].data[n];
    s.packets[work_packet].datum = n + 1;
    return queue_packet(s, device_packet);
}

fn run_idle(s: Scheduler&, t: i64, control: i64, count: i64) -> i64 {
    let remaining = count - 1;
    if remaining == 0 {
        s.tasks[t].action = Action.Idle { control: control, count: remaining };
        return hold_self(s);
    }
    if (control & 1) == 0 {
        s.tasks[t].action = Action.Idle { control: control / 2, count: remaining };
        return release(s, DEVICE_A);
    } else {
        s.tasks[t].action = Action.Idle { control: (control / 2) ^ 53256, count: remaining };
        return release(s, DEVICE_B);
    }
}

fn run_worker(s: Scheduler&, t: i64, message: i64, dest: i64, count: i64) -> i64 {
    if message < 0 { return mark_waiting(s); }
    let p = message;
    let target = if dest == HANDLER_A { HANDLER_B } else { HANDLER_A };
    var letter = count;
    s.packets[p].identity = target;
    s.packets[p].datum = 0;
    for i in 4 {
        letter += 1;
        if letter > 26 {
            letter = 1;
        }
        s.packets[p].data[i] = 65 + letter - 1;
    }
    s.tasks[t].action = Action.Worker { destination: target, count: letter };
    return queue_packet(s, p);
}

fn run_task(s: Scheduler&, t: i64) -> i64 {
    var message: i64 = -1;
    if s.tasks[t].packet_pending && s.tasks[t].waiting && !s.tasks[t].holding {
        let p = s.tasks[t].input;
        let link = s.packets[p].link;
        s.tasks[t].input = link;
        s.tasks[t].packet_pending = link >= 0;
        s.tasks[t].waiting = false;
        s.tasks[t].holding = false;
        message = p;
    }
    return match s.tasks[t].action {
        Device a => run_device(s, t, message, a.pending),
        Handler a => run_handler(s, t, message, a.work, a.device),
        Idle a => run_idle(s, t, a.control, a.count),
        Worker a => run_worker(s, t, message, a.destination, a.count)
    };
}

fn schedule(s: Scheduler&) {
    var current = s.head;
    while current >= 0 {
        let t = current;
        if s.tasks[t].holding || (!s.tasks[t].packet_pending && s.tasks[t].waiting) {
            current = s.tasks[t].link;
        } else {
            s.current = t;
            s.current_identity = s.tasks[t].identity;
            current = run_task(s, t);
        }
    }
}

fn packet_chain(s: Scheduler&, count: i64, identity: i64, is_work: bool) -> i64 {
    var head: i64 = -1;
    for _ in count {
        head = create_packet(s, head, identity, is_work);
    }
    return head;
}

fn benchmark(count: i64) -> bool {
    var s = Scheduler { packets: [], tasks: [], table: [0; 6],
        head: -1, current: 0, current_identity: 0, queue_count: 0, hold_count: 0 };
    create_task(s, IDLE, 0, -1, false, Action.Idle { control: 1, count: count });
    let worker = packet_chain(s, 2, WORKER, true);
    create_task(s, WORKER, 1000, worker, true, Action.Worker { destination: HANDLER_A, count: 0 });
    let first = packet_chain(s, 3, DEVICE_A, false);
    create_task(s, HANDLER_A, 2000, first, true, Action.Handler { work: -1, device: -1 });
    let second = packet_chain(s, 3, DEVICE_B, false);
    create_task(s, HANDLER_B, 3000, second, true, Action.Handler { work: -1, device: -1 });
    create_task(s, DEVICE_A, 4000, -1, true, Action.Device { pending: -1 });
    create_task(s, DEVICE_B, 5000, -1, true, Action.Device { pending: -1 });
    schedule(s);
    return s.queue_count == 23246 && s.hold_count == 9297;
}
