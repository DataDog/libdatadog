// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <iostream>
#include <string>

int cpp_function() {
    std::cout << "Hello world" << std::endl;
    return 0;
}

// For normalization/symbolization tests
// do not use template class, depending on the compiler the mangled names
// won't be the same and the test will fail
namespace MyNamespace {
    class ClassInNamespace {
    public:
        void MethodInNamespace1(float, long long);
        static void MethodInNamespace2(double, char);

        class InnerClassInNamespace {
        public:
            void InnerMethod1();
            static void InnerMethod2(int, int, long long);
        };
    };

    void ClassInNamespace::MethodInNamespace1(float, long long) {
        std::cout << "ClassInNamespace::MethodInNamespace1 called" << std::endl;
    }

    void ClassInNamespace::MethodInNamespace2(double, char) {
        std::cout << "ClassInNamespace::MethodInNamespace2 called" << std::endl;
    }

    void ClassInNamespace::InnerClassInNamespace::InnerMethod1() {
        std::cout << "ClassInNamespace::InnerClassInNamespace::InnerMethod1 called" << std::endl;
    }

    void ClassInNamespace::InnerClassInNamespace::InnerMethod2(int, int, long long) {
        std::cout << "ClassInNamespace::InnerClassInNamespace::InnerMethod2 called" << std::endl;
    }
}

class FirstClass {
public:
    void Method1();
    static void Method2(int, bool, std::string);

    class InnerClass {
    public:
        void InnerMethod1();
        static void InnerMethod2();
    };
};

void FirstClass::Method1() {
    std::cout << "FirstClass::Method1 called" << std::endl;
}

void FirstClass::Method2(int, bool, std::string) {
    std::cout << "FirstClass::Method2 called" << std::endl;
}

void FirstClass::InnerClass::InnerMethod1() {
    std::cout << "FirstClass::InnerClass" << std::endl;
}

void FirstClass::InnerClass::InnerMethod2() {
    std::cout << "FirstClass::InnerClass::InnerMethod2 called" << std::endl;
}
