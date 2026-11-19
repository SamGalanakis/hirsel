/* L3's native count (RUNNER-SPEC v2.6): linked through
 * --wrap=hirsel_core_os_run_with_sources into the test executable of one case, it counts
 * CONC-10 TRY_SEND events (the case's body, once per run, and the publisher, once per
 * answer it tells, and the owner retirement wake) and fails the run (status 105) unless there are exactly TRY_SENDS. */
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_tasks.h"
#include "hirsel_core_rt_sites.h"
#include <string.h>
#include <stdio.h>
#ifndef TRY_SENDS
#error "define TRY_SENDS"
#endif
// The socket owner and ingress add their own TRY_SENDs. Keep the original
// body/publisher/compute-marker observation by attributing their tasks and the
// marker's channel from native source sites. Count the lease wake on the head inbox.
static const HirselCoreSourceTable *source_table;
static const char source_dir[] = SOURCE_MANIFEST_DIR "/";
static uint64_t head_task, publisher_task, retiring_task;
static int publisher_spawn, observed, marker_send, retirement_send;
static HirselCoreChannel *marker_channel, *head_channel;
#if defined(READY_BEFORE_ACCEPT) || defined(LAG_ADMISSION)
static int place_spawn, permit_create, places, publisher_live;
static int first_permits, second_permits, place_permits, premature_accept;
static uint64_t place_task, status_root, phases, permit_id;
#endif
static int item_named(uint64_t site, const char *name) {
    uint64_t ordinal = site >> 32;
    if (!source_table || !ordinal || ordinal > source_table->items_len) return 0;
    HirselCoreSourceItem item = source_table->items[ordinal - 1];
    if (item.file >= source_table->files_len) return 0;
    // Compact positions retain byte coordinates but omit declaration text.
    // Source names are relative to the fixture manifest.
    HirselCoreSourceView path = source_table->files[item.file].path;
    if (!path.len) return 0;
    // Views are byte ranges from the start of the files allocation.
    const char *path_data = (const char *)source_table->files + path.offset;
    size_t prefix_len = path_data[0] == '/' ? 0 : sizeof(source_dir) - 1;
    char filename[prefix_len + path.len + 1];
    memcpy(filename, source_dir, prefix_len);
    memcpy(filename + prefix_len, path_data, path.len);
    filename[prefix_len + path.len] = '\0';
    FILE *file = fopen(filename, "rb");
    if (!file) return 0;
    enum { PREFIX = 32 };
    size_t n = strlen(name);
    char source[PREFIX + 3 + n];
    size_t len = 0;
    if (fseek(file, item.file_offset, SEEK_SET) == 0) {
        len = fread(source, 1, sizeof(source), file);
    }
    fclose(file);
    for (size_t i = 0; i + 3 < len && i < PREFIX; i++) {
        if (!memcmp(source + i, "fn ", 3)) {
            size_t at = i + 3;
            return at + n < len && !memcmp(source + at, name, n)
                && (source[at+n] == '(' || source[at+n] == '[');
        }
    }
    return 0;
}
extern HirselCoreSpawn __real_hirsel_core_spawn(HirselCoreContext *, uint64_t, uint64_t, void *, HirselCoreTaskBody, HirselCoreTaskLayout, uint64_t);
HirselCoreSpawn __wrap_hirsel_core_spawn(HirselCoreContext *ctx, uint64_t tasks, uint64_t bytes, void *input, HirselCoreTaskBody body, HirselCoreTaskLayout layout, uint64_t site) {
    publisher_spawn = item_named(site, "start_publisher");
#if defined(READY_BEFORE_ACCEPT) || defined(LAG_ADMISSION)
    place_spawn = item_named(site, "start_managed");
#endif
    HirselCoreSpawn out = __real_hirsel_core_spawn(ctx, tasks, bytes, input, body, layout, site);
    publisher_spawn = 0;
#if defined(READY_BEFORE_ACCEPT) || defined(LAG_ADMISSION)
    place_spawn = 0;
#endif
    return out;
}
extern HirselCoreChannelNew __real_hirsel_core_channel_new(HirselCoreContext *, uint64_t, HirselCoreValueLayout, uint64_t);
HirselCoreChannelNew __wrap_hirsel_core_channel_new(HirselCoreContext *ctx, uint64_t capacity, HirselCoreValueLayout layout, uint64_t site) {
#if defined(READY_BEFORE_ACCEPT) || defined(LAG_ADMISSION)
    permit_create = item_named(site, "serve_sockets");
#endif
    HirselCoreChannelNew out = __real_hirsel_core_channel_new(ctx, capacity, layout, site);
    if (item_named(site, "answer")) marker_channel = out.sender;
    if (!head_channel && item_named(site, "served_with_handles")) head_channel = out.sender;
#if defined(READY_BEFORE_ACCEPT) || defined(LAG_ADMISSION)
    permit_create = 0;
#endif
    return out;
}
extern uint8_t __real_hirsel_core_try_send(HirselCoreChannel *, void *);
uint8_t __wrap_hirsel_core_try_send(HirselCoreChannel *channel, void *input) {
    marker_send = channel == marker_channel && marker_channel != NULL;
    if (marker_send) marker_channel = NULL;
    retirement_send = channel == head_channel && head_channel != NULL;
    uint8_t result = __real_hirsel_core_try_send(channel, input);
    marker_send = 0;
    retirement_send = 0;
    return result;
}
static uint64_t try_sends;
#ifdef HUNG_BODY_SENDS
static struct { uint64_t object, task, count, first; int marker, retirement; } senders[4];
static size_t sender_count;
static uint64_t slice_task, slices, slice_at;
static int too_many_senders;
#endif
static void event(void *state, HirselCoreEvent e) {
    (void)state;
#if defined(READY_BEFORE_ACCEPT) || defined(LAG_ADMISSION)
    if (e.kind == HIRSEL_CORE_EVENT_SPAWN && place_spawn) {
        places++;
        place_task = e.object;
        phases = 0;
        permit_id = 0;
        place_permits = 0;
    }
    if (e.kind == HIRSEL_CORE_EVENT_LATEST_PUBLISH && e.task == place_task) {
        if (phases == 0) status_root = e.object;
        if (e.object == status_root) phases++;
    }
    if (e.kind == HIRSEL_CORE_EVENT_SENDERS && permit_create) permit_id = e.object;
    if (e.kind == HIRSEL_CORE_EVENT_TRY_SEND && e.value == 0 && permit_id && e.object == permit_id) {
        if (place_permits++ == 0 && phases != 5) premature_accept++;
        if (places == 1) first_permits++;
        if (places == 2) second_permits++;
    }
    if (e.kind == HIRSEL_CORE_EVENT_SPAWN && publisher_spawn) publisher_live = 1;
    if (e.kind == HIRSEL_CORE_EVENT_END && e.object == publisher_task) publisher_live = 0;
#endif
    if (e.kind == HIRSEL_CORE_EVENT_SPAWN && publisher_spawn) {
        publisher_task = e.object;
        head_task = e.task;
        observed = 1;
    }
    // The owner publishes its closed lease before offering a wake on the head inbox.
    if (e.kind == HIRSEL_CORE_EVENT_LATEST_PUBLISH && observed
        && e.task != head_task && e.task != publisher_task) {
        retiring_task = e.task;
    }
    if (e.kind == HIRSEL_CORE_EVENT_TRY_SEND && observed
        && (e.task == head_task || e.task == publisher_task || marker_send
            || (retirement_send && e.task == retiring_task))) {
        try_sends++;
#ifdef HUNG_BODY_SENDS
        size_t i = 0;
        while (i < sender_count && senders[i].object != e.object) i++;
        if (i == 4) { too_many_senders = 1; return; }
        if (i == sender_count) {
            sender_count++;
            senders[i].object = e.object;
            senders[i].task = e.task;
            senders[i].first = try_sends;
            senders[i].marker = marker_send;
            senders[i].retirement = retirement_send && e.task == retiring_task;
        }
        senders[i].count++;
#endif
    }
#ifdef HUNG_BODY_SENDS
    if (e.kind == HIRSEL_CORE_EVENT_STEP_SLICE) {
        slice_task = e.task;
        slice_at = try_sends;
        slices++;
    }
#endif
}
#ifdef HUNG_BODY_SENDS
extern uint8_t hirsel_core_test_run_last(const HirselCoreProcessConfig *, HirselCoreEntry);
#endif
uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *input,
    const HirselCoreView *names, const HirselCoreSourceTable *sources, HirselCoreEntry entry) {
    (void)names;
    source_table = sources;
    HirselCoreOpenProcess open = hirsel_core_os_process_new(input);
    if (!open.process) return HIRSEL_CORE_NOT_STARTED;
    hirsel_core_os_process_set_sources(open.process, sources);
    HirselCoreProcessConfig cfg = *hirsel_core_os_process_config(open.process);
    cfg.scheduler.event = event;
#ifdef HUNG_BODY_SENDS
    uint8_t status = hirsel_core_test_run_last(&cfg, entry);
#else
    uint8_t status = hirsel_core_run(&cfg, entry);
#endif
    hirsel_core_os_process_free(open.process);
#ifdef READY_BEFORE_ACCEPT
    if (status) return status;
    if (places != 3 || publisher_live) {
        fprintf(stderr, "ready: incomplete place cleanup (%d starts, %d live publishers)\n", places, publisher_live);
        return 105;
    }
    if (first_permits != 1 || second_permits != 0 || place_permits != 0 || premature_accept) {
        fprintf(stderr, "ready: accept enabled %d times, %d outside Ready\n", first_permits, premature_accept);
        return 105;
    }
    return 0;
#elif defined(LAG_ADMISSION)
    if (status) return status;
    if (places != 2 || first_permits != 2 || second_permits > 1) {
        fprintf(stderr, "lag: expected two accept permits in the lag case and at most one in the clipped case; saw %d and %d\n", first_permits, second_permits);
        return 105;
    }
    return 0;
#else
    dprintf(2, "runner_count: try_send %llu, want %llu\n", (unsigned long long)try_sends,
            (unsigned long long)TRY_SENDS);
    if (status) return status;
#ifdef HUNG_BODY_SENDS
    if (try_sends != TRY_SENDS || too_many_senders || sender_count != 4 || slices != 1) {
        for (size_t k = 0; k < sender_count; k++) fprintf(stderr, "runner_count: object %llu task %llu sends %llu\n", (unsigned long long)senders[k].object, (unsigned long long)senders[k].task, (unsigned long long)senders[k].count);
        return 105;
    }
    /* RUNNER-SPEC §8, L6+L7 orders the compute before the hung call's stop.
     * MEM-30 permits the compute to run at the slice end. */
    size_t body = 4, marker = 4, told = 4, retirement = 4;
    for (size_t k = 0; k < 4; k++) {
        if (senders[k].count == HUNG_BODY_SENDS) body = k;
        else if (senders[k].count == 1 && senders[k].marker) marker = k;
        else if (senders[k].count == 1 && senders[k].retirement) retirement = k;
        else if (senders[k].count == HUNG_TOLDS && senders[k].task == publisher_task) told = k;
    }
    if (body == 4 || marker == 4 || told == 4 || retirement == 4 ||
        senders[body].task != slice_task || senders[marker].task == slice_task ||
        senders[marker].first >= senders[told].first ||
        slice_at >= senders[told].first || senders[told].first >= senders[retirement].first) {
        fprintf(stderr, "runner_count: body=%zu marker=%zu told=%zu retirement=%zu slice_task=%llu\n",
                body, marker, told, retirement, (unsigned long long)slice_task);
        return 105;
    }
#endif
    return try_sends == TRY_SENDS ? 0 : 105;
#endif
}
