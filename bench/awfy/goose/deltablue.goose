// AWFY JavaScript deltablue.js. The planner owns the graph and its edge lists.
namespace deltablue;
import std;

const REQUIRED = -800;
const PREFERRED = -400;
const STRONG_DEFAULT = -200;
const DEFAULT = 0;
const WEAKEST = 10000;

enum Kind { Edit, Stay, Equality, Scale { scale: i64, offset: i64 } }
enum Direction { Unchosen, Forward, Backward }

struct Constraint {
    kind: Kind,
    strength: i64,
    first: i64,
    second: i64,
    direction: Direction = Direction.Unchosen,
    satisfied: bool = false
}

struct Variable {
    value: i64,
    first_edge: i64 = -1,
    last_edge: i64 = -1,
    source: i64 = -1,
    mark: i64 = 0,
    walk: i64 = WEAKEST,
    stay: bool = true
}

struct Edge { constraint: i64, next: i64 = -1 }

root struct Planner {
    vars: Variable[>..] = [],
    constraints: Constraint[>..] = [],
    edges: Edge[>..] = [],
    pending: i64[>..<] = [],
    mark: i64 = 1,
    valid: bool = true
}

fn unary(c: Constraint) -> bool {
    return match c.kind { Edit, Stay => true, _ => false };
}

fn edit(c: Constraint) -> bool {
    return match c.kind { Edit => true, _ => false };
}

fn selected(c: Constraint) -> bool {
    return if unary(c) { c.satisfied } else { c.direction != Direction.Unchosen };
}

fn output(c: Constraint) -> i64 {
    return if c.direction == Direction.Forward { c.second } else { c.first };
}

fn input(c: Constraint) -> i64 {
    return if c.direction == Direction.Forward { c.first } else { c.second };
}

fn add_variable(p: Planner&, value: i64) -> i64 {
    let index = p.vars.len;
    p.vars.push(Variable { value: value });
    return index;
}

fn append_edge(p: Planner&, v: i64, id: i64) {
    let old = p.vars[v].last_edge;
    let index = p.edges.len;
    p.edges.push(Edge { constraint: id });
    if old < 0 { p.vars[v].first_edge = index; }
    else { p.edges[old].next = index; }
    p.vars[v].last_edge = index;
}

fn add_to_graph(p: Planner&, id: i64) {
    let c = p.constraints[id];
    append_edge(p, c.first, id);
    if !unary(c) {
        append_edge(p, c.second, id);
        match c.kind {
            Scale args => {
                append_edge(p, args.scale, id);
                append_edge(p, args.offset, id);
            },
            _ => {},
        }
    }
}

fn remove_edge(p: Planner&, v: i64, id: i64) {
    var prev = -1;
    var edge = p.vars[v].first_edge;
    while edge >= 0 {
        let next = p.edges[edge].next;
        if p.edges[edge].constraint == id {
            if prev < 0 { p.vars[v].first_edge = next; }
            else { p.edges[prev].next = next; }
            if next < 0 { p.vars[v].last_edge = prev; }
        } else {
            prev = edge;
        }
        edge = next;
    }
    if p.vars[v].source == id { p.vars[v].source = -1; }
}

fn unselect(c: Constraint&) {
    c.direction = Direction.Unchosen;
    c.satisfied = false;
}

fn remove_from_graph(p: Planner&, id: i64) {
    let c = p.constraints[id];
    remove_edge(p, c.first, id);
    if !unary(c) {
        remove_edge(p, c.second, id);
        match c.kind {
            Scale args => {
                remove_edge(p, args.scale, id);
                remove_edge(p, args.offset, id);
            },
            _ => {},
        }
    }
    unselect(p.constraints[id]);
}

fn choose(p: Planner&, id: i64, mark: i64) {
    let c = p.constraints[id];
    let a_mark = p.vars[c.first].mark;
    let a_walk = p.vars[c.first].walk;
    if unary(c) {
        p.constraints[id].satisfied = a_mark != mark && c.strength < a_walk;
        return;
    }
    let b_mark = p.vars[c.second].mark;
    let b_walk = p.vars[c.second].walk;
    p.constraints[id].direction = if a_mark == mark {
        if b_mark != mark && c.strength < b_walk { Direction.Forward } else { Direction.Unchosen }
    } else if b_mark == mark {
        if a_mark != mark && c.strength < a_walk { Direction.Backward } else { Direction.Unchosen }
    } else if a_walk > b_walk {
        if c.strength < a_walk { Direction.Backward } else { Direction.Unchosen }
    } else if c.strength < b_walk {
        Direction.Forward
    } else {
        Direction.Unchosen
    };
}

