//! Native-only subprocess transport. Worker threads never contain Lisp/GC values.
//! Pipe bytes remain bytes; owner-thread code decides coding and callbacks.
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};
const CHUNK: usize = 8192;
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
static PROCESSES: AtomicUsize = AtomicUsize::new(0);
static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Budget(&'static AtomicUsize);
impl Budget {
    fn acquire(counter: &'static AtomicUsize, max: usize) -> io::Result<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < max).then_some(n + 1))
            .map_err(|_| io::Error::other("subprocess resource capacity exceeded"))?;
        Ok(Self(counter))
    }
}
impl Drop for Budget {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct SpawnSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessStatus {
    Running,
    Exited(ProcessExit),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}
#[derive(Debug)]
pub(crate) enum ProcessEvent {
    Output {
        stream: OutputStream,
        bytes: Vec<u8>,
    },
    Eof(OutputStream),
    Exit(ProcessExit),
    Failure(String),
    LimitReached,
}
struct ChildOwner {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct SignalRequest {
    signal: i32,
    reply: SyncSender<io::Result<()>>,
}
fn signal_owned_child(child: &std::process::Child, signal: i32) -> io::Result<()> {
    #[cfg(unix)]
    {
        if !matches!(signal, 2 | 9 | 15) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported owned-child signal",
            ));
        }
        let pid = libc::pid_t::try_from(child.id()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "child PID is outside pid_t")
        })?;
        if pid <= 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid child PID"));
        }
        // SAFETY: called only by the supervisor while it exclusively owns
        // this unreaped Child. Its positive PID cannot yet have been reused.
        if unsafe { libc::kill(pid, signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (child, signal);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned-child Unix signals are unsupported on this platform",
        ))
    }
}

