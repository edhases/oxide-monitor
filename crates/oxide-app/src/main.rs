#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Composition root for Oxide Monitor: logging, single instance, config,
//! the sensor thread and the Win32 UI event loop.

mod logging;
mod single_instance;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use oxide_core::{
    Config, PawnioCommand, PawnioState, PawnioStatus, ProviderUpdateStatus, ProvidersConfig,
    Snapshot, TempSource,
};
use oxide_pawnio_host::{
    Action as PawnioAction, Controller as PawnioController, Event as PawnioEvent, HELPER_VERSION,
    HelperClient, HelperProcess, detect as detect_pawnio_install, launch_helper,
};
use oxide_providers::update::{
    InstalledState, ProviderPaths, UpdateManager, now_unix_ms, resolve_installed_lhm,
    scan_managed_lhm,
};
use oxide_providers::{
    ExternalSensors, HttpDownloader, HttpFetcher, ProviderReadings, ProviderSource,
    WindowsVerifier, detect_pawnio, find_running_lhm,
};
use oxide_sensors::NativeSensors;
use oxide_ui::Ui;
use single_instance::SingleInstance;

/// Sampling cadence of the sensor thread.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// Back-off after a panicking provider.
const PANIC_BACKOFF: Duration = Duration::from_secs(5);
/// Official version checks are cached for 24 h; re-evaluate the installed state this often.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// How often the update thread wakes to notice a config change or shutdown.
const UPDATE_WAKE_INTERVAL: Duration = Duration::from_secs(30);
/// How long shutdown waits for the sensor thread before detaching it.
const JOIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Number of samples taken by `--headless` (one per second).
const HEADLESS_POLLS: usize = 3;

#[derive(Clone, Copy, Debug, Default)]
struct Args {
    headless: bool,
    once: bool,
    show: bool,
    autostart: bool,
    help: bool,
}

impl Args {
    fn parse() -> Self {
        Self::parse_from(std::env::args_os().skip(1))
    }

    fn parse_from<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut parsed = Self::default();
        for arg in args {
            let arg = arg.as_ref().to_string_lossy();
            match arg.as_ref() {
                "--headless" => parsed.headless = true,
                "--once" => parsed.once = true,
                "--show" => parsed.show = true,
                "--autostart" => parsed.autostart = true,
                "--help" | "-h" => parsed.help = true,
                other => tracing::warn!(argument = other, "unknown command-line argument"),
            }
        }
        parsed
    }
}

fn main() -> ExitCode {
    // Keep the appender guard alive until main returns so log lines are flushed.
    let _log_guard = logging::init();
    logging::install_panic_hook();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "Oxide Monitor starting"
    );

    let args = Args::parse();
    if args.help {
        println!(
            "Oxide Monitor {}\n\n\
             USAGE:\n    oxide-monitor [OPTIONS]\n\n\
             OPTIONS:\n\
             \x20   --headless    Poll native sensors without creating the UI\n\
             \x20   --once        With --headless, take a single sample\n\
             \x20   --show        Reveal the widget of an already running instance\n\
             \x20   --autostart   Logon-launch mode (do not focus an existing instance)\n\
             \x20   -h, --help    Print this help",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }

    if args.headless {
        run_headless(args.once)
    } else {
        run_gui(args.show, args.autostart)
    }
}

