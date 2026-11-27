#pragma once

#include <mach/mach.h>
#include <stdint.h>

typedef struct {
    int32_t status;
    uint64_t resident;
    uint64_t virtual_size;
} hirsel_gate_memory;

static inline hirsel_gate_memory hirsel_gate_memory_sample(void) {
    mach_task_basic_info_data_t info = {0};
    mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
    int status = task_info(mach_task_self(), MACH_TASK_BASIC_INFO, (task_info_t)&info, &count);
    return (hirsel_gate_memory){status, info.resident_size, info.virtual_size};
}
