"""Bounded RUNNER-SPEC L55–L57 traces for shared views and publisher order."""
from dataclasses import dataclass, field
from itertools import product


MUTANTS = (
    "per_view_round_advance",
    "fence_twice",
    "skip_fence_when_equal",
    "shared_across_callers",
)


class Violation(Exception):
    def __init__(self, law, description, trace):
        super().__init__(description)
        self.law = law
        self.trace = tuple(trace)


@dataclass(frozen=True)
class Image:
    rows: tuple[str, ...]
    value: bytes
    reads: int = 100


@dataclass
class Session:
    budget: int
    due: bool = False
    image: Image | None = None
    ended: bool = False


@dataclass
class View:
    image: Image
    base: int
    sessions: dict[str, Session] = field(default_factory=dict)
    ended: bool = False


@dataclass
class Views:
    mutant: str | None
    base: int = 1
    live: dict = field(default_factory=dict)
    runs: int = 0
    encodes: int = 0

    def subscribe(self, entry, args, caller, session, budget, compute):
        key = entry, args, None if self.mutant == "shared_across_callers" else caller
        v = self.live.get(key)
        if v is None or v.ended:
            v = View(compute(), self.base)
            self.live[key] = v
            self.runs += 1
        assert v.base == self.base
        v.sessions[session] = Session(budget, image=v.image)
        return v

    def rerun(self, view, compute, bounced=()):
        old = view.image
        new = compute()
        self.runs += 1
        self.base += 1
        view.base = self.base
        view.image = new
        payload = new.value if new.value != old.value else b""
        group = (new.rows, payload)
        changed = old.rows != new.rows or bool(payload)
        if changed:
            self.encodes += 1
        sent = {}
        for name, session in view.sessions.items():
            if new.reads > session.budget:
                session.ended = True
                continue
            if session.due or not changed:
                continue
            if name in bounced:
                session.due = True
                continue
            session.image = new
            sent[name] = group
        return sent

    def repair(self, view, session):
        entry = view.sessions[session]
        if not entry.due or entry.ended:
            return None
        entry.due = False
        entry.image = view.image
        return view.image


def shared_view_once(mutant):
    m = Views(mutant)
    one = Image(("alice row",), b"one")
    other = Image(("bob row",), b"one")
    new = Image(("alice next",), b"next", reads=140)
    trace = ["alice subscribes query_as(7)"]
    v = m.subscribe("query_as", b"7", b"alice", "alice", 120, lambda: one)
    trace.append("bob subscribes query_as(7)")
    b = m.subscribe("query_as", b"7", b"bob", "bob", 150, lambda: other)
    if b.sessions["bob"].image != other:
        raise Violation("L55", "Bob received Alice's row", trace)
    same = m.subscribe("query_as", b"7", b"alice", "carol", 150, lambda: one)
    roomy = m.subscribe("query_as", b"7", b"alice", "dana", 150, lambda: one)
    assert v is same is roomy and m.runs == 2
    sent = m.rerun(v, lambda: new)
    assert m.runs == 3 and m.encodes == 1
    assert sent == {"carol": (("alice next",), b"next"), "dana": (("alice next",), b"next")}
    assert v.sessions["alice"].ended and v.sessions["alice"].image == one

    m = Views(mutant)
    v = m.subscribe("query", b"7", None, "alice", 150, lambda: one)
    m.rerun(v, lambda: Image(("second row",), b"two"), bounced=("alice",))
    assert v.sessions["alice"].due
    empty = Image((), b"zero")
    assert m.rerun(v, lambda: empty) == {}
    joined = m.subscribe("query", b"7", None, "dana", 150, lambda: one)
    assert joined is v and joined.sessions["dana"].image == empty and m.runs == 3
    assert m.repair(v, "alice") == empty
    assert m.repair(v, "alice") is None
    v.ended = True
    assert m.subscribe("query", b"7", None, "eve", 150, lambda: empty) is not v


@dataclass(frozen=True)
class Hit:
    name: str
    sessions: tuple[str, ...]
    read_hit: bool = True
    equal: bool = False
    offer: bool = True


def actions(views, mutant):
    visited = set()
    for view in views:
        yield "stage", view.name
        if mutant == "per_view_round_advance":
            yield from fence(view, visited, mutant)
            yield "publish", view.name
    if mutant != "per_view_round_advance":
        for view in views:
            yield from fence(view, visited, mutant)
        yield "publish", "all"
    for view in views:
        if view.offer:
            yield "offer", view.name


