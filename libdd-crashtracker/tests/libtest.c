// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <stdio.h>

int my_function(void) {
    return 42;
}

// The following code helps in generating a case where blazesym failed at
// opening the file and we were failing at normalizing/symbolizing the
// address.
// The goal is to increase the size of the debug_XX sections and make sure
// that at least one of them will be compressed (and not aligned)
#define MAKE_FUNC(N) \
    void func##N(void) { \
        int arr[100]; \
        for (int i = 0; i < 100; i++) { \
            arr[i] = i * N; \
        } \
        printf("Function %d called, value = %d\n", N, arr[99]); \
    }

#define MAKE_STRUCT(N) \
    struct Struct##N { \
        int a[50]; \
        double b[50]; \
        char c[100]; \
    };

MAKE_STRUCT(1)
MAKE_STRUCT(2)
MAKE_STRUCT(3)
MAKE_STRUCT(4)
MAKE_STRUCT(5)
MAKE_STRUCT(6)
MAKE_STRUCT(7)
MAKE_STRUCT(8)
MAKE_STRUCT(9)
MAKE_STRUCT(10)

MAKE_FUNC(1)
MAKE_FUNC(2)
MAKE_FUNC(3)
MAKE_FUNC(4)
MAKE_FUNC(5)
MAKE_FUNC(6)
MAKE_FUNC(7)
MAKE_FUNC(8)
MAKE_FUNC(9)
MAKE_FUNC(10)

int main(void) {
    printf("Starting main\n");
    func1();
    func2();
    func3();
    func4();
    func5();
    func6();
    func7();
    func8();
    func9();
    func10();
    return 0;
}
