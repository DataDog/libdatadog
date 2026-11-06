\* Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
\* SPDX-License-Identifier: Apache-2.0

---- MODULE AtomicMultisetLifecycle ----
EXTENDS Naturals

CONSTANT Capacity
VARIABLES used, occupied

vars == <<used, occupied>>

Init ==
    /\ used = 0
    /\ occupied = 0

Insert ==
    /\ used < Capacity
    /\ used' = used + 1
    /\ occupied' = occupied + 1

Consume ==
    /\ occupied > 0
    /\ occupied' = 0
    /\ used' = used

Next == Insert \/ Consume
Spec == Init /\ [][Next]_vars

AccountingConsistent == used = occupied

====