/// Arc residence controls direct-child lifetime. No thread captures this Arc,
/// so dropping the last residence requests cancellation without joining/waiting.
pub(crate) struct ProcessTransport {
    pid: u32,
    status: Arc<Mutex<ProcessStatus>>,
    cancel: Arc<AtomicBool>,
    input: Mutex<Option<SyncSender<Vec<u8>>>>,
    events: Mutex<Receiver<ProcessEvent>>,
    terminal: Arc<Mutex<VecDeque<ProcessEvent>>>,
    signals: SyncSender<SignalRequest>,
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn terminal(queue: &Mutex<VecDeque<ProcessEvent>>, event: ProcessEvent) {
    // Each worker emits at most one failure/limit and one terminal event;
    // at most ten small control records exist per process, independent of I/O.
    lock(queue).push_back(event);
}
fn send_output(
    sender: &SyncSender<ProcessEvent>,
    cancel: &AtomicBool,
    mut event: ProcessEvent,
) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
fn reader(
    mut pipe: impl Read + Send + 'static,
    stream: OutputStream,
    sender: SyncSender<ProcessEvent>,
    cancel: Arc<AtomicBool>,
    total: Arc<AtomicUsize>,
    controls: Arc<Mutex<VecDeque<ProcessEvent>>>,
    budget: Budget,
) -> io::Result<()> {
    thread::Builder::new().name("rune-process-reader".into()).spawn(move || {
        let _budget = budget;
        let mut bytes = [0; CHUNK];
        loop {
            match pipe.read(&mut bytes) {
                Ok(0) => {
                    terminal(&controls, ProcessEvent::Eof(stream));
                    break;
                }
                Ok(count) => {
                    let previous = total.fetch_add(count, Ordering::AcqRel);
                    if previous > MAX_OUTPUT.saturating_sub(count) {
                        cancel.store(true, Ordering::Release);
                        terminal(&controls, ProcessEvent::LimitReached);
                        terminal(&controls, ProcessEvent::Eof(stream));
                        break;
                    }
                    if !send_output(
                        &sender,
                        &cancel,
                        ProcessEvent::Output { stream, bytes: bytes[..count].to_vec() },
                    ) {
                        terminal(&controls, ProcessEvent::Eof(stream));
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    terminal(&controls, ProcessEvent::Failure(error.to_string()));
                    terminal(&controls, ProcessEvent::Eof(stream));
                    break;
                }
            }
        }
    })?;
    Ok(())
}
impl ProcessTransport {
    pub(crate) fn spawn(spec: SpawnSpec) -> io::Result<Arc<Self>> {
        Self::spawn_mode(spec, false, false)
    }
    pub(crate) fn spawn_merged(spec: SpawnSpec) -> io::Result<Arc<Self>> {
        Self::spawn_mode(spec, true, false)
    }
    /// Spawn using a complete owner-supplied environment, without changing the
    /// process-wide environment shared by native worker threads.
    pub(crate) fn spawn_exact_environment(spec: SpawnSpec) -> io::Result<Arc<Self>> {
        Self::spawn_mode(spec, false, true)
    }
    pub(crate) fn spawn_merged_exact_environment(spec: SpawnSpec) -> io::Result<Arc<Self>> {
        Self::spawn_mode(spec, true, true)
    }
    fn spawn_mode(spec: SpawnSpec, merge_stderr: bool, exact_environment: bool) -> io::Result<Arc<Self>> {
        if spec.program.is_empty() || spec.args.len() > 256 || spec.env.len() > 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid subprocess arguments",
            ));
        }
        let bytes = spec
            .args
            .iter()
            .map(|s| s.len())
            .chain(spec.env.iter().flat_map(|(k, v)| [k.len(), v.len()]))
            .try_fold(spec.program.len(), |total, length| total.checked_add(length))
            .ok_or_else(|| io::Error::other("subprocess argument size overflow"))?;
        if bytes > 64 * 1024 {
            return Err(io::Error::other("subprocess argument size limit exceeded"));
        }
        let process_budget = Budget::acquire(&PROCESSES, 16)?;
        let supervisor_budget = Budget::acquire(&WORKERS, 64)?;
        let stdout_budget = Budget::acquire(&WORKERS, 64)?;
        let stderr_budget = Budget::acquire(&WORKERS, 64)?;
        let writer_budget = Budget::acquire(&WORKERS, 64)?;
        let mut command = Command::new(spec.program);
        if exact_environment {
            command.env_clear();
        }
        command
            .args(spec.args)
            .envs(spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = spec.cwd {
            command.current_dir(cwd);
        }
        // A real anonymous pipe shared by stdout/stderr preserves OS write
        // ordering, unlike merging independently scheduled reader events.
        let merged_reader = if merge_stderr {
            let (reader, writer) = std::io::pipe()?;
            command.stdout(writer.try_clone()?).stderr(writer);
            Some(reader)
        } else {
            None
        };
        let mut child = ChildOwner { child: command.spawn()?, reaped: false };
        drop(command);
        let pid = child.child.id();
        let (stdout, stderr): (Box<dyn Read + Send>, Box<dyn Read + Send>) = match merged_reader {
            Some(reader) => (Box::new(reader), Box::new(std::io::empty())),
            None => (
                Box::new(
                    child
                        .child
                        .stdout
                        .take()
                        .ok_or_else(|| io::Error::other("missing stdout pipe"))?,
                ),
                Box::new(
                    child
                        .child
                        .stderr
                        .take()
                        .ok_or_else(|| io::Error::other("missing stderr pipe"))?,
                ),
            ),
        };
        let mut stdin =
            child.child.stdin.take().ok_or_else(|| io::Error::other("missing stdin pipe"))?;
        let (event_tx, event_rx) = mpsc::sync_channel(64);
        let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(32);
        let (signal_tx, signal_rx) = mpsc::sync_channel::<SignalRequest>(16);
        let cancel = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(ProcessStatus::Running));
        let controls = Arc::new(Mutex::new(VecDeque::new()));
        let process = Arc::new(Self {
            pid,
            status: status.clone(),
            cancel: cancel.clone(),
            input: Mutex::new(Some(input_tx)),
            events: Mutex::new(event_rx),
            terminal: controls.clone(),
            signals: signal_tx,
        });
        let supervisor_cancel = cancel.clone();
        let supervisor_controls = controls.clone();
        // Spawn the supervisor first: any later worker startup failure drops
        // `process`, requesting cancellation; the supervisor still reaps Child.
        let supervisor =
            thread::Builder::new().name("rune-process-supervisor".into()).spawn(move || {
                let (_process_budget, _worker_budget) = (process_budget, supervisor_budget);
                let mut killed = false;
                loop {
                    if supervisor_cancel.load(Ordering::Acquire) && !killed {
                        let _ = child.child.kill();
                        killed = true;
                    }
                    for _ in 0..16 {
                        let Ok(request) = signal_rx.try_recv() else {
                            break;
                        };
                        let result = if killed {
                            Err(io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "process cancellation is pending",
                            ))
                        } else {
                            signal_owned_child(&child.child, request.signal)
                        };
                        let _ = request.reply.try_send(result);
                    }
                    match child.child.try_wait() {
                        Ok(Some(exit)) => {
                            child.reaped = true;
                            #[cfg(unix)]
                            let signal = {
                                use std::os::unix::process::ExitStatusExt;
                                exit.signal()
                            };
                            #[cfg(not(unix))]
                            let signal = None;
                            let exit = ProcessExit { code: exit.code(), signal };
                            *lock(&status) = ProcessStatus::Exited(exit);
                            terminal(&supervisor_controls, ProcessEvent::Exit(exit));
                            break;
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(5)),
                        Err(error) => {
                            let _ = child.child.kill();
                            let _ = child.child.wait();
                            child.reaped = true;
                            let exit = ProcessExit { code: None, signal: None };
                            *lock(&status) = ProcessStatus::Exited(exit);
                            terminal(
                                &supervisor_controls,
                                ProcessEvent::Failure(error.to_string()),
                            );
                            terminal(&supervisor_controls, ProcessEvent::Exit(exit));
                            break;
                        }
                    }
                }
            });
        // On thread-creation failure, dropping the closure drops ChildOwner,
        // which kills/reaps the child. No live Child is silently abandoned.
        supervisor?;
        let total = Arc::new(AtomicUsize::new(0));
        reader(
            stdout,
            OutputStream::Stdout,
            event_tx.clone(),
            cancel.clone(),
            total.clone(),
            controls.clone(),
            stdout_budget,
        )?;
        reader(
            stderr,
            OutputStream::Stderr,
            event_tx.clone(),
            cancel.clone(),
            total,
            controls.clone(),
            stderr_budget,
        )?;
        let writer_status = process.status.clone();
        thread::Builder::new().name("rune-process-writer".into()).spawn(move || {
            let _budget = writer_budget;
            loop {
                if cancel.load(Ordering::Acquire)
                    || matches!(*lock(&writer_status), ProcessStatus::Exited(_))
                {
                    break;
                }
                match input_rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(bytes) => {
                        if cancel.load(Ordering::Acquire) {
                            break;
                        }
                        if let Err(error) = stdin.write_all(&bytes).and_then(|_| stdin.flush()) {
                            terminal(&controls, ProcessEvent::Failure(error.to_string()));
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })?;
        Ok(process)
    }
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }
    pub(crate) fn status(&self) -> ProcessStatus {
        *lock(&self.status)
    }
    pub(crate) fn send(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > CHUNK {
            return Err(io::Error::other("subprocess send exceeds 8192-byte limit"));
        }
        if self.status() != ProcessStatus::Running {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "subprocess has exited"));
        }
        let input = lock(&self.input);
        let sender = input.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "subprocess stdin is closed")
        })?;
        sender.try_send(bytes.to_vec()).map_err(|error| match error {
            TrySendError::Full(_) => {
                io::Error::new(io::ErrorKind::WouldBlock, "subprocess input queue is full")
            }
            TrySendError::Disconnected(_) => {
                io::Error::new(io::ErrorKind::BrokenPipe, "subprocess stdin is closed")
            }
        })
    }
    pub(crate) fn signal(&self, signal: i32) -> io::Result<()> {
        if !matches!(signal, 2 | 9 | 15) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only SIGINT, SIGKILL and SIGTERM are supported",
            ));
        }
        if self.cancel.load(Ordering::Acquire) || self.status() != ProcessStatus::Running {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "owned child is not running"));
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.signals
            .try_send(SignalRequest { signal, reply: reply_tx })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "process signal queue is full")
                }
                TrySendError::Disconnected(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "owned child has exited")
                }
            })?;
        reply_rx.recv_timeout(Duration::from_millis(100)).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => io::Error::new(
                io::ErrorKind::TimedOut,
                "owned-child signal delivery acknowledgement timed out",
            ),
            mpsc::RecvTimeoutError::Disconnected => io::Error::new(
                io::ErrorKind::BrokenPipe,
                "owned child exited before signal acknowledgement",
            ),
        })?
    }
    pub(crate) fn close_stdin(&self) {
        lock(&self.input).take();
    }
    pub(crate) fn terminate(&self) {
        self.cancel.store(true, Ordering::Release);
        self.close_stdin();
    }
    pub(crate) fn recv(&self, wait: Duration) -> Option<ProcessEvent> {
        let started = std::time::Instant::now();
        let wait = wait.min(Duration::from_millis(100));
        let receiver = lock(&self.events);
        {
            // Observe terminal publication before checking data. A reader
            // queues its last bytes before publishing EOF under this lock;
            // checking data afterwards prevents EOF overtaking those bytes.
            let mut controls = lock(&self.terminal);
            if let Ok(event) = receiver.try_recv() {
                return Some(event);
            }
            if let Some(event) = controls.pop_front() {
                return Some(event);
            }
        }
        match receiver.recv_timeout(wait) {
            Ok(event) => Some(event),
            Err(error) => {
                {
                    let mut controls = lock(&self.terminal);
                    if let Ok(event) = receiver.try_recv() {
                        return Some(event);
                    }
                    if let Some(event) = controls.pop_front() {
                        return Some(event);
                    }
                }
                if error == mpsc::RecvTimeoutError::Disconnected
                    && self.status() == ProcessStatus::Running
                {
                    thread::sleep(wait.saturating_sub(started.elapsed()));
                }
                let mut controls = lock(&self.terminal);
                if let Ok(event) = receiver.try_recv() {
                    return Some(event);
                }
                controls.pop_front()
            }
        }
    }
}
impl Drop for ProcessTransport {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn python(script: &str) -> Arc<ProcessTransport> {
        ProcessTransport::spawn(SpawnSpec {
            program: "/usr/bin/python3".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
            env: vec![],
        })
        .unwrap()
    }
    fn collect(process: &ProcessTransport) -> (Vec<u8>, Vec<u8>, ProcessExit) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut stdout, mut stderr, mut eof, mut exit) = (Vec::new(), Vec::new(), 0, None);
        while Instant::now() < deadline {
            match process.recv(Duration::from_millis(20)) {
                Some(ProcessEvent::Output { stream: OutputStream::Stdout, bytes }) => {
                    stdout.extend(bytes)
                }
                Some(ProcessEvent::Output { stream: OutputStream::Stderr, bytes }) => {
                    stderr.extend(bytes)
                }
                Some(ProcessEvent::Eof(_)) => eof += 1,
                Some(ProcessEvent::Exit(value)) => exit = Some(value),
                Some(ProcessEvent::Failure(error)) => panic!("subprocess I/O: {error}"),
                Some(ProcessEvent::LimitReached) => panic!("unexpected process output limit"),
                None => {}
            }
            if eof == 2 && exit.is_some() {
                return (stdout, stderr, exit.unwrap());
            }
        }
        panic!(
            "subprocess event drain timed out: eof={eof}, exit={exit:?}, status={:?}",
            process.status()
        );
    }
    #[test]
    fn argv_pipes_preserve_raw_bytes_and_stderr() {
        let process = python(
            "import os; data=os.read(0,8192); os.write(1,data); os.write(2,b'error\\xff'); raise SystemExit(7)",
        );
        process.send(b"raw\0\xfe\xff\xce\xbb").unwrap();
        process.close_stdin();
        let (stdout, stderr, exit) = collect(&process);
        assert_eq!(stdout, b"raw\0\xfe\xff\xce\xbb");
        assert_eq!(stderr, b"error\xff");
        assert_eq!(exit.code, Some(7));
        assert_eq!(process.status(), ProcessStatus::Exited(exit));
        assert!(process.send(b"late").is_err());
    }
    #[test]
    fn last_native_residence_cancels_and_reaps_direct_child() {
        let process = python("import time; time.sleep(30)");
        let status = process.status.clone();
        let start = Instant::now();
        drop(process);
        assert!(start.elapsed() < Duration::from_millis(100));
        while Instant::now().duration_since(start) < Duration::from_secs(5) {
            if matches!(*lock(&status), ProcessStatus::Exited(_)) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("cancelled direct child did not reap");
    }
    #[test]
    fn command_arguments_are_not_shell_syntax_and_send_is_bounded() {
        let process = ProcessTransport::spawn(SpawnSpec {
            program: "/usr/bin/python3".into(),
            args: vec![
                "-c".into(),
                "import sys; print(sys.argv[1])".into(),
                "$(touch /never-created); literal".into(),
            ],
            cwd: None,
            env: vec![],
        })
        .unwrap();
        assert!(process.send(&vec![0; CHUNK + 1]).is_err());
        let (out, _, exit) = collect(&process);
        assert_eq!(out, b"$(touch /never-created); literal\n");
        assert_eq!(exit.code, Some(0));
    }
    #[test]
    fn output_limit_requests_cancellation_and_emits_eof() {
        let (tx, rx) = mpsc::sync_channel(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let controls = Arc::new(Mutex::new(VecDeque::new()));
        reader(
            std::io::Cursor::new(vec![1, 2]),
            OutputStream::Stdout,
            tx,
            cancel.clone(),
            Arc::new(AtomicUsize::new(MAX_OUTPUT)),
            controls.clone(),
            Budget::acquire(&WORKERS, 64).unwrap(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while lock(&controls).len() < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(lock(&controls).pop_front(), Some(ProcessEvent::LimitReached)));
        assert!(matches!(
            lock(&controls).pop_front(),
            Some(ProcessEvent::Eof(OutputStream::Stdout))
        ));
        drop(rx);
        assert!(cancel.load(Ordering::Acquire));
    }
    #[test]
    fn cancellation_releases_workers_when_output_queue_is_full_and_arc_survives() {
        let process = python("import os,time; os.write(1,b'x'*2000000); time.sleep(30)");
        thread::sleep(Duration::from_millis(100));
        // Intentionally do not drain output; retain the native Arc after cancel.
        process.terminate();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let controls = lock(&process.terminal);
            let eof = controls.iter().filter(|e| matches!(e, ProcessEvent::Eof(_))).count();
            let exit = controls.iter().any(|e| matches!(e, ProcessEvent::Exit(_)));
            if eof == 2 && exit {
                break;
            }
            drop(controls);
            assert!(Instant::now() < deadline, "cancelled full-queue workers did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(process.status(), ProcessStatus::Exited(_)));
    }
    #[test]
    fn natural_exit_releases_idle_writer_with_native_arc_retained() {
        let process = python("print('done')");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(process.status(), ProcessStatus::Exited(_))
                && Arc::strong_count(&process.status) == 1
            {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("idle native writer survived natural child exit");
    }
    #[test]
    #[cfg(unix)]
    fn owned_sigint_reaches_real_handler_not_fake_kill() {
        let process = python(
            "import signal,sys,time; signal.signal(signal.SIGINT,lambda *args: sys.exit(42)); print('ready',flush=True); time.sleep(30)",
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(ProcessEvent::Output { stream: OutputStream::Stdout, bytes }) =
                process.recv(Duration::from_millis(20))
            {
                if bytes.windows(5).any(|word| word == b"ready") {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "synthetic signal handler did not become ready");
        }
        process.signal(2).unwrap();
        let (_, _, exit) = collect(&process);
        assert_eq!(exit.code, Some(42));
        assert_eq!(exit.signal, None);
        assert!(process.signal(9).is_err());
        assert!(process.signal(12345).is_err());
    }
    #[test]
    #[ignore = "requires explicitly configured official mu + synthetic Maildir fixture"]
    fn real_mu_server_roundtrip_preserves_binary_framing() {
        let binary = std::env::var_os("RUNE_MU_BINARY").expect("RUNE_MU_BINARY synthetic wrapper");
        let home = std::env::var_os("RUNE_MU_HOME").expect("RUNE_MU_HOME synthetic database");
        let fixture = std::env::var_os("RUNE_MU_FIXTURE").expect("RUNE_MU_FIXTURE synthetic root");
        let fixture = PathBuf::from(fixture);
        let process = ProcessTransport::spawn(SpawnSpec {
            program: binary,
            args: vec![
                "server".into(),
                format!("--muhome={}", PathBuf::from(home).display()).into(),
            ],
            cwd: Some(fixture.clone()),
            env: vec![
                ("HOME".into(), fixture.join("home").into()),
                ("XDG_CACHE_HOME".into(), fixture.join("cache").into()),
                ("XDG_CONFIG_HOME".into(), fixture.join("config").into()),
            ],
        })
        .unwrap();
        process.send(b"(ping)\n(find :query \"subject:synthetic\" :threads nil :sortfield :date :descending nil :maxnum 10 :skip-dups t :include-related nil)\n(view :docid 1 :mark-as-read nil :rename nil)\n(quit)\n").unwrap();
        let (out, _, exit) = collect(&process);
        assert_eq!(exit.code, Some(0));
        let mut offset = 0;
        let mut frames = Vec::new();
        while let Some(index) = out[offset..].iter().position(|b| *b == 0xfe) {
            let start = offset + index + 1;
            let end = start + out[start..].iter().position(|b| *b == 0xff).unwrap();
            let size =
                usize::from_str_radix(std::str::from_utf8(&out[start..end]).unwrap(), 16).unwrap();
            assert!(size <= 1024 * 1024);
            let bytes = &out[end + 1..end + 1 + size];
            frames.push(std::str::from_utf8(bytes).unwrap().to_owned());
            offset = end + 1 + size;
        }
        assert!(frames.iter().any(|s| s.contains(":pong")));
        assert!(frames.iter().any(|s| s.contains(":headers") && s.contains("Rune synthetic λ")));
        assert!(frames.iter().any(|s| s.contains(":view") && s.contains("001-synthetic.eml")));
    }
}
