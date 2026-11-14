# Queued native startup failure exit

The native launch callback previously called App::quit synchronously on font
calibration/admission errors. GPUI 0.2.2's generic Linux Platform::run invokes the
launch callback BEFORE LinuxClient::run starts its event loop. An actual official
Application::headless diagnostic recorded its failure, then hung until the
30-second guard terminated it: the early stop signal did not end the new loop.

Queue quit through GPUI's official foreground executor. The failure flag is
stored first, the callback returns without opening a window, and the foreground
turn requests quit after the loop starts. Existing worker-failure reporting,
channel ownership, definitely-unprocessed event rejection and later in-loop
window-close/owner-failure behavior are unchanged. No editor command is replayed.

The dedicated native-only headless example intentionally records a startup
failure, invokes the SAME helper and verifies App::run returns with the failure
state intact. Its successful diagnostic exit does NOT make production startup
success: native::run still returns Err(()) after the recorded failure. No Window,
GPU paint, OS input, font shaping or graphical compatibility is established.
