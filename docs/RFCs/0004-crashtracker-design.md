# RFC 0004: Crashtracker design

## Background

A crashtracker implementation must balance between two conflicting aims.
On the one hand, its purpose is to **reliably collect and report the information necessary to diagnose and debug all crashes** in the tracked process.
On the other hand, a crashtracker, like other instrumentation technology, should ideally **cause no observable difference in the execution of the processes where it is installed**.
These aims are in tension: design decisions which increase the quality and reliability of data-collection often increase the (potential) for impact on the customer system.

This document lays out the design priorities of the crashtracker, describes how the current architecture affects/implements those priorities, and then lays out a plan for future improvements to the crashtracking ecosystem.
It focuses on the design and implementation of the current (version 1) crashtracker: i.e., either features that are already implemented, or are scheduled to be implemented in the near future.
Additional improvements will be the subject for future RFCs.

## Design Priorities

1.  When the system is functioning normally, the crashtracker should, to the greatest extent possible, **have no impact on the functioning of a non-crashing system**.
2.  When a crash does occur, the crashtracker should **prioritize the reliable collection and reporting of crash information**.
3.  When a crash does occur, the crashtracker should **make a best-effort attempt to minimize impact** on the crashing process.

## Architectural model

### Collector

For all languages other than .Net and Java, this is a signal handler.
The collector interacts with the system in the following ways:

1.  Registers a signal handler (`sigaction`) to capture `sigsegv`, `sigbus` and (planned but not yet implemented) `sigabort`.
    The old handler is saved into a global variable to enable changing.
    - Risks to normal operation
      - Chaining handlers is not atomic.
        It is possible for concurrent races to occur.
        Mitigation: doing this operation once, early during system initialization.
      - If the system expects and handles as part of its ordinary functioning, this can violate principle 1.
        This is not common for most languages, but Java and .Net use signals for internal VM mechanisms (e.g. GC optimizations and the like).
        A crashtracker signal handler risks breaking these mechanisms.
        Mitigation: not using signal handler based crashtracking for these languages.
    - Risks during a crash
      - If user code installs signal handlers, these can conflict with the crashtracker.
        This can occur either intentionally by the user, or unexpectedly as the result of installing a framework which does so.
        Mitigation:
        If the user installs their handler after the crashtracker is initialized, then the crashtracker will likely not work unless the user was careful to chain signal handlers.
        There is no obvious mitigation to take in that case.
        There is no known tooling to reliably detect when a signal handler is replaced.
        If the user installs their handler before the crashtracker is initialized, then we make a best-effort attempt to chain their handler.
        Chaining handlers is not directly supported by POSIX, so a best-effort attempt is the best we can do.
2.  When a crash occurs, the signal handler is invoked.
    - Risks to normal operation
      - N/A
    - Risks during a crash
      - In some cases (e.g. stack overflow), invoking a signal handler can itself fail.
        Mitigation: support the use of `sigaltstack`
      - The signal handler could crash.
        This would prevent the chaining of signal handlers, and could interfere with the users existing debugging workflow.
        Mitigation:
        Operating inside a crashing process is inherently risky.
        We do our best to limit the operations we perform, and to move as much as possible across a process boundary, but there will always be a risk here.
        Provide an environment variable to enable customers to disable the crashtracker.
      - The signal handler could hang.
        This is potentially worse for the customer, since a crashed process will be restarted by the kubernetes controller, while a hanging process will remain alive until reaped.
        This consumes resources and reduces the ability of the customer application to handle load.
        Mitigation:
        Operating inside a crashing process is inherently risky.
        We do our best to limit the operations we perform, and to move as much as possible across a process boundary, but there will always be a risk here.
        Provide an environment variable to enable customers to disable the crashtracker.
        Implementing a foolproof timeout in a crash handler is difficult.
        However, we SHOULD provide checkpoints where we exit early if a user-specified amount of time has been exceeded.