fn execute(p: Planner&, c: Constraint) {
    match c.kind {
        Equality => { p.vars[output(c)].value = p.vars[input(c)].value; },
        Scale args => {
            let s = p.vars[args.scale].value;
            let o = p.vars[args.offset].value;
            if c.direction == Direction.Forward {
                p.vars[c.second].value = p.vars[c.first].value * s + o;
            } else {
                p.vars[c.first].value = (p.vars[c.second].value - o) / s;
            }
        },
        _ => {},
    }
}

fn recalculate(p: Planner&, c: Constraint) {
    let dest = output(c);
    if unary(c) {
        p.vars[dest].walk = c.strength;
        p.vars[dest].stay = !edit(c);
    } else {
        let source = input(c);
        let strength = p.vars[source].walk;
        p.vars[dest].walk = if c.strength > strength { c.strength } else { strength };
        var stay = p.vars[source].stay;
        match c.kind {
            Scale args => { stay = stay && p.vars[args.scale].stay && p.vars[args.offset].stay; },
            _ => {},
        }
        p.vars[dest].stay = stay;
    }
    if p.vars[dest].stay { execute(p, c); }
}

fn inputs_known(p: Planner&, c: Constraint, mark: i64) -> bool {
    if unary(c) { return true; }
    let source .= p.vars[input(c)];
    return source.mark == mark || source.stay || source.source < 0;
}

fn mark_inputs(p: Planner&, c: Constraint, mark: i64) {
    if !unary(c) {
        p.vars[input(c)].mark = mark;
        match c.kind {
            Scale args => {
                p.vars[args.scale].mark = mark;
                p.vars[args.offset].mark = mark;
            },
            _ => {},
        }
    }
}

fn append_consumers(p: Planner&, v: i64, todo: i64[>..]&) {
    var edge = p.vars[v].first_edge;
    while edge >= 0 {
        let id = p.edges[edge].constraint;
        if id != p.vars[v].source && selected(p.constraints[id]) { todo.push(id); }
        edge = p.edges[edge].next;
    }
}

recursive fn add_propagate(p: Planner&, id: i64, mark: i64) -> bool {
    {
        var todo: i64[>..] = [];
        todo.push(id);
        var index = 0;
        while index < todo.len && p.vars[output(p.constraints[todo[index]])].mark != mark {
            let c = p.constraints[todo[index]];
            recalculate(p, c);
            append_consumers(p, output(c), todo);
            index += 1;
        }
        if index == todo.len { return true; }
    }
    incremental_remove(p, id);
    return false;
}

recursive fn satisfy(p: Planner&, id: i64, mark: i64) -> i64 {
    choose(p, id, mark);
    let c = p.constraints[id];
    if !selected(c) {
        p.valid = p.valid && c.strength != REQUIRED;
        return -1;
    }
    mark_inputs(p, c, mark);
    let dest = output(c);
    let overridden = p.vars[dest].source;
    if overridden >= 0 { unselect(p.constraints[overridden]); }
    p.vars[dest].source = id;
    if !add_propagate(p, id, mark) {
        p.valid = false;
        return -1;
    }
    p.vars[dest].mark = mark;
    return overridden;
}

recursive fn incremental_add(p: Planner&, id: i64) {
    p.mark += 1;
    let mark = p.mark;
    var next = id;
    while next >= 0 { next = satisfy(p, next, mark); }
}

fn add_constraint(p: Planner&, kind: Kind, strength: i64, first: i64, second: i64) -> i64 {
    let id = p.constraints.len;
    p.constraints.push(Constraint { kind: kind, strength: strength, first: first, second: second });
    add_to_graph(p, id);
    incremental_add(p, id);
    return id;
}