def fence(view, visited, mutant):
    if not view.read_hit or (view.equal and mutant == "skip_fence_when_equal"):
        return
    for session in view.sessions:
        if session not in visited or mutant == "fence_twice":
            visited.add(session)
            yield "fence", session


def schedules(views, mutant, expected_fences, law):
    steps = tuple(actions(views, mutant))
    sessions = sorted({s for v in views for s in v.sessions})
    checked = 0
    expected_order = None
    # Each writer holds a version-1 unit. Enumerate the cuts at which writers
    # take their acceptance load. Loads at the same cut do not affect each other.
    for cuts in product(range(len(steps) + 1), repeat=len(sessions)):
        r = 1
        fences = {s: (1, 1) for s in sessions}
        count = dict.fromkeys(sessions, 0)
        staged = set()
        publishes = 0
        offers = []
        ordering = None
        trace = []
        for index in range(len(steps) + 1):
            for session, cut in zip(sessions, cuts):
                if cut != index:
                    continue
                prior, latest = fences[session]
                passed = 1 >= (latest if latest <= r else prior)
                trace.append(f"writer {session} {'accepts' if passed else 'drops'} at R={r}")
                if passed != (r == 1 or session not in expected_fences):
                    raise Violation(law, "stale content reached a writer", trace)
            if index == len(steps):
                break
            op, arg = steps[index]
            trace.append(f"{op} {arg}")
            if op == "stage":
                staged.add(arg)
            elif op == "fence":
                count[arg] += 1
                if count[arg] > 1:
                    raise Violation(law, "a session was fenced twice", trace)
                fences[arg] = (fences[arg][1], 2)
            elif op == "publish":
                if not ordering and (publishes or len(staged) != len(views)):
                    ordering = tuple(trace)
                r = 2
                publishes += 1
            else:
                if r != 2:
                    raise Violation(law, "offered before R was published", trace)
                offers.append(arg)
        assert offers == [v.name for v in views if v.offer]
        if ordering is not None and expected_order is None:
            expected_order = ordering
        checked += 1
    if expected_order is not None:
        raise Violation(law, "R advanced before every view was staged", expected_order)
    assert publishes == 1
    assert {s for s, n in count.items() if n} == expected_fences
    return checked


def value_elided(mutant):
    m = Views(mutant)
    v = m.subscribe("count", b"7", None, "alice", 100, lambda: Image((), b"\x02"))
    m.subscribe("count", b"7", None, "bob", 100, lambda: Image((), b"\x02"))
    assert m.rerun(v, lambda: Image((), b"\x02")) == {}
    assert m.rerun(v, lambda: Image((), b"\x03"), bounced=("bob",)) == {"alice": ((), b"\x03")}
    assert m.rerun(v, lambda: Image((), b"\x03")) == {}
    assert m.repair(v, "bob") == Image((), b"\x03")
    v = m.subscribe("unit", b"", None, "alice", 100, lambda: Image(("old",), b""))
    assert m.rerun(v, lambda: Image(("new",), b"")) == {"alice": (("new",), b"")}
    return schedules((Hit("count", ("alice", "bob"), equal=True, offer=False),), mutant, {"alice", "bob"}, "L57")


def check_shared_views(mutant=None):
    shared_view_once(mutant)
    count = schedules((Hit("V", ("s1", "s2")), Hit("W", ("s1", "s3")),
                       Hit("Cover", ("s4",), read_hit=False)),
                      mutant, {"s1", "s2", "s3"}, "L56")
    return count + value_elided(mutant)


def read_result(rows, fault, fail_at):
    staged = []
    for index, row in enumerate(rows):
        if index == fail_at:
            return fault, ()
        staged.append(row)
    return None, tuple(staged)


def failed_read_barrier():
    source = ("new public row", "next row")
    next_base = ("replacement public row",)
    count = 0
    for fault, operation in product(("Reclaimed", "Remote", "Io"), ("changes", "look", "subscribe")):
        # Each operation yields one row (or page of one row) before refusing.
        error, rows = read_result(source, fault, 1)
        groups = {"s1": ("Reset", rows, ("Told",))} if error else {"s1": ("Delta", rows, ("Told",))}
        groups["s2"] = ("Delta", ("unaffected row",), ())
        if operation == "subscribe":
            assert error == fault and not rows
        count += schedules((Hit(operation, ("s1",)), Hit("unaffected", ("s2",), read_hit=False)),
                           None, {"s1"}, "L56")
        assert groups == {"s1": ("Reset", (), ("Told",)), "s2": ("Delta", ("unaffected row",), ())}
        assert read_result(next_base, None, -1) == (None, ("replacement public row",))
    return count