/// Normal application path: single instance, config, sensor thread, UI.
fn run_gui(show_requested: bool, launched_by_autostart: bool) -> ExitCode {
    enable_dpi_awareness();

    let instance = match SingleInstance::acquire() {
        Some(instance) if instance.already_running() => {
            if launched_by_autostart {
                // Logon task fired while the user already runs the widget: stay quiet
                // instead of yanking the existing instance into the foreground.
                tracing::info!(
                    "--autostart launch ignored: another Oxide Monitor instance is already running"
                );
            } else if show_existing_instance() {
                tracing::info!("another instance is running; asked it to show the widget");
            } else {
                tracing::info!(
                    "another Oxide Monitor instance is already running; exiting (use --show to reveal it)"
                );
            }
            return ExitCode::SUCCESS;
        }
        Some(instance) => Some(instance),
        None => None,
    };
    // Hold the mutex handle for the whole process lifetime.
    let _instance = instance;

    if show_requested {
        tracing::debug!("--show requested, but no other instance is running; starting normally");
    }

    let config = load_config();
    // Shared with the sensor thread and the UI: Settings Apply / hot reload swap the
    // provider configuration live without restarting the monitor.
    let provider_config = Arc::new(Mutex::new(config.providers.clone()));
    let provider_updates = Arc::new(Mutex::new(Vec::<ProviderUpdateStatus>::new()));
    let pawnio_status = Arc::new(Mutex::new(PawnioStatus::default()));
    let game_mode = Arc::new(AtomicBool::new(false));
    let (pawnio_tx, pawnio_rx) = mpsc::channel();

    let shared = Arc::new(Mutex::new(Snapshot::default()));
    let shutdown = Arc::new(AtomicBool::new(false));
    let sensor_thread = spawn_sensor_thread(
        config.clone(),
        Arc::clone(&shared),
        Arc::clone(&shutdown),
        Arc::clone(&provider_config),
        Arc::clone(&provider_updates),
        Arc::clone(&pawnio_status),
    );
    spawn_update_thread(
        Arc::clone(&provider_config),
        Arc::clone(&provider_updates),
        Arc::clone(&shutdown),
    );
    let pawnio_thread = spawn_pawnio_thread(
        Arc::clone(&provider_config),
        Arc::clone(&pawnio_status),
        Arc::clone(&game_mode),
        Arc::clone(&shutdown),
        pawnio_rx,
    );

    tracing::info!("starting UI");
    let mut ui = match Ui::new(config, Arc::clone(&shared)) {
        Ok(ui) => ui,
        Err(err) => {
            tracing::error!(%err, "failed to initialise UI");
            shutdown.store(true, Ordering::SeqCst);
            // Do not wait for a sensor thread that may be stuck in a blocking
            // provider call; process exit will reclaim it.
            return ExitCode::FAILURE;
        }
    };
    ui.set_provider_config_handle(provider_config);
    ui.set_pawnio_command_sender(pawnio_tx);
    ui.set_game_mode_handle(game_mode);

    let ui_result = ui.run();

    shutdown.store(true, Ordering::SeqCst);
    if let Some(handle) = sensor_thread {
        join_thread(handle, "sensor");
    }
    if let Some(handle) = pawnio_thread {
        join_thread(handle, "PawnIO helper manager");
    }

    match ui_result {
        Ok(()) => {
            tracing::info!("Oxide Monitor exited");
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!(%err, "UI exited with an error");
            ExitCode::FAILURE
        }
    }
}

