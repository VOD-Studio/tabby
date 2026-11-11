use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Mutex, thread};

use super::{runner::Cmd, testing::*, *};

async fn ensure(core: &ManagedCore) -> (Result<(), CoreError>, Vec<Step>) {
    let steps = Mutex::new(vec![]);
    let result = core
        .ensure_running(|step| steps.lock().unwrap().push(step))
        .await;
    let steps = steps.into_inner().unwrap();
    (result, steps)
}

#[tokio::test]
async fn a_kernel_that_already_answers_is_used_and_nothing_is_started() {
    let dir = scratch("attach");
    let port = free_port();
    serve(port, Reply::Mihomo, Duration::ZERO);
    let recorder = systemd(port, Duration::ZERO);
    let core = core_at(&dir, port, &recorder, true);
    thread::sleep(Duration::from_millis(100));

    let (result, steps) = ensure(&core).await;

    result.unwrap();
    assert_eq!(steps, Vec::<Step>::new());
    assert_eq!(recorder.calls(), Vec::<Cmd>::new());
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_port_answered_by_something_else_is_reported_and_left_alone() {
    for reply in [Reply::Rejects, Reply::Other] {
        let dir = scratch("foreign");
        let port = free_port();
        serve(port, reply, Duration::ZERO);
        let recorder = systemd(port, Duration::ZERO);
        let core = core_at(&dir, port, &recorder, true);
        thread::sleep(Duration::from_millis(100));

        let (result, _) = ensure(&core).await;

        assert!(
            matches!(result, Err(CoreError::PortTaken(taken)) if taken == port),
            "{result:?}"
        );
        assert_eq!(recorder.calls(), Vec::<Cmd>::new());
        fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn a_transient_unit_runs_the_kernel_and_the_secret_stays_off_the_command_line() {
    let dir = scratch("unit");
    let port = free_port();
    let recorder = systemd(port, Duration::from_millis(150));
    let core = core_at(&dir, port, &recorder, true);

    let (result, steps) = ensure(&core).await;

    result.unwrap();
    assert_eq!(steps, [Step::Starting, Step::WaitingController]);
    let binary = dir.join("mihomo");
    let home = dir.join("core/home");
    assert_eq!(
        recorder.runs("systemd-run"),
        [format!(
            "--user --unit={UNIT} --collect -p Restart=on-failure -p RestartSec=2 -- {} -d {} -f {}",
            binary.display(),
            home.display(),
            home.join("config.yaml").display()
        )]
    );
    for cmd in recorder.calls() {
        assert!(!cmd.args.join(" ").contains(SECRET), "{cmd:?}");
    }
    let config = home.join("config.yaml");
    let text = fs::read_to_string(&config).unwrap();
    assert!(
        text.contains(&format!("\"secret\": \"{SECRET}\"")),
        "{text}"
    );
    assert!(text.contains(&format!("\"external-controller\": \"127.0.0.1:{port}\"")));
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_unit_that_is_starting_is_waited_for_not_started_twice() {
    let dir = scratch("activating");
    let port = free_port();
    let recorder = Recorder::new(|cmd| match cmd.args.get(1).map(String::as_str) {
        Some("is-active") => answer(3, "activating\n"),
        _ => answer(0, ""),
    });
    serve(port, Reply::Mihomo, Duration::from_millis(200));
    let core = core_at(&dir, port, &recorder, true);

    let (result, steps) = ensure(&core).await;

    result.unwrap();
    assert_eq!(steps, [Step::WaitingController]);
    assert_eq!(recorder.runs("systemd-run"), Vec::<String>::new());
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_kernel_that_dies_at_start_fails_with_what_the_journal_says() {
    let dir = scratch("dies");
    let port = free_port();
    let polls = Mutex::new(0);
    let recorder = Recorder::new(move |cmd| match (cmd.program.as_str(), cmd.args.get(1)) {
        ("systemctl", Some(verb)) if verb == "is-active" => {
            let mut polls = polls.lock().unwrap();
            *polls += 1;
            // Not there before the start; failed after it.
            if *polls == 1 {
                answer(3, "inactive\n")
            } else {
                answer(3, "failed\n")
            }
        }
        ("journalctl", _) => answer(0, "Parse config error: yaml: line 3\n"),
        _ => answer(0, ""),
    });
    let core = core_at(&dir, port, &recorder, true);

    let (result, _) = ensure(&core).await;

    match result {
        Err(CoreError::StartFailed { log }) => {
            assert_eq!(log, "Parse config error: yaml: line 3");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        recorder.runs("journalctl"),
        [format!("--user -u {UNIT} -n 20 -o cat --no-pager")]
    );
    // A failed unit is stopped, not left to be restarted forever.
    assert_eq!(
        recorder.runs("systemctl").last().unwrap(),
        &format!("--user stop {UNIT}")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_kernel_whose_controller_never_answers_times_out_with_its_log() {
    let dir = scratch("timeout");
    let port = free_port();
    let recorder = Recorder::new(|cmd| match (cmd.program.as_str(), cmd.args.get(1)) {
        ("systemctl", Some(verb)) if verb == "is-active" => answer(3, "activating\n"),
        ("journalctl", _) => answer(0, "address already in use\n"),
        _ => answer(0, ""),
    });
    let mut core = core_at(&dir, port, &recorder, true);
    core.timing.ready = Duration::from_millis(200);

    // `activating`: the unit is waited for, then given up on.
    let (result, _) = ensure(&core).await;

    assert!(
        matches!(&result, Err(CoreError::StartFailed { log }) if log == "address already in use"),
        "{result:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_user_manager_is_found_by_its_socket_or_by_saying_it_runs() {
    for (answer_text, expected) in [
        ("running\n", Mode::Systemd),
        ("degraded\n", Mode::Systemd),
        ("offline\n", Mode::Fallback),
        ("", Mode::Fallback),
    ] {
        let dir = scratch("mode");
        let recorder = Recorder::new(move |_| answer(1, answer_text));
        let core = core_at(&dir, free_port(), &recorder, false);
        assert_eq!(core.mode().await, expected, "{answer_text:?}");
        assert_eq!(
            recorder.runs("systemctl"),
            ["--user is-system-running"],
            "asked once, and the answer kept"
        );
        core.mode().await;
        assert_eq!(recorder.runs("systemctl").len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    // The socket alone is enough: nobody is asked.
    let dir = scratch("mode-socket");
    let recorder = Recorder::new(|_| answer(1, ""));
    let core = core_at(&dir, free_port(), &recorder, true);
    assert_eq!(core.mode().await, Mode::Systemd);
    assert_eq!(recorder.calls(), Vec::<Cmd>::new());
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_running_unit_is_restarted_through_systemd_on_a_fresh_config() {
    let dir = scratch("restart");
    let port = free_port();
    serve(port, Reply::Mihomo, Duration::ZERO);
    let recorder = Recorder::new(|cmd| match cmd.args.get(1).map(String::as_str) {
        Some("is-active") => answer(0, "active\n"),
        _ => answer(0, ""),
    });
    let core = core_at(&dir, port, &recorder, true);
    thread::sleep(Duration::from_millis(100));

    core.restart().await.unwrap();

    assert_eq!(
        recorder.runs("systemctl").last().unwrap(),
        &format!("--user restart {UNIT}")
    );
    assert_eq!(recorder.runs("systemd-run"), Vec::<String>::new());
    assert!(dir.join("core/home/config.yaml").exists());
    fs::remove_dir_all(dir).unwrap();
}

/// Systemd is not there; a `kill` really kills and `ps` really looks.
fn without_systemd() -> Arc<Recorder> {
    Recorder::new(|cmd| match cmd.program.as_str() {
        "kill" | "ps" => System.run(cmd).unwrap(),
        _ => answer(1, "offline\n"),
    })
}

/// A core of `recorder` for the system the tests run on: its processes are
/// real.
fn real_core(dir: &Path, recorder: &Arc<Recorder>) -> ManagedCore {
    let mut core = core_at(dir, free_port(), recorder, false);
    core.os = Os::current();
    core
}

/// What `ps` says of process `pid`, `field` alone; `None` when it is gone.
fn ps(pid: u32, field: &str) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}

fn gone(core: &ManagedCore, pid: u32) -> bool {
    let binary = core.binary().unwrap();
    ps(pid, "command").is_none_or(|command| !command.contains(&*binary.to_string_lossy()))
}

#[tokio::test]
async fn without_systemd_the_kernel_is_a_process_that_can_be_found_and_stopped() {
    let dir = scratch("fallback");
    let recorder = without_systemd();
    let core = real_core(&dir, &recorder);
    let binary = core.binary().unwrap();
    core.write_config().unwrap();

    assert_eq!(core.status().await, supervisor::Status::Inactive);
    core.launch(&binary).await.unwrap();
    let pid = core
        .stored_process()
        .expect("the process is kept in state.toml")
        .pid;
    assert_eq!(core.status().await, supervisor::Status::Active);
    assert!(core.alive().await);
    // Its own process group: a ^C for tabby does not reach it.
    assert_eq!(ps(pid, "pgid"), Some(pid.to_string()));

    core.stop().await.unwrap();
    assert_eq!(core.stored_process(), None);
    assert_eq!(core.status().await, supervisor::Status::Inactive);
    assert!(gone(&core, pid));
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_second_launch_keeps_the_previous_log_and_the_log_tail_is_the_current_one() {
    let dir = scratch("log");
    let recorder = without_systemd();
    let core = real_core(&dir, &recorder);
    let binary = dir.join("noisy");
    fs::write(
        &binary,
        "#!/bin/sh\necho \"started $$\"\necho oops >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let mut core = core;
    core.binary = Some(binary.clone());

    core.launch(&binary).await.unwrap();
    thread::sleep(Duration::from_millis(300));
    let first = fs::read_to_string(dir.join("core/core.log")).unwrap();
    core.launch(&binary).await.unwrap();
    thread::sleep(Duration::from_millis(300));

    assert_eq!(
        fs::read_to_string(dir.join("core/core.log.1")).unwrap(),
        first
    );
    let tail = core.log_tail().await;
    assert!(tail.contains("oops") && tail.contains("started"), "{tail}");
    assert_ne!(tail, first.trim());
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn on_macos_the_kernel_is_told_from_a_reused_pid_by_when_ps_says_it_started() {
    let dir = scratch("ps-start");
    let started = Arc::new(Mutex::new("Wed Nov 11 10:00:00 2026"));
    let says = Arc::clone(&started);
    let recorder = Recorder::new(move |cmd| match cmd.program.as_str() {
        "ps" if cmd.args == ["-o", "lstart=", "-p", "4242"] => {
            answer(0, &format!("{}\n", says.lock().unwrap()))
        }
        "ps" => answer(1, ""),
        _ => answer(0, ""),
    });
    let mut core = core_at(&dir, free_port(), &recorder, false);
    core.os = Os::Macos;
    let start = core.start_time(4242).await;
    assert!(start.is_some());
    assert_eq!(core.start_time(7).await, None);
    let mut state = State::load_or_create(&core.paths.state()).unwrap();
    state.pid = Some(4242);
    state.pid_start = start;
    state.save(&core.paths.state()).unwrap();

    assert_eq!(core.status().await, supervisor::Status::Active);
    // The kernel ended and another process got its number.
    *started.lock().unwrap() = "Wed Nov 11 10:05:00 2026";
    assert_eq!(core.status().await, supervisor::Status::Inactive);
    core.stop().await.unwrap();

    assert_eq!(recorder.runs("kill"), Vec::<String>::new());
    assert_eq!(core.stored_process(), None);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_pid_that_is_not_the_kernel_is_never_signalled() {
    let dir = scratch("stale");
    let recorder = without_systemd();
    let core = real_core(&dir, &recorder);
    // This very process has a number, but did not start when the kernel did.
    let mut state = State::load_or_create(&core.paths.state()).unwrap();
    state.pid = Some(std::process::id());
    state.pid_start = Some(1);
    state.save(&core.paths.state()).unwrap();

    assert_eq!(core.status().await, supervisor::Status::Inactive);
    core.stop().await.unwrap();

    assert_eq!(recorder.runs("kill"), Vec::<String>::new());
    assert_eq!(core.stored_process(), None);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_fallback_kernel_that_exits_at_start_fails_with_its_log() {
    let dir = scratch("fallback-dies");
    let port = free_port();
    let recorder = without_systemd();
    let mut core = core_at(&dir, port, &recorder, false);
    let binary = dir.join("broken");
    fs::write(
        &binary,
        "#!/bin/sh\necho 'Parse config error: bad' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    core.binary = Some(binary);

    let (result, _) = ensure(&core).await;

    match result {
        Err(CoreError::StartFailed { log }) => assert_eq!(log, "Parse config error: bad"),
        other => panic!("{other:?}"),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn without_a_configured_kernel_the_one_tabby_keeps_is_used_once_it_is_there() {
    let dir = scratch("kept");
    let recorder = Recorder::new(|_| answer(0, ""));
    let core = core_without_binary(&dir, free_port(), &recorder);
    let kept = dir.join("core/bin/mihomo");
    assert_eq!(core.binary_path(), kept);
    assert!(!core.is_user_binary());
    // Nothing downloaded yet: not a path that was configured, so it says
    // there is nothing to point at.
    assert!(matches!(
        core.binary(),
        Err(CoreError::NoBinary { path: None })
    ));

    fs::create_dir_all(kept.parent().unwrap()).unwrap();
    fs::write(&kept, "").unwrap();
    assert_eq!(core.binary().unwrap(), kept);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_configured_binary_that_is_not_a_file_is_named_in_the_error() {
    let dir = scratch("configured");
    let recorder = Recorder::new(|_| answer(0, ""));
    let mut core = core_at(&dir, free_port(), &recorder, false);
    let missing = dir.join("nowhere");
    core.binary = Some(missing.clone());
    assert!(matches!(
        core.binary(),
        Err(CoreError::NoBinary { path: Some(path) }) if path == missing
    ));
    fs::remove_dir_all(dir).unwrap();
}

/// An executable `mihomo` in a directory of its own under `dir`.
fn kernel_in(dir: &Path, name: &str) -> PathBuf {
    let bin = dir.join(name);
    fs::create_dir_all(&bin).unwrap();
    let kernel = bin.join("mihomo");
    fs::write(&kernel, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&kernel, fs::Permissions::from_mode(0o755)).unwrap();
    kernel
}

#[test]
fn without_a_kernel_of_its_own_tabby_runs_the_first_mihomo_on_path_and_does_not_replace_it() {
    let dir = scratch("on-path");
    let recorder = Recorder::new(|_| answer(0, ""));
    let mut core = core_without_binary(&dir, free_port(), &recorder);
    // Not executable, then executable: the first that can run is taken.
    let plain = dir.join("plain");
    fs::create_dir_all(&plain).unwrap();
    fs::write(plain.join("mihomo"), "").unwrap();
    let system = kernel_in(&dir, "system");
    let later = kernel_in(&dir, "later");
    core.search_path = Some(
        env::join_paths([
            plain,
            system.parent().unwrap().to_owned(),
            later.parent().unwrap().to_owned(),
        ])
        .unwrap(),
    );

    assert_eq!(core.binary().unwrap(), system);
    assert!(core.is_user_binary());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_kernel_tabby_keeps_comes_before_one_on_path() {
    let dir = scratch("kept-over-path");
    let recorder = Recorder::new(|_| answer(0, ""));
    let mut core = core_without_binary(&dir, free_port(), &recorder);
    let system = kernel_in(&dir, "system");
    core.search_path = Some(system.parent().unwrap().as_os_str().to_owned());
    let kept = dir.join("core/bin/mihomo");
    fs::create_dir_all(kept.parent().unwrap()).unwrap();
    fs::write(&kept, "").unwrap();

    assert_eq!(core.binary().unwrap(), kept);
    assert!(!core.is_user_binary());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_relative_path_entry_is_not_searched_for_a_kernel() {
    let dir = scratch("relative-path");
    let recorder = Recorder::new(|_| answer(0, ""));
    let mut core = core_without_binary(&dir, free_port(), &recorder);
    let kernel = kernel_in(&dir, "bin");
    // A relative entry that does lead to it from where the tests run: it
    // would be found if relative entries were searched.
    let up = "../".repeat(env::current_dir().unwrap().components().count() - 1);
    let relative = PathBuf::from(up).join(kernel.parent().unwrap().strip_prefix("/").unwrap());
    assert!(relative.join("mihomo").is_file());
    core.search_path = Some(relative.into_os_string());

    assert!(matches!(
        core.binary(),
        Err(CoreError::NoBinary { path: None })
    ));
    assert!(!core.is_user_binary());
    fs::remove_dir_all(dir).unwrap();
}
