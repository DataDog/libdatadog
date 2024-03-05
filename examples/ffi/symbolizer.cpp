// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#include <iostream>
#include <vector>

#include <unistd.h>
#include <cassert>

#include "datadog/blazesym.h"

#define _THIS_IP_                                                              \
  ({                                                                           \
    __label__ __here;                                                          \
  __here:                                                                      \
    (unsigned long)&&__here;                                                   \
  })

void symbolize_and_print_abs(blaze_symbolizer* symbolizer, uintptr_t addr) {
    std::vector<uintptr_t> addrs = {addr};
    
    blaze_symbolize_src_process src = {
      .type_size = sizeof(blaze_symbolize_src_process),
      .pid = static_cast<uint32_t>(getpid()),
      .debug_syms = false,
      .perf_map = false,
      .map_files = false,
      .reserved = {},
    };
    const blaze_result* results = blaze_symbolize_process_abs_addrs(
        symbolizer, &src, addrs.data(), addrs.size());
    assert(results);
    bool found = false;
    for (size_t i = 0; i < addrs.size(); ++i) {
        std::cout << "Address: " << addrs[i] << ", Symbolized: " << results->syms[i].name << std::endl;
        if (std::string(results->syms[i].name).find("test_symbolizer") != std::string::npos){
            found = true;
        }
    }
    assert(found);
    // Free the results
    blaze_result_free(results);
}

void test_symbolizer() {
    blaze_symbolizer* symbolizer = blaze_symbolizer_new();
    uintptr_t ip = _THIS_IP_;
    symbolize_and_print_abs(symbolizer, ip);
    blaze_symbolizer_free(symbolizer);
}

int main() {
    test_symbolizer();
    return 0;
}
