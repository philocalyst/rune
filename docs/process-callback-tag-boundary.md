# Owner callback tag boundary

Process filter/sentinel setters can retain arbitrary owner-rooted callback
objects, as GNU does; that does not make every value callable. At delivery the
owner checks the function tag before converting it. Invalid tags signal
invalid-function with the original rooted object and never invoke the generic
TypeError graph formatter. NIL continues to select the genuine default callback.
The snapshot/process/argument roots remain live before and during the call.

This boundary must accompany the permissive process setter changes. It does not
validate every malformed interpreted-function body, implement missing function
forms, preempt long callbacks, alter delivery/retry state, or move evaluation to
worker threads. A cyclic vector containing bytecode constants is rejected safely
and retains original signal-data identity across moving collections.
