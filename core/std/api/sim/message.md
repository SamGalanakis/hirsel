# Named protocol messages

`std.sim.message` queues whole protocol messages in the task driving a
simulation. Use `hold_until` with the sender index, receiver index, message
name, and event name. Call `fire` when the protocol reaches that event.

`send` appends to the queue. `receive` takes the first eligible message for
one receiver, skipping messages still held. A hold applies to every message
with that direction and name. If several holds match, all their events must
fire. An event remains fired for the run, including for later sends.

`repro(queue, clocks)` returns the seed, clock pauses, and hold rules on one
line. Save the line before handing the clocks to `sim.world.drive` so the
law can report it on failure. These rules consume no random draws.
