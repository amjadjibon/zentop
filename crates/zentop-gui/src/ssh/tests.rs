use super::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

const LINE: &str = r#"{"version":1,"sequence":1,"captured_at_ms":0,"metrics":{"cpu_percent":2,"memory_used":4,"memory_total":8,"disk_used":3,"disk_total":20,"uptime_seconds":50},"processes":[{"pid":42,"name":"fixture","cpu_percent":112,"memory_bytes":123}]}"#;

struct Fixture(PathBuf);

impl Fixture {
    fn fake_ssh(body: &str) -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "zentop-fake-ssh-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let script = dir.join("ssh");
        fs::write(
            &script,
            format!("#!/bin/sh\nset -eu\ncd -- \"${{0%/*}}\"\n{body}"),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        Self(dir)
    }

    fn program(&self) -> PathBuf {
        self.0.join("ssh")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "fixture did not reach expected state"
        );
        Timer::after(Duration::from_millis(10)).await;
    }
}

fn dead(pid_path: &Path) -> bool {
    let Ok(pid) = fs::read_to_string(pid_path) else {
        return false;
    };
    !std::process::Command::new("/bin/kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn fake_ssh_retries_retains_readings_and_reaps_on_disconnect() {
    let fixture = Fixture::fake_ssh(&format!(
        r#"
printf '%s\n' "$@" > args
printf '%s\n' '{LINE}'
if [ ! -f first ]; then
    touch first
    echo 'synthetic link loss' >&2
    exit 255
fi
echo "$$" > pid
exec /bin/sleep 30
"#
    ));
    smol::block_on(async {
        let (send, recv) = channel::bounded(4);
        let state = Arc::new(Mutex::new(Readings {
            phase: Phase::Disconnected,
            recent: VecDeque::new(),
        }));
        let worker_state = state.clone();
        let program = fixture.program();
        let task = smol::spawn(async move {
            supervise(program.as_os_str(), "observer@lab", &worker_state, recv).await;
        });

        until(|| matches!(state.lock().unwrap().phase, Phase::Reconnecting { .. })).await;
        let args = fs::read_to_string(fixture.0.join("args"))
            .unwrap_or_else(|e| panic!("{e}: {:?}", state.lock().unwrap().phase));
        let args: Vec<_> = args.lines().collect();
        assert_eq!(
            &args[args.len() - 3..],
            &["--", "observer@lab", "zentop-cli stream --protocol 1"]
        );
        for option in [
            "-a",
            "-T",
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "-oClearAllForwardings=yes",
            "-oControlPath=none",
        ] {
            assert!(args.contains(&option));
        }
        {
            let readings = state.lock().unwrap();
            assert!(
                matches!(&readings.phase, Phase::Reconnecting { error, .. } if error.contains("synthetic link loss"))
            );
            assert_eq!(readings.recent.front().unwrap().sample.processes[0].pid, 42);
        }

        // The scheduled retry should create a new sequence without clearing history.
        let pid_path = fixture.0.join("pid");
        until(|| state.lock().unwrap().recent.len() == 2 && pid_path.exists()).await;
        send.send(Action::Disconnect).await.unwrap();
        until(|| state.lock().unwrap().phase == Phase::Disconnected).await;
        until(|| dead(&pid_path)).await;
        assert_eq!(state.lock().unwrap().recent.len(), 2);
        let previous_pid = fs::read_to_string(&pid_path).unwrap();

        send.send(Action::Connect).await.unwrap();
        until(|| {
            state.lock().unwrap().recent.len() == 3
                && fs::read_to_string(&pid_path).is_ok_and(|pid| pid != previous_pid)
        })
        .await;
        drop(send);
        task.await;
        until(|| dead(&pid_path)).await;
    });
}

#[test]
fn fake_ssh_rejects_unexpected_sequence_and_limited_reader_rejects_flood() {
    let fixture = Fixture::fake_ssh(&format!(
        "printf '%s\\n' '{}'\n",
        LINE.replace("\"sequence\":1", "\"sequence\":2")
    ));
    smol::block_on(async {
        let error = read_session(fixture.program().as_os_str(), "fixture", |_| {
            panic!("accepted wrong sequence")
        })
        .await
        .unwrap_err();
        assert!(error.contains("out of sequence"), "{error}");

        let mut flood = BufReader::new(smol::io::repeat(b'x'));
        let mut line = Vec::new();
        assert_eq!(
            read_line(&mut flood, &mut line, Duration::from_secs(1))
                .await
                .unwrap(),
            ssh_stream::MAX_LINE_BYTES + 1
        );
        assert!(ssh_stream::decode_sample(&line).is_err());
    });
}

#[test]
fn quiet_loopback_stream_times_out_with_partial_input() {
    smol::block_on(async {
        use smol::io::AsyncWriteExt;
        use smol::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut writer = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (reader, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(reader);
        writer.write_all(b"{").await.unwrap();
        let mut line = Vec::new();
        let error = read_line(&mut reader, &mut line, Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(error.contains("Timed out"));
        assert_eq!(line, b"{");
    });
}