/// Diagnostic path: poll `NativeSensors` and print snapshots, no window and no
/// single-instance mutex so it can run next to a live instance.
fn run_headless(once: bool) -> ExitCode {
    let config = load_config();
    let polls = if once { 1 } else { HEADLESS_POLLS };
    tracing::info!(polls, "headless sensor check");

    let provider_config = config.providers.clone();
    let pawnio = headless_pawnio_status(&config.providers);
    let mut sensors = NativeSensors::new(config);
    let mut external = ExternalSensors::new(provider_config);
    let mut failed = false;

    for index in 0..polls {
        if index > 0 {
            thread::sleep(POLL_INTERVAL);
        }

        match catch_unwind(AssertUnwindSafe(|| sensors.poll())) {
            // `poll` owns sequencing and `captured_at`; print as-is.
            Ok(mut snapshot) => {
                let readings = external.poll();
                apply_provider_readings(&mut snapshot, &readings, external.source());
                snapshot.providers = external.statuses();
                snapshot.pawnio = pawnio.clone();
                println!("{snapshot:#?}");
            }
            Err(payload) => {
                failed = true;
                let message = logging::panic_message(payload.as_ref());
                tracing::error!(poll = index + 1, panic = %message, "sensor poll panicked");
                eprintln!("sensor poll {} panicked: {message}", index + 1);
            }
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Resolve and load the config, falling back to defaults with a log entry.
fn load_config() -> Config {
    let path = Config::resolve_path();
    tracing::info!(path = %path.display(), "loading config");

    match Config::load_from_path(&path) {
        Ok(config) => {
            tracing::info!(
                path = %path.display(),
                items = config.items.len(),
                "config loaded"
            );
            config
        }
        Err(err) => {
            tracing::error!(
                path = %path.display(),
                %err,
                "failed to load config; using defaults"
            );
            Config::default()
        }
    }
}

/// Spawn the 1 Hz sensor thread. Returns `None` if the thread could not start.
fn spawn_sensor_thread(
    config: Config,
    shared: Arc<Mutex<Snapshot>>,
    shutdown: Arc<AtomicBool>,
    provider_config: Arc<Mutex<ProvidersConfig>>,
    provider_updates: Arc<Mutex<Vec<ProviderUpdateStatus>>>,
    pawnio_status: Arc<Mutex<PawnioStatus>>,
) -> Option<JoinHandle<()>> {
    match thread::Builder::new()
        .name("oxide-sensors".into())
        .spawn(move || {
            sensor_loop(
                config,
                shared,
                shutdown,
                provider_config,
                provider_updates,
                pawnio_status,
            )
        }) {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::error!(%err, "failed to spawn sensor thread");
            None
        }
    }
}

fn clone_provider_config(handle: &Mutex<ProvidersConfig>) -> ProvidersConfig {
    match handle.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn clone_provider_updates(handle: &Mutex<Vec<ProviderUpdateStatus>>) -> Vec<ProviderUpdateStatus> {
    match handle.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn clone_pawnio_status(handle: &Mutex<PawnioStatus>) -> PawnioStatus {
    match handle.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn headless_pawnio_status(providers: &ProvidersConfig) -> PawnioStatus {
    let install = detect_pawnio_install();
    let installed_version = install
        .as_ref()
        .and_then(|install| install.version.clone())
        .or_else(|| {
            detect_pawnio()
                .and_then(|pawnio| pawnio.version)
                .map(|version| version.as_string())
        });
    let installed = install.is_some();
    let (state, detail) = if !providers.pawnio {
        (
            PawnioState::Disabled,
            "optional elevated PawnIO helper is disabled",
        )
    } else if !installed {
        (
            PawnioState::NotInstalled,
            "PawnIO is not installed; install it manually from pawnio.eu",
        )
    } else {
        (
            PawnioState::Stopped,
            "headless mode never launches the elevated helper; use Settings to enable it",
        )
    };
    PawnioStatus {
        installed,
        installed_version,
        enabled: providers.pawnio,
        state,
        temperature_c: None,
        helper_version: None,
        official_url: oxide_core::provider_update::PAWNIO_OFFICIAL_URL.into(),
        detail: detail.into(),
        last_error: None,
        updated_at_unix_ms: Some(now_unix_ms()),
    }
}

fn sensor_loop(
    config: Config,
    shared: Arc<Mutex<Snapshot>>,
    shutdown: Arc<AtomicBool>,
    provider_config: Arc<Mutex<ProvidersConfig>>,
    provider_updates: Arc<Mutex<Vec<ProviderUpdateStatus>>>,
    pawnio_status: Arc<Mutex<PawnioStatus>>,
) {
    let mut sensors = NativeSensors::new(config);
    let mut current_providers = clone_provider_config(&provider_config);
    let mut external = ExternalSensors::new(current_providers.clone());
    let mut panic_count: u64 = 0;

    while !shutdown.load(Ordering::Relaxed) {
        // Pick up Settings Apply / hot reload between ticks. No UI blocking: the lock
        // is held only for a cheap clone.
        let next_providers = clone_provider_config(&provider_config);
        if next_providers != current_providers {
            tracing::info!(
                hwinfo = next_providers.hwinfo,
                lhm_http = next_providers.lhm_http,
                lhm_url = %next_providers.lhm_url,
                "live provider configuration changed"
            );
            external.reconfigure(next_providers.clone());
            current_providers = next_providers;
        }

        let polled = catch_unwind(AssertUnwindSafe(|| sensors.poll()));

        match polled {
            Ok(mut snapshot) => {
                let readings = external.poll();
                apply_provider_readings(&mut snapshot, &readings, external.source());
                snapshot.providers = external.statuses();
                snapshot.provider_updates = clone_provider_updates(&provider_updates);
                snapshot.pawnio = clone_pawnio_status(&pawnio_status);
                apply_pawnio_temp(&mut snapshot);
                if panic_count > 0 {
                    tracing::info!(failures = panic_count, "sensor polling recovered");
                    panic_count = 0;
                }
                // The sensors own `seq`/`captured_at`; publish the snapshot
                // unchanged except for optional provider temperatures/status.
                publish(&shared, snapshot);
            }
            Err(payload) => {
                panic_count = panic_count.saturating_add(1);
                let message = logging::panic_message(payload.as_ref());
                tracing::error!(
                    failures = panic_count,
                    panic = %message,
                    backoff_ms = PANIC_BACKOFF.as_millis(),
                    "sensor poll panicked; backing off"
                );
                thread::sleep(PANIC_BACKOFF);
                continue;
            }
        }

        thread::sleep(POLL_INTERVAL);
    }

    tracing::debug!("sensor thread stopped");
}

/// Background official-version checks and LHM staging. Never blocks the UI/sensor threads.
fn spawn_update_thread(
    provider_config: Arc<Mutex<ProvidersConfig>>,
    provider_updates: Arc<Mutex<Vec<ProviderUpdateStatus>>>,
    shutdown: Arc<AtomicBool>,
) {
    let spawn = thread::Builder::new()
        .name("oxide-updates".into())
        .spawn(move || update_loop(provider_config, provider_updates, shutdown));
    if let Err(err) = spawn {
        tracing::warn!(%err, "failed to spawn provider update thread; version checks disabled");
    }
}

fn update_loop(
    provider_config: Arc<Mutex<ProvidersConfig>>,
    provider_updates: Arc<Mutex<Vec<ProviderUpdateStatus>>>,
    shutdown: Arc<AtomicBool>,
) {
    let paths = ProviderPaths::per_user();
    let fetcher = HttpFetcher::new(paths.cache_root());
    let downloader = HttpDownloader::new();
    let verifier = WindowsVerifier;
    let mut last_config: Option<ProvidersConfig> = None;
    let mut last_check: Option<std::time::Instant> = None;

    while !shutdown.load(Ordering::Relaxed) {
        let config = clone_provider_config(&provider_config);
        let due = last_check.is_none()
            || last_config.as_ref() != Some(&config)
            || last_check.is_some_and(|checked| checked.elapsed() >= UPDATE_CHECK_INTERVAL);
        if due {
            let installed = InstalledState {
                lhm: resolve_installed_lhm(
                    scan_managed_lhm(&paths.lhm_root()),
                    find_running_lhm(),
                    &paths.lhm_root(),
                ),
                pawnio: detect_pawnio(),
            };
            let manager = UpdateManager {
                paths: &paths,
                fetcher: &fetcher,
                downloader: &downloader,
                verifier: &verifier,
                now_unix_ms: now_unix_ms(),
            };
            let report = manager.evaluate(&config, &installed);
            if let Ok(mut guard) = provider_updates.lock() {
                *guard = report;
            }
            last_config = Some(config);
            last_check = Some(std::time::Instant::now());
        }

        // Wake periodically so Settings Apply and shutdown are noticed promptly.
        let mut slept = Duration::ZERO;
        while slept < UPDATE_WAKE_INTERVAL && !shutdown.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_secs(1));
            slept += Duration::from_secs(1);
        }
    }
    tracing::debug!("provider update thread stopped");
}

/// Background PawnIO helper manager. It never launches the elevated helper at
/// monitor startup: only an explicit Start action or a false→true provider toggle
/// after startup can do that.
fn spawn_pawnio_thread(
    provider_config: Arc<Mutex<ProvidersConfig>>,
    status: Arc<Mutex<PawnioStatus>>,
    game_mode: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    commands: mpsc::Receiver<PawnioCommand>,
) -> Option<JoinHandle<()>> {
    match thread::Builder::new()
        .name("oxide-pawnio".into())
        .spawn(move || pawnio_loop(provider_config, status, game_mode, shutdown, commands))
    {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::warn!(%err, "failed to spawn PawnIO helper manager; helper disabled");
            None
        }
    }
}

fn pawnio_loop(
    provider_config: Arc<Mutex<ProvidersConfig>>,
    status: Arc<Mutex<PawnioStatus>>,
    game_mode: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    commands: mpsc::Receiver<PawnioCommand>,
) {
    let mut controller = PawnioController::default();
    let mut install = detect_pawnio_install();
    let mut installed_version = pawnio_installed_version(install.as_ref());
    let mut last_config = clone_provider_config(&provider_config);
    let initial = PawnioEvent::Initial {
        enabled: last_config.pawnio,
        installed: install.is_some(),
    };
    let _ = controller.on(initial);

    let mut client: Option<HelperClient> = None;
    let mut helper: Option<HelperProcess> = None;
    let mut temperature: Option<f32> = None;
    let mut last_read = Instant::now();
    let mut starting_since: Option<Instant> = None;
    let mut last_game_mode = game_mode.load(Ordering::Relaxed);
    let mut last_install_check = Instant::now();
    let mut last_publish = Instant::now();
    let mut published_state = controller.state();
    let mut published_temperature = None;

    publish_pawnio_status(
        &status,
        &controller,
        installed_version.as_deref(),
        temperature,
        None,
    );

    while !shutdown.load(Ordering::Relaxed) {
        let mut actions = Vec::new();

        while let Ok(command) = commands.try_recv() {
            let event = match command {
                PawnioCommand::Start => PawnioEvent::StartRequested,
                PawnioCommand::Stop => PawnioEvent::StopRequested,
            };
            actions.extend(controller.on(event));
        }

        let config = clone_provider_config(&provider_config);
        let mut installed_now = install.is_some();
        if last_install_check.elapsed() >= Duration::from_secs(60) {
            install = detect_pawnio_install();
            installed_version = pawnio_installed_version(install.as_ref());
            installed_now = install.is_some();
            last_install_check = Instant::now();
        }
        if config.pawnio != last_config.pawnio || installed_now != controller.installed() {
            actions.extend(controller.on(PawnioEvent::ConfigChanged {
                enabled: config.pawnio,
                installed: installed_now,
            }));
            last_config = config;
        }

        let current_game_mode = game_mode.load(Ordering::Relaxed);
        if current_game_mode != last_game_mode {
            actions.extend(controller.on(PawnioEvent::GameMode(current_game_mode)));
            last_game_mode = current_game_mode;
        }

        // Publish "starting" before ShellExecuteEx blocks on the UAC prompt.
        if controller.state() == PawnioState::Starting && published_state != PawnioState::Starting {
            publish_pawnio_status(
                &status,
                &controller,
                installed_version.as_deref(),
                None,
                None,
            );
            published_state = PawnioState::Starting;
            published_temperature = None;
            last_publish = Instant::now();
        }

        for action in actions {
            match action {
                PawnioAction::Launch => {
                    if client.is_some() || helper.is_some() {
                        continue;
                    }
                    // If a helper from a previous monitor session is still alive, reuse it
                    // instead of showing a redundant UAC prompt.
                    if let Ok(existing) = HelperClient::connect(Duration::from_millis(300)) {
                        tracing::info!("connected to an already-running PawnIO helper");
                        client = Some(existing);
                        starting_since = None;
                        last_read = Instant::now();
                        let _ = controller.on(PawnioEvent::Connected);
                        continue;
                    }
                    match launch_helper() {
                        Ok(process) => {
                            tracing::info!(
                                pid = process.pid(),
                                "launched PawnIO helper (UAC accepted)"
                            );
                            helper = Some(process);
                            starting_since = Some(Instant::now());
                        }
                        Err(oxide_pawnio_host::HostError::UacDenied) => {
                            tracing::warn!("PawnIO helper UAC prompt was declined; not retrying");
                            let _ = controller.on(PawnioEvent::LaunchDenied);
                        }
                        Err(err) => {
                            tracing::warn!(%err, "PawnIO helper failed to launch");
                            let _ = controller.on(PawnioEvent::LaunchFailed(err.to_string()));
                            stop_pawnio_helper(&mut client, &mut helper);
                            temperature = None;
                            starting_since = None;
                        }
                    }
                }
                PawnioAction::Stop => {
                    stop_pawnio_helper(&mut client, &mut helper);
                    temperature = None;
                    starting_since = None;
                }
            }
        }

        if controller.state() == PawnioState::Starting {
            if helper.is_none() {
                let _ = controller.on(PawnioEvent::LaunchFailed(
                    "helper process disappeared before connecting".into(),
                ));
            } else {
                match HelperClient::connect(Duration::from_millis(250)) {
                    Ok(connected) => {
                        tracing::info!(
                            pawnio_api = format_args!("{:#010X}", connected.hello().pawnio_api),
                            "PawnIO helper connected"
                        );
                        client = Some(connected);
                        starting_since = None;
                        last_read = Instant::now();
                        let _ = controller.on(PawnioEvent::Connected);
                    }
                    Err(err) => {
                        let exited = helper
                            .as_ref()
                            .is_some_and(|process| process.wait(Duration::ZERO));
                        if exited {
                            tracing::warn!(%err, "PawnIO helper exited before connecting");
                            let _ = controller.on(PawnioEvent::Disconnected);
                            stop_pawnio_helper(&mut client, &mut helper);
                            starting_since = None;
                        } else if starting_since
                            .is_some_and(|since| since.elapsed() >= Duration::from_secs(15))
                        {
                            tracing::warn!(%err, "timed out waiting for the PawnIO helper pipe");
                            let _ = controller.on(PawnioEvent::LaunchFailed(format!(
                                "timed out waiting for the helper pipe: {err}"
                            )));
                            stop_pawnio_helper(&mut client, &mut helper);
                            starting_since = None;
                        }
                    }
                }
            }
        }

        if controller.state() == PawnioState::Running {
            let helper_exited = helper
                .as_ref()
                .is_some_and(|process| process.wait(Duration::ZERO));
            if helper_exited || client.is_none() {
                tracing::warn!("PawnIO helper stopped unexpectedly");
                let _ = controller.on(PawnioEvent::Disconnected);
                stop_pawnio_helper(&mut client, &mut helper);
                temperature = None;
            } else if last_read.elapsed() >= Duration::from_secs(1) {
                match client
                    .as_ref()
                    .expect("client checked above")
                    .read_cpu_temp(Duration::from_secs(2))
                {
                    Ok(reading) => {
                        temperature = Some(reading.temp_c);
                        last_read = Instant::now();
                    }
                    Err(err) => {
                        tracing::warn!(%err, "PawnIO temperature read failed; stopping helper");
                        let _ = controller.on(PawnioEvent::ReadFailed(err.to_string()));
                        stop_pawnio_helper(&mut client, &mut helper);
                        temperature = None;
                    }
                }
            }
        }

        let state = controller.state();
        if state != published_state
            || temperature != published_temperature
            || last_publish.elapsed() >= Duration::from_secs(1)
        {
            publish_pawnio_status(
                &status,
                &controller,
                installed_version.as_deref(),
                temperature,
                client.is_some().then_some(HELPER_VERSION),
            );
            published_state = state;
            published_temperature = temperature;
            last_publish = Instant::now();
        }

        thread::sleep(Duration::from_millis(100));
    }

    let _ = controller.on(PawnioEvent::Shutdown);
    stop_pawnio_helper(&mut client, &mut helper);
    tracing::debug!("PawnIO helper manager stopped");
}

fn pawnio_installed_version(install: Option<&oxide_pawnio_host::PawnioInstall>) -> Option<String> {
    install
        .and_then(|install| install.version.clone())
        .or_else(|| {
            detect_pawnio()
                .and_then(|pawnio| pawnio.version)
                .map(|version| version.as_string())
        })
}

fn publish_pawnio_status(
    handle: &Mutex<PawnioStatus>,
    controller: &PawnioController,
    installed_version: Option<&str>,
    temperature: Option<f32>,
    helper_version: Option<&str>,
) {
    let running = controller.state() == PawnioState::Running;
    let status = PawnioStatus {
        installed: controller.installed(),
        installed_version: installed_version.map(str::to_string),
        enabled: controller.enabled(),
        state: controller.state(),
        temperature_c: running.then_some(temperature).flatten(),
        helper_version: helper_version.map(str::to_string),
        official_url: oxide_core::provider_update::PAWNIO_OFFICIAL_URL.into(),
        detail: controller.detail().to_string(),
        last_error: controller.last_error().map(str::to_string),
        updated_at_unix_ms: Some(now_unix_ms()),
    };
    match handle.lock() {
        Ok(mut guard) => *guard = status,
        Err(poisoned) => *poisoned.into_inner() = status,
    }
}

fn stop_pawnio_helper(client: &mut Option<HelperClient>, helper: &mut Option<HelperProcess>) {
    if let Some(client) = client.take() {
        let _ = client.shutdown();
    }
    if let Some(helper) = helper.take()
        && !helper.wait(Duration::from_secs(2))
    {
        helper.terminate();
        let _ = helper.wait(Duration::from_secs(1));
    }
}

fn apply_pawnio_temp(snapshot: &mut Snapshot) {
    if snapshot.pawnio.state != PawnioState::Running {
        return;
    }
    let Some(temp_c) = snapshot.pawnio.temperature_c else {
        return;
    };
    if temp_c.is_finite() {
        // An explicitly enabled/consented direct CPU sensor takes precedence over
        // ACPI and the external providers; the source is shown as PawnIO.
        snapshot.cpu.temp_c = Some(temp_c);
        snapshot.cpu.temp_source = TempSource::PawnIo;
    }
}

fn apply_provider_readings(
    snapshot: &mut Snapshot,
    readings: &ProviderReadings,
    source: ProviderSource,
) {
    let temp_source = match source {
        ProviderSource::Hwinfo => TempSource::Hwinfo,
        ProviderSource::Lhm => TempSource::Lhm,
        ProviderSource::None => TempSource::None,
    };

    // A provider-backed CPU temperature is more accurate than the ACPI zone, so
    // it replaces the native value. GPU/disk temperatures are only filled when
    // the native source did not produce one (NVML/NVMe take precedence).
    if let Some(value) = readings.cpu_temp_c {
        snapshot.cpu.temp_c = Some(value);
        snapshot.cpu.temp_source = temp_source;
    }
    if snapshot.gpu.temp_c.is_none()
        && let Some(value) = readings.gpu_temp_c
    {
        snapshot.gpu.temp_c = Some(value);
        snapshot.gpu.temp_source = temp_source;
    }
    if snapshot.disk.temp_c.is_none()
        && let Some(value) = readings.disk_temp_c
    {
        snapshot.disk.temp_c = Some(value);
        snapshot.disk.temp_source = temp_source;
    }
}

fn publish(shared: &Mutex<Snapshot>, snapshot: Snapshot) {
    match shared.lock() {
        Ok(mut current) => *current = snapshot,
        Err(poisoned) => {
            tracing::warn!("snapshot mutex was poisoned; recovering");
            *poisoned.into_inner() = snapshot;
        }
    }
}

/// Join a background thread with a bounded wait so a stuck provider/helper cannot
/// keep the process alive forever.
fn join_thread(handle: JoinHandle<()>, name: &'static str) {
    let (done_tx, done_rx) = mpsc::channel();
    let waiter = thread::Builder::new()
        .name(format!("oxide-{name}-join"))
        .spawn(move || {
            let _ = handle.join();
            let _ = done_tx.send(());
        });

    match waiter {
        Ok(_) => match done_rx.recv_timeout(JOIN_TIMEOUT) {
            Ok(()) => tracing::debug!("{name} thread joined"),
            Err(_) => tracing::warn!(
                timeout_ms = JOIN_TIMEOUT.as_millis(),
                "the {name} thread did not stop in time; detaching"
            ),
        },
        Err(err) => tracing::warn!(%err, "failed to spawn {name} join watchdog; detaching"),
    }
}

/// Per-monitor-v2 DPI awareness, set before any window exists.
fn enable_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };

    if let Err(err) =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
    {
        // Not fatal: the manifest or the OS may already have set awareness.
        tracing::debug!(%err, "SetProcessDpiAwarenessContext failed");
    }
}

