pub const SOURCE: &str = r#"
fn release_write(take w: WriteHalf) {}
fn wire(n: Net, c: Clock) -> u64 waits {
    let (listener0, local_address) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433u32), port: 0), 16))
    let mut listener = listener0
    let client = result.unwrap(host.connect(n, local_address, Never))
    let (server, peer) = result.unwrap(host.accept(mut listener, Never))
    let (cr, cw0) = net.split(take client)
    let (sr0, sw) = net.split(take server)
    let mut cw = cw0
    let mut sr = sr0
    let sent = result.unwrap(host.send_pair(mut cw, string.as_bytes("a"), string.as_bytes("bc"), Never))
    let mut input: Bytes = list.with_capacity(3)
    let first = result.unwrap(host.recv_bounded(mut sr, mut input, 2, At(c, 0)))
    let tail = result.unwrap(host.recv_bounded(mut sr, mut input, 1, At(c, 0)))
    let address_ok = match peer.ip { V4(2130706433u32) => 1u64, _ => 0u64 }
    let mut empty: Bytes = list.with_capacity(1)
    let timed_out = match host.recv_bounded(mut sr, mut empty, 1, At(c, 0)) { Error(TimedOut) => 1u64, _ => 0u64 }
    release_write(take cw)
    let ended = result.unwrap(host.recv_bounded(mut sr, mut empty, 1, Never))
    if local_address.port != 0 && peer.port != 0 && sent == 3 && first == 2 && tail == 1 && input[0] == 97u8 && input[2] == 99u8 && ended == 0 {
        40 + address_ok + timed_out
    } else { 0 }
}
"#;

pub const BINDINGS: &str = r#"
#include "hirsel_core_rt_host.h"
#include <stdlib.h>
static HirselCoreOsProcess *wire_process;
static void wire_free(void) { hirsel_core_os_process_free(wire_process); }
static const HirselCoreProcessConfig *wire_config(void) {
    if (!wire_process) {
        HirselCoreNetRule rules[] = {
            {.dir=0,.family=0,.lo=0x7f000000,.bits=8,.first=1,.last=65535},
            {.dir=1,.family=0,.lo=0x7f000000,.bits=8,.first=0,.last=65535}
        };
        HirselCoreOsConfig cfg={.root_limit=1048576,.grant_mask=6,.stdout_fd=1,.stderr_fd=2,.net_rules=rules,.net_rules_len=2,.target={(const uint8_t*)"wire",4}};
        HirselCoreOpenProcess p=hirsel_core_os_process_new(&cfg);
        assert(p.process); wire_process=p.process; atexit(wire_free);
    }
    return hirsel_core_os_process_config(wire_process);
}
"#;

