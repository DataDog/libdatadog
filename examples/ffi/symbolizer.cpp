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


void normalize(blaze_symbolizer* symbolizer, uintptr_t addr) {
  blaze_normalizer *normalizer = blaze_normalizer_new();
  std::vector<uintptr_t> addrs = {addr};
  blaze_normalized_user_output *normalized_output = blaze_normalize_user_addrs(
                                                                normalizer,
                                                                getpid(),
                                                                addrs.data(),
                                                                addrs.size());
  assert(normalized_output);
  if (normalized_output) {
    for (size_t i = 0; i < addrs.size(); ++i) {
      std::cout << "Address: " << addrs[i] << ", Normalized: " << normalized_output->outputs << std::endl;
    }
    blaze_user_output_free(normalized_output);
  }
  blaze_normalizer_free(normalizer);
}

// Virtual only works as privileged user
// This is slightly surprising considering self maps should be readable
void symbolize_and_print_abs(blaze_symbolizer* symbolizer, uintptr_t addr) {
    std::vector<uintptr_t> addrs = {addr};
    
    blaze_symbolize_src_process src = {
      .type_size = sizeof(blaze_symbolize_src_process),
      .pid = static_cast<uint32_t>(getpid()),
      .debug_syms = false,
      .perf_map = false,
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
    normalize(symbolizer, ip);
    symbolize_and_print_abs(symbolizer, ip);
    blaze_symbolizer_free(symbolizer);
}

int main() {
    test_symbolizer();
    return 0;
}
