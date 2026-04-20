### gotter
This is used to provide our GOT table interposition approach. It will contain:

* A set of functions such as `gotter_malloc` that will be used to override the _originals_ of these functions
* Overrides for _other bits_ we need for this to work robustly in a running process - including e.g. `fork` 
* A function to install the overrides in a running process `install_heap_overrides()`

This will be used in places such as the python profiler to install the heap profiler at runtime to capture native allocations. 