fn remove_propagate(p: Planner&, start: i64) {
    p.vars[start].source = -1;
    p.vars[start].walk = WEAKEST;
    p.vars[start].stay = true;
    let pending_start = p.pending.len;
    var todo: i64[>..] = [];
    todo.push(start);
    var index = 0;
    while index < todo.len {
        let v = todo[index];
        var edge = p.vars[v].first_edge;
        while edge >= 0 {
            let id = p.edges[edge].constraint;
            if !selected(p.constraints[id]) { p.pending.push(id); }
            edge = p.edges[edge].next;
        }
        edge = p.vars[v].first_edge;
        while edge >= 0 {
            let id = p.edges[edge].constraint;
            let c = p.constraints[id];
            if id != p.vars[v].source && selected(c) {
                recalculate(p, c);
                todo.push(output(c));
            }
            edge = p.edges[edge].next;
        }
        index += 1;
    }
    // AWFY Vector.sort's swap path is unimplemented.
    let count = p.pending.len - pending_start;
    p.valid = p.valid && (count <= 1 || (count == 2 &&
        p.constraints[p.pending[pending_start]].strength < p.constraints[p.pending[pending_start + 1]].strength));
}

recursive fn incremental_remove(p: Planner&, id: i64) {
    let dest = output(p.constraints[id]);
    unselect(p.constraints[id]);
    remove_from_graph(p, id);
    let start = p.pending.len;
    remove_propagate(p, dest);
    let end = p.pending.len;
    for i in start..end { incremental_add(p, p.pending[i]); }
    p.pending.resize(start);
}

fn destroy(p: Planner&, id: i64) {
    if selected(p.constraints[id]) { incremental_remove(p, id); }
    remove_from_graph(p, id);
}

fn make_plan(p: Planner&, edit_id: i64, plan: i64[>..]&) {
    p.mark += 1;
    let mark = p.mark;
    var todo: i64[>..] = [];
    if edit(p.constraints[edit_id]) && selected(p.constraints[edit_id]) { todo.push(edit_id); }
    var index = 0;
    while index < todo.len {
        let id = todo[index];
        let c = p.constraints[id];
        let dest = output(c);
        if p.vars[dest].mark != mark && inputs_known(p, c, mark) {
            plan.push(id);
            p.vars[dest].mark = mark;
            append_consumers(p, dest, todo);
        }
        index += 1;
    }
}

fn execute_plan(p: Planner&, plan: const i64[:]) {
    for id in plan { execute(p, p.constraints[id]); }
}

fn change(p: Planner&, v: i64, value: i64) {
    let id = add_constraint(p, Kind.Edit, PREFERRED, v, v);
    var plan: i64[>..] = [];
    make_plan(p, id, plan);
    for _ in 10 {
        p.vars[v].value = value;
        execute_plan(p, plan);
    }
    destroy(p, id);
}

fn chain(size: i64) -> bool {
    var p = Planner { .. };
    var vars: i64[>..] = [];
    for _ in size + 1 { vars.push(add_variable(p, 0)); }
    for i in size { add_constraint(p, Kind.Equality, REQUIRED, vars[i], vars[i + 1]); }
    let first = vars[0];
    let last = vars[size];
    add_constraint(p, Kind.Stay, STRONG_DEFAULT, last, last);
    let id = add_constraint(p, Kind.Edit, PREFERRED, first, first);
    var plan: i64[>..] = [];
    make_plan(p, id, plan);
    for value in 100 {
        p.vars[first].value = value;
        execute_plan(p, plan);
        if p.vars[last].value != value { return false; }
    }
    destroy(p, id);
    return p.valid;
}

fn projection(size: i64) -> bool {
    var p = Planner { .. };
    var destinations: i64[>..] = [];
    let scale = add_variable(p, 10);
    let offset = add_variable(p, 1000);
    var src = 0;
    var dst = 0;
    for i in 1..size + 1 {
        src = add_variable(p, i);
        dst = add_variable(p, i);
        destinations.push(dst);
        add_constraint(p, Kind.Stay, DEFAULT, src, src);
        add_constraint(p, Kind.Scale { scale: scale, offset: offset }, REQUIRED, src, dst);
    }
    change(p, src, 17);
    if p.vars[dst].value != 1170 { return false; }
    change(p, dst, 1050);
    if p.vars[src].value != 5 { return false; }
    change(p, scale, 5);
    for i in size - 1 {
        if p.vars[destinations[i]].value != (i + 1) * 5 + 1000 { return false; }
    }
    change(p, offset, 2000);
    for i in size - 1 {
        if p.vars[destinations[i]].value != (i + 1) * 5 + 2000 { return false; }
    }
    return p.valid;
}

fn benchmark(size: i64) -> bool {
    return chain(size) && projection(size);
}
