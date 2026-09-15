// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#pragma once

#include "libdd-profiling/src/cxx.rs.h"

#include <array>
#include <cstddef>
#include <span>
#include <vector>

namespace datadog::profiling::views {

template <typename T>
rust::Slice<const T> slice(const std::vector<T>& values) noexcept {
    return {values.data(), values.size()};
}

template <typename T>
rust::Slice<const T> slice(const rust::Vec<T>& values) noexcept {
    return {values.data(), values.size()};
}

template <typename T, std::size_t N>
rust::Slice<const T> slice(const std::array<T, N>& values) noexcept {
    return {values.data(), values.size()};
}

template <typename T, std::size_t N>
rust::Slice<const T> slice(const T (&values)[N]) noexcept {
    return {values, N};
}

template <typename T>
rust::Slice<const T> slice(std::span<T> values) noexcept {
    return {values.data(), values.size()};
}

template <typename T>
rust::Slice<const T> slice(std::span<const T> values) noexcept {
    return {values.data(), values.size()};
}

template <typename Locations, typename Values, typename Labels>
Sample sample(const Locations& locations, const Values& values, const Labels& labels) noexcept {
    return Sample{
        .locations = slice(locations),
        .values = slice(values),
        .labels = slice(labels),
    };
}

template <typename Locations, typename Values, typename Labels>
DictionarySample dictionary_sample(
    const Locations& locations,
    const Values& values,
    const Labels& labels) noexcept {
    return DictionarySample{
        .locations = slice(locations),
        .values = slice(values),
        .labels = slice(labels),
    };
}

}  // namespace datadog::profiling::views