3.  Backtrace collected.
    - Risks to normal operation
      - N/A
    - Risks during a crash
      - If the stack is corrupted, attempting to collect the backtrace can crash the crashing process.
        This prevents chaining the old signal handler and can break customer debugging workflows.
        Mitigation: provide an environment variable allowing customers to turn off backtrace collection
      - If the stack is corrupted, attempting to collect the backtrace can crash the crashing process.
        This will prevent the collection and reporting of any additional crash info.
        Mitigation:
        Collect the backtrace last, allowing less risky operations to complete first.
        As a stretch mitigation, we could do out-of-process unwinding (see below).
4.  Results written to a pipe (unix socket)
    - Risks to normal operation
      - Maintaining the open pipe requires a file-descriptor.
      - Maintaining a unix socket requires a synchronization point, typically a file in the file system.
    - Risks during a crash
      - If the pipe/socket is not drained, the collector will stall when attempting to write.
        This could hang the crashing process.
        Mitigation:
        Minimize the amount of data sent.
        Todo: allow the specification of a max file-size to upload.
        Todo: use nonblocking read/writes, and bail if there is an issue.
        Todo: increase the buffer size to mitigate this issue, as discussed [here](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819070556).
      - If the pipe/socket is not available or is closed early, data could not be sent, or the crashtracker could hang.
        Mitigation:
        Data is sent to the receiver in priority order.
        If the channel is closed, the receiver will send as much data as possible, as well as a flag indicating that an error occurred.
5.  Files collected from `/proc`
    - Risks to normal operation
      - N/A
    - Risks during a crash
      - Due to user security controls, these files may not be available to read.
        Mitigation: The crashtracker receiver will still transmit partial crash reports.
      - The files may be large.
        Mitigation:
        None currently.
        In the future, we could add a file size limit and only send the first `n` bytes.
      - These files could contain sensitive data
        Mitigation:
        only send a select set of files which do not contain sensitive data.
        At present, this is based on careful coding and audit of which files are sent.
        Stretch mitigation: redact potentially sensitive data either client-side or server-side (see below).
6.  Returns and chains the previous handlers, if any.
    - Risks to normal operation
      - N/A
    - Risks during a crash
      - Resulting core dumps are sometimes a bit corrupted / contain the crashtracker handler.
        This can be confusing when viewing the core dumps and registers are not quite what they were before the handler was invoked.
        Mitigation: Unset the crashtracker handler and return from the signal handler, so that the OS will fall back to the default action of creating a core dump ... at the original sigsegv location.
        Stretch: It may be possible to do magic using `sigsetjmp`/`siglongjmp`, as discussed [here](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819149232).

### Receiver

