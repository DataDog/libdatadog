// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern "C" {
#include <datadog/common.h>
}
#include <atomic>
#include <cassert>
#include <cstdio>
#include <memory>
#include <thread>
#include <vector>

struct Sample {
  int x;
  int y;
};

void delete_fn(void *sample) { delete (Sample *)sample; }

struct Deleter {
  void operator()(ddog_ArrayQueue *object) { ddog_array_queue_drop(object); }
};

void print_error(const char *s, const ddog_Error &err) {
  ddog_CharSlice charslice = ddog_Error_message(&err);
  printf("%s (%.*s)\n", s, static_cast<int>(charslice.len), charslice.ptr);
}

int main(void) {
  ddog_ArrayQueue_NewResult array_queue_new_result = ddog_array_queue_new(5, delete_fn);
  if (array_queue_new_result.tag != DDOG_ARRAY_QUEUE_NEW_RESULT_OK) {
    print_error("Failed to create array queue", array_queue_new_result.err);
    ddog_Error_drop(&array_queue_new_result.err);
    return 1;
  }
  std::unique_ptr<ddog_ArrayQueue, Deleter> array_queue(&array_queue_new_result.ok);

  size_t num_threads = 4;
  size_t num_elements = 50;
  std::vector<std::atomic<size_t>> counts(num_elements);
  for (size_t i = 0; i < num_elements; ++i) {
    counts[i].store(0);
  }

  auto consumer = [&array_queue, &counts, num_elements]() {
    for (size_t i = 0; i < num_elements; ++i) {
      while (true) {
        ddog_ArrayQueue_PopResult pop_result = ddog_array_queue_pop(array_queue.get());
        if (pop_result.tag == DDOG_ARRAY_QUEUE_POP_RESULT_OK) {
          Sample *sample = (Sample *)pop_result.ok;
          counts[sample->x].fetch_add(1, std::memory_order_seq_cst);
          delete sample;
          break;
        } else if (pop_result.tag == DDOG_ARRAY_QUEUE_POP_RESULT_EMPTY) {
          std::this_thread::yield();
        } else {
          print_error("Failed to pop from array queue", pop_result.err);
          ddog_Error_drop(&pop_result.err);
          return;
        }
      }
    }
  };

  auto producer = [&array_queue, num_elements]() {
    for (size_t i = 0; i < num_elements; ++i) {
      Sample *sample = new Sample();
      sample->x = i;
      sample->y = i;
      while (true) {
        ddog_ArrayQueue_PushResult push_result = ddog_array_queue_push(array_queue.get(), sample);
        if (push_result.tag == DDOG_ARRAY_QUEUE_PUSH_RESULT_OK) {
          break;
        } else if (push_result.tag == DDOG_ARRAY_QUEUE_PUSH_RESULT_FULL) {
          std::this_thread::yield();
        } else {
          print_error("Failed to push to array queue", push_result.err);
          ddog_Error_drop(&push_result.err);
          delete sample;
          return;
        }
      }
    }
  };

  std::vector<std::thread> threads;
  for (size_t i = 0; i < num_threads; ++i) {
    threads.emplace_back(consumer);
    threads.emplace_back(producer);
  }

  for (auto &t : threads) {
    t.join();
  }

  for (const auto &c : counts) {
    assert(c.load(std::memory_order_seq_cst) == num_threads);
  }
}
