# RFC 0004: Mitigate hangs and crashes in the Crashtracker

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT", "SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this document are to be interpreted as described in [IETF RFC 2119](https://datatracker.ietf.org/doc/html/rfc2119).

## Background

As documented in [RFC 3: Crashtracker design](https://github.com/DataDog/libdatadog/blob/main/docs/RFCs/0003-crashtracker-design.md#design-priorities), the crashtracker has the following design priorities:

1.  When the system is functioning normally, the crashtracker should, to the greatest extent possible, **have no impact on the functioning of a non-crashing system**.
2.  When a crash does occur, the crashtracker should **prioritize the reliable collection and reporting of crash information**.
3.  When a crash does occur, the crashtracker should **make a best-effort attempt to minimize impact** on the crashing process.

In the [current architecture](https://github.com/DataDog/libdatadog/blob/main/docs/RFCs/0003-crashtracker-design.md#architectural-model), the crashtracker collector operates within the context of a crashing process.
As a baseline requirement, crashtracker code should aim to be [signal safe](https://man7.org/linux/man-pages/man7/signal-safety.7.html).
This is difficult, since some operations (e.g. stack walking), may do non signal safe operations such as `malloc`.
Even if the code does avoid all non-signal-safe operations, the program itself may be in such a corrupted state that the crashtracker will itself crash or hang.

In the context of a crashing customer process, a crash within the crashtracker is bad because it can disrupt existing debugging workflows.
This could be a frustrating and impactful situation for end-users. Imagine that you have a crashing application and it turns out that it's due to a dependency, but you have absolutely no means by which to deduce this fact. You might waste an extreme amount of engineering time trying to figure out why it's crashing.
Moreover, if we crash, then we're not getting a crash report, so our ability to intervene on the behalf of such a user is limited.
However, since the process was crashing anyway, it does not affect the overall availability of the service.

A hang is even worse, because it can prevent the process/pod from being restarted for a significant amount of time, potentially affecting the availability of the entire service.
This RFC proposes an architectural mechanism to mitigate the risk of crashtracker hangs.

## Proposed Architecture

The proposed architecture consists of three processes.
By splitting dangerous operations out across a process boundary, the blast-radius of both crashes and hangs can be limited.

1. The crashing process, referred to as the **watchdog** process in this RFC.
   The purpose of the watchdog is to split dangerous operations off into child processes, monitor those processes, recover from any failures, and then cleanly chain any preexisting signal handlers.
   The watchdog SHOULD carefully limit the operations it performs in process to maximize the chance that it will succeed even if the state of the crashing process is corrupted.
   The watchdog has the following tasks:
   1. [fork](https://man7.org/linux/man-pages/man2/fork.2.html) a child process to collect the (referred to as the **collector** in this RFC).
      The watchdog MUST NOT perform non-signal-safe operations such as `malloc` and `at_fork` handlers during this step.
      There are [several potential system calls](https://github.com/DataDog/libdatadog/pull/716#discussion_r1832076807) that could be used here, including `clone`, `fork`, `vfork`, etc.
      The key requirement is that whichever operation is used, it MUST maintain the information required by the collector to report a crash.
   2. Spawn (`fork/exec`) a receiver process if necessary (unless there is already a long-lived receiver side-process).
   3. Monitor and cleanup the child processes
      - If the child processes (collector and receiver) complete successfully, the watchdog process reaps their PIDs, and then chains the next signal handler, as it does currently.
      - If the collector or the receiver exceed their timeout budget, the watchdog [kills](https://man7.org/linux/man-pages/man2/kill.2.html) the offending child process, and then chains the next signal handler.
      - Similarly, if either the collector or receiver crash (), the watchdog cleans up its child processes, and then chains the next signal handler.
2. The **collector** child process is responsible for collecting the data required for crashtracking, and forwarding it to the receiver.
   Since it runs in a clone of the crashing process, it has access to all data necessary to do so.
   The collector SHOULD limit its use of non-signal-safe operations, but MAY use them when necessary (e.g. during stack unwinding).
   The collector SHOULD maximize the chance of getting at least a partial crash report by performing operations in ranked order of risk, leaving riskiest operations for last.
   Note that in this model, the collector process MUST NOT chain signal handlers when it finishes.
   Instead, it SHOULD simply `abort` on failure, and `exit(SUCCESS)` if it succeeds.
3. The **receiver** is responsible for receiving crash information from the collector, formatting it into a crash report, and forwarding it to the backend.
   The receiver SHOULD be written to be resilient even if the collector crashes or hangs, to increase the probability of getting at least a partial crash report out.

### Timeouts

Each component has a total timeout budget, which it must enforce.
Since each component conducts a series of operations, the total budget will typically be subdivided among those operations, in a hierarchical fashion.
This increases the probability that some operations will succeed, maximizing the total amount of actionable data that makes it to the backend.
The values for these timeout budgets SHOULD be configurable, either through an explicit configuration object, or through documented environment variables.
This RFC is probably the best place to document such variables: implementers who add a configuration variable SHOULD make an amendment to this RFC to document it.

#### Watchdog

The watchdog is the fundamental defence against process hangs in this scheme.
The recommended default overall timeout budget is 5 seconds.

It MUST ensure that the crashing process cleanly calls the next signal-handler/default/abort when the overall crashtracker timeout budget is exceeded.
The watchdog SHOULD make a best effort attempt to cleanup its child processes before exiting.
It is RECOMMENDED that some portion of the timeout budget be reserved for this purpose.
The watchdog MAY maintain separate timeout budgets for the collector and receiver, or MAY have a single budget shared by both.

#### Collector

As a defence in depth mechanism, the collector SHOULD maintain its own timeout budget, and cleanly exit if that budget is exceeded.
This timeout should leave sufficient room for the receiver to transmit to the backend without exceeding the overall timeout budget.
The recommended default overall timeout budget is 2 seconds.

It is RECOMMENDED that the collector attempt to limit the time spent in any one operation, in order to ensure that as much data as possible overall is collected.
For example, if the collector stalls while reading/transmitting `/proc/self/maps`, then no subsequent data will be collected.
System calls SHOULD, when possible, take advantage of OS level timeout mechanisms.
Loops SHOULD track their timeout budget at every iteration, and exit when the budget it exceeded.
The collector SHOULD proactively flush its output channel to increase the probability that data will be received even if the collector is terminated.

#### Receiver

As a defence in depth mechanism, the receiver SHOULD maintain its own timeout budget, and cleanly exit if that budget is exceeded.
It is RECOMMENDED that the receiver have the same timeout budget as the overall watchdog budget.
This means that if the receiver finishes early, the collector can continue and use all of the remaining budget.
Hence, the recommended default overall timeout budget is 5 seconds.

It is RECOMMENDED that the receiver attempt to limit the time spent in any one operation, in order to ensure that as much data as possible overall is collected.
In particular, the expensive operations undertaken by the receiver are:

1. Receive the crash report from the collector.
   This should be the same as the receiver timeout budget.
   Recommended default: 2 seconds.
2. Collect additional system info.
   This is of lower importance, and should have a lower timeout.
   Recommended default: 0.5 seconds
3. Resolve debug symbols, if requested.
   This is of lower importance, and should have a lower timeout.
   Recommended default: 0.5 seconds.
4. Upload the crash-report to the endpoint.
   Recommended default: 3

The receiver SHOULD maintain separate timeouts for each of these operations, to maximize the probability that some useful message will successfully reach the backend in the overall timeout budget available to the receiver.

### Heartbeat 

Knowing that a crash has occurred is valuable, even if we do not have full information.
Furthermore, it is useful to know when the crashtracker itself crashes or hangs.
We can get this information by having the crashtracker send a small heartbeat message as soon as it can.
The crashtracker SHOULD send such a message.

One possible design would be to have the receiver spawn a thread which uploads either a metric, or a small log line, as soon as the crash metadata has been received.