A signal handler is fundamentally limited in what operations it can perform (see [signal-safety(7) \- Linux manual page](https://man7.org/linux/man-pages/man7/signal-safety.7.html)).
This is made worse by the fact that the collector operates in the context of a crashing process whose state may be corrupted, causing even seemingly safe operations to crash/  
The receiver interacts with the system in the following ways:

1.  Forks a new process.
    This can either occur eagerly, at process initialization or lazily, as part of crash handling.
    Both options have issues.
    - Risks to normal operation
      - Eager initialization consumes a PID and a slot in the process table.
        This can cause either initialization to fail, or block the creation of another process on the customer system.
        Mitigation: This is [unlikely to occur](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819230509).
      - Eager initialization can lead to an additional child process.
        This was originally believed to be harmless.
        However, it turns out that some frameworks assume that they are the only thing that can spawn workers, and do a `waitpid` on all children.
        The existence of an additional child process hangs the framework.
        Mitigation: do lazy initilization.
        The possibility of using a sidecar or updated agent is discussed in future work below.
    - Risks during a crash
      - Lazy initialization consumes a PID and a slot in the process table.
        This can cause either initialization to fail, or block the creation of another process on the customer system.
        Mitigation: This is unlikely to occur.
      - Forking inside a signal handler is unsafe (see Notes on [signal-safety(7) \- Linux manual page](https://man7.org/linux/man-pages/man7/signal-safety.7.html)).
        Mitigation: Do cursed heroics to avoid triggering `at_fork` handlers.
2.  Listens for a crashreport a socket/pipe.

    - Risks to normal operation
      - May consume a small amount of resources.
        Mitigation: None.
    - Risks during a crash
      - If the pipe is not drained fast enough, the crashtracker collector will hang.
        Mitigation:
        Currently, none, other than careful coding to limit work done while draining the socket.
        In the future we might wish to play with process niceness to increase the chance of the receiver running and draining the socket.
        We can increase the size
      - The collector might crash or be terminated, truncating the message.
        Note that this is an issue for the receiver, because a partial (and potentially malformed) message will be received.
        The receiver must ensure that it does not hang or crash in this case, and transmit as much information as possible to the backend.
        Mitigation: If an unexpected input is received, including EOF, make a best effort attempt to format and send a partial crash report.

3.  Transmits the message to the backend.
    - Risks to normal operation
      - NA
    - Risks during a crash
      - The endpoint may be inaccessible.
        Mitigation: drop the report.
        As a future mitigation, if this is a common problem, explore posting a failure metric to a different endpoint, increasing the chance of some message getting out.
      - It may take a significant amount of time to transmit the crash report.
        This could cause the user process to hang.
        Mitigation: a configurable timeout on transmission (default 3s).

## Potential future improvements.

There are a number of potential which could improve the reliability of the crashtracker, at the cost of increased design and implementation complexity.
These are not in scope for the current document, but could be valuable improvements in followup RFCs.

### Out of process stack collection

Currently, we collect the stack within the signal handler in the crashing process.
This is inherently dangerous: the crashing process may have a corrupted stack, in which case stack walking itself may crash or hang the process.
Additionally, stack-walking routines are generally not guaranteed to be signal-safe.
They may allocate, touch code which uses mutexes, or other non signal-safe activities.
To minimize the effect of stack walking, the crashtracker support out of process _symbolization_ of stacks collected in process.
A next step would be to support out of process collection of the stack.

Options include:

- Using an existing out-of band crash-tracker such as ptrace, [discussed here](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819070836).
- Forking the process, and doing the stack walk in the fork, as [discussed here](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819322727).

### Injecting `sigaction` to enable more reliable handler chaining

POSIX lacks a proper mechanism to chain signal handlers.
We currently make a best-effort attempt to do so by storing the return value of `sigaction` in a global variable, and then chaining a call to the old handler at the end of the crashtracker handler.
This is a best effort attempt, which is not guaranteed to work.  
Conversely, if the user attempts to set a new signal handler after the crashtracker is registered, there is currently no notification of this fact to the crashtracker, and hence no way for the crashtracker to control what happens in this situation.
One proposal, discussed [here](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819265900) and [here](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819192232), is to inject our own wrapper for `sigaction`, giving the crashtracker full knowledge of what signal handlers are registered, and allowing it to programmatically take action.

### Eliminating the need for a receiver process

As discussed above, transmitting data from within a crashing process is difficult to do in a signal safe manner.
The current mitigation is to fork a separate receiver binary which gets data from the collector over a socket/pipe and then formats and transmits it to the endpoint.
Other options include

- Using a daemonized sidecar.
  This is supported for PHP, but [still has gotchas in a containerized environment where process 1 is non reaping](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819075313).
- [Adding support for this to the agent](https://github.com/DataDog/libdatadog/pull/696#discussion_r1820226183), avoiding the need for a seperate process on the machine.
- [Forking the crashing process](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819312104), and then directly use the forked process to collect and transmit the crash report, avoiding the need for a separate binary and IPC.

### Scrubbing potentially sensitive data

The crashtracker transmits data about the state of the customer process.
Although we make every effort to only upload non-sensitive data, it is possible that something might slip through the cracks.

- As a future task, investigate using a sensitive-data redaction tool, either client side or on the backend, as an additional mitigation.
- Limit the data we upload by doing client-side preprocessing.
  For example, instead of sending `/proc/self/maps`, send normalized addresses as discussed [here](https://github.com/DataDog/libdatadog/pull/696#discussion_r1819293109).
