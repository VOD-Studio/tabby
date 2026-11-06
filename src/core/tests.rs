use std::{os::unix::fs::PermissionsExt, sync::Mutex, thread};

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

/// Systemd is not there; a `kill` really kills.
fn without_systemd() -> Arc<Recorder> {
    Recorder::new(|cmd| match cmd.program.as_str() {
        "kill" => System.run(cmd).unwrap(),
        _ => answer(1, "offline\n"),
    })
}

fn gone(core: &ManagedCore, pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}/cmdline")).exists()
        || fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|text| {
            !text
                .split(|byte| *byte == 0)
                .any(|part| part == core.binary().unwrap().as_os_str().as_encoded_bytes())
        })
}

#[tokio::test]
async fn without_systemd_the_kernel_is_a_process_that_can_be_found_and_stopped() {
    let dir = scratch("fallback");
    let recorder = without_systemd();
    let core = core_at(&dir, free_port(), &recorder, false);
    let binary = core.binary().unwrap();
    core.write_config().unwrap();

    assert_eq!(core.status().await, supervisor::Status::Inactive);
    core.launch(&binary).await.unwrap();
    let pid = core.stored_pid().expect("the pid is kept in state.toml");
    assert_eq!(core.status().await, supervisor::Status::Active);
    assert!(core.alive().await);
    // Its own process group: a ^C for tabby does not reach it.
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let group: u32 = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(group, pid);

    core.stop().await.unwrap();
    assert_eq!(core.stored_pid(), None);
    assert_eq!(core.status().await, supervisor::Status::Inactive);
    assert!(gone(&core, pid));
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_second_launch_keeps_the_previous_log_and_the_log_tail_is_the_current_one() {
    let dir = scratch("log");
    let recorder = without_systemd();
    let core = core_at(&dir, free_port(), &recorder, false);
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
async fn a_pid_that_is_not_the_kernel_is_never_signalled() {
    let dir = scratch("stale");
    let recorder = without_systemd();
    let core = core_at(&dir, free_port(), &recorder, false);
    // This very process has a number, but is not the kernel.
    core.store_pid(Some(std::process::id())).unwrap();

    assert_eq!(core.status().await, supervisor::Status::Inactive);
    core.stop().await.unwrap();

    assert_eq!(recorder.runs("kill"), Vec::<String>::new());
    assert_eq!(core.stored_pid(), None);
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
fn a_binary_is_looked_up_in_path_order_and_must_be_executable() {
    let dir = scratch("path");
    let (first, second) = (dir.join("a"), dir.join("b"));
    for sub in [&first, &second] {
        fs::create_dir_all(sub).unwrap();
        fs::write(sub.join("mihomo"), "").unwrap();
    }
    fs::set_permissions(second.join("mihomo"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = env::join_paths([&first, &second]).unwrap();

    // The first has no execute bit, so the second is the one.
    assert_eq!(find_in(&path, "mihomo"), Some(second.join("mihomo")));
    assert_eq!(find_in(&path, "clash"), None);
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