/// Ask an already running instance to reveal its overlay.
///
/// The overlay window class is registered per process, so `FindWindowW` by class
/// name does not work across processes; enumerate and match the class instead.
fn show_existing_instance() -> bool {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, PostMessageW};
    use windows::core::BOOL;

    unsafe extern "system" fn find_overlay(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: the caller passes a pointer to a local `Option<HWND>` that
        // outlives the synchronous `EnumWindows` call.
        let found = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };
        let mut class = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut class) };
        if len > 0 && String::from_utf16_lossy(&class[..len as usize]) == "OxideOverlay" {
            *found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }

    let mut found: Option<HWND> = None;
    // SAFETY: `find_overlay` only writes to the `found` pointer for the duration
    // of this call and returns a valid BOOL.
    unsafe {
        let _ = EnumWindows(Some(find_overlay), LPARAM(&mut found as *mut _ as isize));
    }

    match found {
        Some(hwnd) => unsafe {
            PostMessageW(Some(hwnd), oxide_ui::SHOW_MESSAGE, WPARAM(0), LPARAM(0)).is_ok()
        },
        None => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_flag_is_parsed_without_forcing_show() {
        let args = Args::parse_from(["--autostart"]);
        assert!(args.autostart);
        assert!(!args.show);
        assert!(!args.headless);
        assert!(!args.once);
    }

    #[test]
    fn existing_flags_still_parse_together() {
        let args = Args::parse_from(["--headless", "--once", "--show"]);
        assert!(args.headless);
        assert!(args.once);
        assert!(args.show);
        assert!(!args.autostart);
        assert!(!args.help);
    }

    #[test]
    fn help_short_and_long_forms_parse() {
        assert!(Args::parse_from(["--help"]).help);
        assert!(Args::parse_from(["-h"]).help);
    }

    #[test]
    fn pawnio_temperature_overrides_the_cpu_only_while_running() {
        let mut snapshot = Snapshot::default();
        snapshot.cpu.temp_c = Some(48.0);
        snapshot.cpu.temp_source = TempSource::Acpi;
        snapshot.pawnio.state = PawnioState::Stopped;
        snapshot.pawnio.temperature_c = Some(72.0);
        apply_pawnio_temp(&mut snapshot);
        assert_eq!(snapshot.cpu.temp_c, Some(48.0));
        assert_eq!(snapshot.cpu.temp_source, TempSource::Acpi);

        snapshot.pawnio.state = PawnioState::Running;
        apply_pawnio_temp(&mut snapshot);
        assert_eq!(snapshot.cpu.temp_c, Some(72.0));
        assert_eq!(snapshot.cpu.temp_source, TempSource::PawnIo);
    }

    #[test]
    fn headless_pawnio_status_never_claims_running() {
        let status = headless_pawnio_status(&ProvidersConfig::default());
        assert!(!status.enabled);
        assert_eq!(status.state, PawnioState::Disabled);
        assert_eq!(status.temperature_c, None);
        assert!(status.detail.contains("disabled"));
    }
}
