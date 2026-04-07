# Refactor the trace buffer to get rid of the mutex protected shared state

# My plan

Code in libdd-data-pipeline/src/trace_buffer/mod.rs currently manually implements a queue, as a Vec protected by a Mutex.
I want to refactor this code to use a crossbeam_channel, and get rid of the Mutex around the state (in most cases, except synchronous mode).

The queue is a queue of trace chunks, each trace chunk is a Vec of items.
I want my queue to be bounded, but in the total number of items and not trace chunks.
In order to do this, the number of items in the queue needs to be kept in an atomic variable, checked before adding to the queue.
I also need to move two flags, flush_needed and has_shutdown to be atomic.

Lastly we have a synchronous mode, which checks if chunks have been exported before returning by comparing a batch version number.
This synchronous mode can use a Mutex as conccurency is limited anyway.

Here is how I want this to be implemented:

* span_count, flush_needed and has_shutdown are kept within a single AtomicU64, with the boolean flags occupying the high bits, and the count the 62 low bits

In the sender:
* First we fetch span_count and the flags, and check for shutdown and if the buffer is full return an error
* If in synchronous mode
  * we lock the Mutex
  * we get the batch generation
* We push the trace chunk in the queue
* We increment span_count, and set flush needed if the ocunt is greater than the threshold
* if we set flush_needed
  * we notify the receiver
* If in synchronous mode
  * we unlock the mutex
* If in synchronous mode
  * we use the condvar to wait for the last_flush to be greater than our batch generation

In the worker:
* We wait for a notification or a timeout
* We fetch the span_count
* If in synchronous mode
  * we lock the mutex
  * we increment batch generation
* we consume at least span_count spans from the queue
* we decrement the span_count from how many we consummed
* If in synchronous mode
  * we unlock the mutex

* We export
* If in synchronous mode
  * we increment last_flush_generation
  * we notify the sender condvar

# How to execute the task

* Figure out details of how this should be implemented
* Validate that this plan is correct
    * If it is not, propose fixes
