# RFC 0004: Mitigate hangs in the Crashtracker

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
However, since the process was crashing anyway, it does not affect the overall availability of the service.
A hang is worse, because it can prevent the process/pod from being restarted for a significant amount of time, potentially affecting the availability of the entire service.
This RFC proposes an architectural mechanism to mitigate the risk of crashtracker hangs.

## Proposed Architecture

In the proposed architecture, the crashing process [clones](https://man7.org/linux/man-pages/man2/clone.2.html) a child process.
Care should be taken during this process to avoid
This child process (referred to as the **collector** in this RFC) is responsible for collecting the data required for crashtracking, while the parent operates as a watchdog (and is hence referred to as the **watchdog** process in the RFC).
The **receiver** process remains as in RFC 3, either running as an [execve](https://man7.org/linux/man-pages/man2/execve.2.html) fork of the watchdog, or as an independent sidecar.
If the child process completes successfully, the parent process reaps its PID, waits for the receiver process to finish uploading, and then chains the next signal handler, as it does currently.
If the collector or the receiver exceed their timeout budget, the watchdog [kills](https://man7.org/linux/man-pages/man2/kill.2.html) the offending child process, and then chains the next signal handler.

### Timeouts

Each component has a total timeout budget, which it must enforce.
Since each component conducts a series of operations, the total budget will typically be subdivided among those operations, in a hierarchical fashion.
This increases the probability that some operations will succeed, maximizing the total amount of actionable data that makes it to the backend.
The values for these timeout budgets SHOULD be configurable, either through an explicit configuration object, or through documented environment variables.
This RFC is probably the best place to document such variables: implementers who add a configuration variable SHOULD make an amendment to this RFC to document it,

#### Watchdog

The watchdog is the fundamental defence against process hangs in this scheme.
The recommended default overall timeout budget is ?.

It MUST ensure that the crashing process cleanly calls the next signal-handler/default/abort when the overall crashtracker timeout budget is exceeded.
The watchdog SHOULD make a best effort attempt to cleanup its child processes before exiting.
It is RECOMMENDED that some portion of the timeout budget be reserved for this purpose.
The watchdog MAY maintain separate timeout budgets for the collector and receiver, or MAY have a single budget shared by both.

#### Collector

As a defence in depth mechanism, the collector SHOULD maintain its own timeout budget, and cleanly exit if that budget is exceeded.
The recommended default overall timeout budget is ?.

It is RECOMMENDED that the collector attempt to limit the time spent in any one operation, in order to ensure that as much data as possible overall is collected.
For example, if the collector stalls while reading/transmitting `/proc/self/maps`, then no subsequent data will be collected.
System calls SHOULD, when possible, take advantage of OS level timeout mechanisms.
Loops SHOULD track their timeout budget at every iteration, and exit when the budget it exceeded.
The collector SHOULD proactively flush its output channel to increase the probability that data will be received even if the collector is terminated.
TODO: list of operations here, with recommended defaults.

#### Receiver

As a defence in depth mechanism, the receiver SHOULD maintain its own timeout budget, and cleanly exit if that budget is exceeded.
The recommended default overall timeout budget is ?.

It is RECOMMENDED that the receiver attempt to limit the time spent in any one operation, in order to ensure that as much data as possible overall is collected.
In particular, the expensive operations undertaken by the receiver are:

1. Receive the crash report from the collector.
   Recommended default: ?
2. Collect additional system info.
   Recommended default: ?
3. Resolve debug symbols, if requested.
   Recommended default: ?
4. Upload the crash-report to the endpoint.
   Recommended default: ?

The receiver SHOULD maintain separate timeouts for each of these operations, to maximize the probability that some useful message will successfully reach the backend in the overall timeout budget available to the receiver.

If the backend supports it, the receiver MAY choose to send a partial crash-report or metric at checkpoints during this process.
