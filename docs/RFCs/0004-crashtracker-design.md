# RFC 0004: Crashtracker design

## Background

A crashtracker implementation must balance between two conflicting aims.
On the one hand, its purpose is to **reliably collect and report the information necessary to diagnose and debug all crashes** in the tracked process.
On the other hand, a crashtracker, like other instrumentation technology, should ideally **cause no observable difference in the execution of the processes where it is installed**.
These aims are in tension: design decisions which increase the quality and reliability of data-collection often increase the (potential) for impact on the customer system.

This document lays out the design priorities of the crashtracker, describes how the current architecture affects/implements those priorities, and then lays out a plan for future improvements to the crashtracking ecosystem.

## Design Priorities

1.  When the system is functioning normally, the crashtracker should, to the greatest extent possible, **have no impact on the functioning of a non-crashing system**.
2.  When a crash does occur, the crashtracker should **prioritize the reliable collection and reporting of crash information**.
3.  When a crash does occur, the crashtracker should **make a best-effort attempt to minimize impact** on the crashing process.

## Architectural model

### Collector

For all languages other than .Net and Java, this is a signal handler.
The collector interacts with the system in the following ways:

1.  Registers a signal handler (`sigaction`).
    The old handler is saved into a global variable to enable changing.
    - Risks to normal operation
      - Chaining handlers is not atomic.
        It is possible for concurrent races to occur.
        Mitigation: doing this operation once, early during system initialization.
      - If the system expects and handles `sigsegv`, `sigbus` and `sigabort` as part of its ordinary functioning, this can violate principle 1.
        This is not common for most languages, but Java and .Net turn some crashes into recoverable user exceptions.
        A crashtracker signal handler risks breaking that mechanism.
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
        As a stretch mitigation, we could do out-of-process unwinding.
        This would increase the complexity of the crashtracker, but would avoid this issue.
4.  Results written to a pipe (unix socket)
    - Risks to normal operation
      - Maintaining the open pipe requires a file-descriptor.
      - Maintaining a unix socket requires a synchronization point, typically a file in the file system.
    - Risks during a crash
      - If the pipe/socket is not drained, the collector will stall when attempting to write.
        This could hang the crashing process.
        Mitigation: Minimize the amount of data sent.
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
        Mitigation: only send a select set of files which do not contain sensitive data.

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
        Mitigation: This is unlikely to occur.
      - Eager initialization can lead to an additional child process.
        This was originally believed to be harmless.
        However, it turns out that some frameworks assume that they are the only thing that can spawn workers, and do a `waitpid` on all children.
        The existence of an additional child process hangs the framework.
        Mitigation: Either do lazy initialization, or use a proper daemonized sidecar.
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
      - The collector might crash or be terminated, truncating the message.
        Mitigation: If an unexpected input is received, including EOF, make a best effort attempt to format and send a partial crash report.
3.  Transmits the message to the backend.
    - Risks to normal operation
      - NA
    - Risks during a crash
      - The endpoint may be inaccessible.
      Mitigation: None.
      - It may take a signifcant amount of time to transmit the crashreport.
      This could cause the user process to hang.
      Mitigation: a configurable timeout on transmission (default 3s).
      - The message could be modified or corrupted in transit
      Mitigation: allow the optional use of `tls`.
