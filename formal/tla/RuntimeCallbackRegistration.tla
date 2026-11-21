\* Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
\* SPDX-License-Identifier: Apache-2.0

---- MODULE RuntimeCallbackRegistration ----
EXTENDS TLC

VARIABLES current, readerPointer, oldAlive

vars == <<current, readerPointer, oldAlive>>

Init ==
    /\ current = "old"
    /\ readerPointer = "none"
    /\ oldAlive = TRUE

ReaderLoad ==
    /\ readerPointer = "none"
    /\ readerPointer' = current
    /\ UNCHANGED <<current, oldAlive>>

WriterReplace ==
    /\ current = "old"
    /\ current' = "new"
    /\ oldAlive' = FALSE
    /\ UNCHANGED readerPointer

Next == ReaderLoad \/ WriterReplace
Spec == Init /\ [][Next]_vars

ReaderPointerIsValid ==
    readerPointer # "old" \/ oldAlive

====
