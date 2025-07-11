// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/ddsketch.h>
#include <stdio.h>
#include <stdlib.h>

#define TRY(expr)                                                                                  \
  {                                                                                                \
    struct DDSketchError *err = expr;                                                             \
    if (err != NULL) {                                                                            \
      const char *message = err->msg.ptr;                                                         \
      fprintf(stderr, "ERROR: %s\n", message);                                                   \
      ddog_ddsketch_error_free(err);                                                              \
      return 1;                                                                                   \
    }                                                                                             \
  }

int main(void) {
  // Create a new DDSketch
  ddog_DDSketch *sketch = NULL;
  TRY(ddog_ddsketch_new(&sketch));

  printf("Created DDSketch successfully\n");

  // Add some sample data points
  printf("Adding sample data points...\n");
  TRY(ddog_ddsketch_add(sketch, 1.0));
  TRY(ddog_ddsketch_add(sketch, 2.5));
  TRY(ddog_ddsketch_add(sketch, 5.0));
  TRY(ddog_ddsketch_add(sketch, 10.0));
  TRY(ddog_ddsketch_add(sketch, 15.0));

  // Add points with specific counts
  printf("Adding points with specific counts...\n");
  TRY(ddog_ddsketch_add_with_count(sketch, 3.0, 5.0));  // Add 3.0 with count 5
  TRY(ddog_ddsketch_add_with_count(sketch, 7.0, 3.0));  // Add 7.0 with count 3

  // Get the total count
  double count = ddog_ddsketch_count(sketch);
  printf("Total count in sketch: %.0f\n", count);

  // Get the ordered bins (buckets)
  printf("Getting ordered bins...\n");
  ddog_Vec_DDSketchBin bins = ddog_ddsketch_ordered_bins(sketch);
  
  printf("Number of bins: %zu\n", bins.len);
  for (size_t i = 0; i < bins.len; i++) {
    printf("  Bin %zu: value=%.2f, weight=%.0f\n", i, bins.ptr[i].value, bins.ptr[i].weight);
  }

  // Clean up bins
  ddog_ddsketch_bins_drop(bins);

  // Encode the sketch to protobuf format
  printf("Encoding sketch to protobuf...\n");
  struct ddog_Vec_u8 encoded = ddog_ddsketch_encode(sketch);
  
  printf("Encoded sketch size: %zu bytes\n", encoded.len);
  
  // Print first few bytes of encoded data (for demonstration)
  printf("First 10 bytes of encoded data: ");
  for (size_t i = 0; i < (encoded.len < 10 ? encoded.len : 10); i++) {
    printf("%02x ", encoded.ptr[i]);
  }
  printf("\n");

  // Clean up the sketch (note: sketch is consumed by ddog_ddsketch_encode)
  // ddog_ddsketch_drop is not called here because the sketch was consumed

  printf("DDSketch example completed successfully!\n");
  return 0;
}
