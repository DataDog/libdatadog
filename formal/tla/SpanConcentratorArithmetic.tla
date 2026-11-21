\* Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
\* SPDX-License-Identifier: Apache-2.0

---- MODULE SpanConcentratorArithmetic ----
EXTENDS Integers, Naturals

CONSTANTS
    UIntCardinality,
    SignedMax,
    BucketInputNs,
    NowNs,
    BufferLen,
    FlushBucketNs,
    SpanStart,
    SpanDuration

VARIABLE phase

vars == <<phase>>

NarrowUInt(value) ==
    value - (value \div UIntCardinality) * UIntCardinality

BucketStoredNs == NarrowUInt(BucketInputNs)

RetainedWindowNs == (BufferLen - 1) * FlushBucketNs

SpanEnd == SpanStart + SpanDuration
SignedMin == -SignedMax - 1

Init == phase = "check"
Next == UNCHANGED vars
Spec == Init /\ [][Next]_vars

NonzeroBucketRemainsNonzero ==
    BucketInputNs > 0 => BucketStoredNs > 0

FlushThresholdIsRepresentable ==
    NowNs >= RetainedWindowNs

SpanEndFitsSignedWord ==
    /\ SignedMin <= SpanEnd
    /\ SpanEnd <= SignedMax

====
