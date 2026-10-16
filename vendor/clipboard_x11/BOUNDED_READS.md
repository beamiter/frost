# Opt-in bounded X11 reader

`Clipboard::connect()` and the public `Context`/`Worker` APIs preserve the legacy
path. `Clipboard::connect_bounded()` opens the reader during the existing
synchronous connection phase, using the same official DISPLAY address order and
xauth fallback as x11rb. It opens the original writer connection separately. No
request opens, authenticates, or reconnects a display connection.

`read_async` is the nonblocking submission API. Submission errors do not call the
completion. The provider clamps requests to 1 MiB, three seconds from submission,
and eight queued plus active requests. Cancellation and completion callbacks must
be nonblocking. Their panics are caught; a cancellation panic cancels the request.
The legacy `read` and `read_primary` methods remain synchronous compatibility
calls even on an opt-in clipboard.

A single owned worker uses a finite-poll stream over the original nonblocking
DefaultStream. Owner expiry between I/O operations destroys the requestor window
and leaves the connection usable. A cancellation/deadline inside a potentially
partial protocol operation fails the endpoint closed: queued requests complete
with errors, the connection is dropped, and future submissions fail. There is no
implicit reconnect. Shutdown sets an atomic checked by active operations and
joins the reader worker. The original writer behavior is unchanged.

Every request creates a new requestor window. Selection and property events are
checked against that window and the expected selection/target/property. Payload
queries first probe size with a zero-length GetProperty; subsequent lengths use
four-byte units. INCR headers are validated but never used to reserve memory.
An aborted transfer destroys its window, rather than deleting the property to
acknowledge another chunk. UTF-8 remains strict, matching the existing X11 API.

## Tests

From the candidate workspace, pure tests run with:

    cargo test -p clipboard_x11 --lib --locked

Three ignored tests compile with the package. They require a native hosted test
machine; do not bypass a local socket access denial. Set FROST_X11_TEST_DISPLAY to
a dedicated disposable X server, never a personal desktop. For example, on a
host with Xvfb installed:

    Xvfb :98 -screen 0 800x600x24 -nolisten tcp &
    FROST_X11_TEST_DISPLAY=:98 cargo test -p clipboard_x11 --lib --locked -- --ignored --test-threads=1

The synthetic fixtures exercise direct and PRIMARY reads, delayed owners,
multichunk INCR, oversized hints and chunks, strict UTF-8, stale requestor events,
expiry followed by a healthy read, cancellation, eight/ninth admission, pending
teardown, and stalled stream deadline. They only use synthetic clipboard bytes.
The Unix stream deadline test does not need DISPLAY, but does need permission to
create an AF_UNIX socket pair. Native runtime evidence is required separately;
a compile-only ignored test is not acceptance evidence.

The bounded endpoint implementation is for the Unix X11 targets selected by
window_clipboard. Initial setup, legacy synchronous compatibility calls, and
legacy writer behavior are outside the asynchronous-read guarantee. An arbitrary
blocking callback or a malicious X server's unbounded protocol output is outside
the clipboard-owner threat model.

Request expiration may be followed by at most 100 ms of bounded requestor-window retirement before callback delivery. Shutdown skips this retirement and drops the connection. Nonblocking event polling does not inject cancellation into the stream: x11rb retains any partial event packet, and cancellation is checked immediately before and after the poll. This preserves reuse after ordinary owner cancellation/expiry; cancellation during potentially partial request/reply I/O still closes the endpoint.
