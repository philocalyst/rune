# Initial genuine pipe-process primitives

This layer depends on the native transport, genuine LispProcess GC tag and
owner-context rooted callback registry. It does not use a replacement MIME
viewer, fake mu4e features or mock process objects. `start-process` executes a
real argv child and registers owner-local process/filter/sentinel/buffer/plist
roots. Native workers never receive Lisp values.

Implemented here: start-process; processp/get-process/process-list; process
name/id/status/exit-status/live-p; buffer/filter/sentinel/plist accessors and
setters; process-get/put; coding/query-on-exit state; send-string/send-eof;
delete-process (nonblocking cancellation, registry retained for owner-pump
sentinel cleanup); callable default UTF8 buffer filter/sentinel. Duplicate live
registry names receive <N> suffixes. Public coding supports binary/no-conversion,
utf-8 and utf-8-unix only. Partial UTF8 read-state changes are rejected rather
than silently dropping bytes. Binary custom filters retain exact byte strings;
the UTF8 editor-buffer default filter rejects invalid UTF8.

The separate VM owner-pump supplies accept-process-output and actual callbacks.
This module alone does not dispatch filters or sentinels. Nil callback roots
select the implemented internal-default functions; getters/setters expose those
function symbols, matching the GNU reference rather than inventing nil metadata.

Explicit remaining gaps: process marks/pty/TTY, complete process-environment and
exec-path semantics, arbitrary process groups/signals, complete coding systems,
network/TLS, full dead-object metadata after registry retirement, arbitrary
large sends and process tree cleanup. Initial default buffer insertion appends
at end rather than implementing the complete GNU process-marker model. Do not
claim full process, mu4e or Doom compatibility from these primitives alone.

Tests create only synthetic Python children and verify genuine object identity,
name uniqueness, raw argument validation, owner-buffer/GC callback root survival,
plist and coding/query metadata. Actual native mu protocol tests are separately
explicitly configured against the generated three-message Maildir. No real
account, mailbox, synchronization, attachment extraction or send operation is
used. Callers retain authorization responsibility for program/env selection.

## Bounded synchronous call-process

The follow-up runs the actual executable via argv. A shell is executed only
when explicitly chosen as PROGRAM; no command string is manufactured here.
Stdout and stderr share one real anonymous pipe, preserving kernel write order.
Stdin is closed (only nil INFILE is supported). Destination nil discards; t,
buffer objects and buffer names receive completed UTF8 output. Complete output
is published only after success of transport/drain/decoding, not incrementally.
Non-UTF8 destination output errors explicitly. File stdin, integer/asynchronous
destinations, separate stderr-file destination lists and complete signal-name
return descriptions are not implemented. DISPLAY is presentation-only here.

A 10-second wall deadline after spawn, owner quit polling, 1 MiB combined capture
limit and native transport limits apply. Failure/quit/timeout requests native
cancellation before return; no partial buffer text is inserted. OS scheduling,
spawn and allocation are not promised hard-real-time. Tests cover literal argv,
explicit shell quoting, merged writes, capture-limit rejection, a real 10-second
sleep timeout and the actual pinned mu --version stdout/exit code. A passing
version handshake is not a claim of mu4e bootstrap/navigation in Rune.
