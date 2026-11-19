use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    battery_icon,
    debounce::{Debouncer, Kind},
    feed::{self, DeviceSnapshot, Feed},
    led,
    manager::DeviceManager,
    notify::Notify,
    options::Options,
    reminder::{Reminder, LOW_LEVEL},
    system,
    wake::{Activity, WakeRetry},
};
use log::{debug, error, info, trace, warn};
use parking_lot::Mutex;
use tao::event_loop::{EventLoopBuilder, EventLoopProxy};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder, TrayIconEvent,
};

/// Fallback poll interval when no device answers (all stale / none present).
const BATTERY_UPDATE_INTERVAL: Duration = Duration::from_secs(300);
const DEVICE_FETCH_INTERVAL: Duration = Duration::from_secs(5);
const CONNECTION_DEBOUNCE: Duration = Duration::from_secs(10);
/// Minimum gap between hover-triggered refreshes, so keeping the cursor on
/// the icon doesn't hammer the dock's RF relay.
const HOVER_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// Input idle at which the mouse was likely just docked (or parked):
/// probe immediately instead of waiting for the next scheduled poll.
const DOCK_DETECT_IDLE: Duration = Duration::from_secs(8);
/// The dock relay can alternate its charging flag on consecutive samples.
/// Publish a transition only after this many reads agree.
const CHARGING_CONFIRM_READS: u8 = 2;
/// While charging, ask only for the charging flag on the fast loop so a
/// lift-off is noticed in seconds instead of at the next battery read.
const CHARGE_PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// Window of level samples used for the time-to-full estimate.
const CHARGE_HISTORY_WINDOW: Duration = Duration::from_secs(45 * 60);

/// How far back to look for the discharge slope. The mouse loses a percent
/// every several minutes, so a long window is needed for a stable rate.
const DISCHARGE_HISTORY_WINDOW: Duration = Duration::from_secs(6 * 60 * 60);

/// Add a discharge sample at least this often, even when the level has not
/// changed, so the slope has a real time span to measure.
const DISCHARGE_SAMPLE_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// A measured slope keeps improving as samples accumulate, but a minimum span
/// stops a single noisy one-percent step from claiming a wildly wrong ETA.
const DISCHARGE_MIN_SPAN_MIN: f64 = 20.0;

/// Typical drain on this mouse, percent per hour, until a measured slope is
/// available. Mirrors the charge fallback: useful immediately, replaced by
/// real data after the first stable samples.
const TYPICAL_DISCHARGE_RATE: f64 = 8.0;

/// Typical dock charge rate for this mouse, in percent per minute. Used for
/// the time-to-full estimate until the real slope has been measured from two
/// rising samples -- roughly a percent a minute from empty to full on the dock.
const TYPICAL_CHARGE_RATE: f64 = 1.0;

/// Adaptive battery poll interval: check often when the battery is low or
/// charging, rarely when it is healthy — a full battery doesn't need
/// watching every 5 minutes. While the user is idle the mouse is likely
/// sitting on the dock, so poll every minute: that is how fast a dock drop
/// has to be noticed for the tray and the widget to feel instant.
fn adaptive_wait(level: Option<i32>, charging: bool, system_idle: bool) -> Duration {
    const MIN_1: Duration = Duration::from_secs(60);
    const MIN_2: Duration = Duration::from_secs(2 * 60);
    const MIN_5: Duration = Duration::from_secs(5 * 60);
    const MIN_10: Duration = Duration::from_secs(10 * 60);
    const MIN_30: Duration = Duration::from_secs(30 * 60);
    const H_1: Duration = Duration::from_secs(60 * 60);
    const H_2: Duration = Duration::from_secs(2 * 60 * 60);
    const H_4: Duration = Duration::from_secs(4 * 60 * 60);
    match level {
        // Nothing fresh yet (first read pending): don't wait long.
        None => MIN_5,
        // Full: nothing to watch for until it drains again.
        Some(l) if l >= 100 => H_4,
        // While charging the level climbs — keep an eye on it.
        Some(_) if charging && system_idle => MIN_1,
        Some(_) if charging => MIN_2,
        // Resting (mouse parked, user away): watch for the dock drop.
        Some(_) if system_idle => MIN_1,
        Some(l) if l <= 2 => MIN_2,
        Some(l) if l <= 6 => MIN_5,
        Some(l) if l <= 14 => MIN_10,
        Some(l) if l <= 29 => MIN_30,
        Some(l) if l <= 49 => H_1,
        Some(l) if l <= 79 => H_2,
        Some(_) => H_4,
    }
}

/// Value published as `poll`: an advisory hint for consumers that want to know
/// how soon the next HID battery read is expected.
///
/// This is deliberately separate from the feed's write cadence. `poll` never
/// meant "seconds until status.json is rewritten", and the bundled skin uses
/// its own fixed 2 s read interval rather than this field.
/// `read_this_round` is true when this tick re-read a device, which happens on
/// every level or charge-state change; 1 marks that fresh read.
fn poll_hint(level: Option<i32>, charging: bool, system_idle: bool, read_this_round: bool) -> u64 {
    if read_this_round {
        // Something just changed: the skin's fast path is poll <= 2.
        return 1;
    }
    // Nothing new. Stay responsive while the level is actively moving
    // (charging) and relax once it settles.
    let adaptive = adaptive_wait(level, charging, system_idle);
    let secs = adaptive.as_secs();
    if secs <= 15 {
        5
    } else {
        secs
    }
}

/// Fake device used by `--simulate-battery` when no mouse is plugged in.
const SIMULATED_PID: u32 = 0;

/// Windows truncates tray tooltips to 127 characters.
const TOOLTIP_MAX_CHARS: usize = 127;

#[derive(Debug)]
pub struct MemoryDevice {
    pub name: String,
    /// Last successfully read level; `None` until the first read.
    pub battery_level: Option<i32>,
    /// The last read failed (mouse asleep or out of range).
    pub stale: bool,
    pub is_charging: bool,
    full_notified: bool,
    reminder: Reminder,
    /// When to read again while the mouse doesn't answer.
    retry: WakeRetry,
    /// The charging flag exactly as the last read reported it. On the Mouse
    /// Dock Pro this is the relayed flag from the paired mouse, which is
    /// reliable: it reads 1 on the dock and 0 the moment the mouse is lifted.
    reported_charging: bool,
    /// At least one successful charging read has been seen.
    charging_known: bool,
    /// A new raw state waiting for a confirming read (state, count).
    charging_candidate: Option<(bool, u8)>,
    /// Consecutive reads where the charging flag could not be read.
    charging_failures: u8,
    /// Recent (time, level) samples while charging, for the time-to-full
    /// estimate.
    charge_history: Vec<(Instant, i32)>,
    /// Minutes to full while charging, from the level slope. `None` when
    /// unknown or not charging.
    pub eta_full_min: Option<i32>,
    /// Recent (time, level) samples while off the dock, for the time-left
    /// estimate. Cleared when charging resumes.
    discharge_history: Vec<(Instant, i32)>,
    /// Minutes until empty while off the dock. `None` when unknown or
    /// charging.
    pub eta_empty_min: Option<i32>,
}

impl MemoryDevice {
    fn new(name: String) -> Self {
        Self {
            name,
            battery_level: None,
            stale: false,
            is_charging: false,
            full_notified: false,
            reminder: Reminder::default(),
            retry: WakeRetry::default(),
            reported_charging: false,
            charging_known: false,
            charging_candidate: None,
            charging_failures: 0,
            charge_history: Vec::new(),
            eta_full_min: None,
            discharge_history: Vec::new(),
            eta_empty_min: None,
        }
    }

    /// Apply one raw charging reading. `None` means the read failed.
    /// Returns true when the published `is_charging` value changed.
    fn apply_charging_reading(&mut self, reading: Option<bool>) -> bool {
        match reading {
            Some(reported) => {
                self.charging_failures = 0;
                self.reported_charging = reported;

                if !self.charging_known {
                    self.charging_known = true;
                    self.charging_candidate = None;
                    if self.is_charging != reported {
                        self.is_charging = reported;
                        self.on_charging_changed();
                        return true;
                    }
                    return false;
                }

                if reported == self.is_charging {
                    self.charging_candidate = None;
                    return false;
                }

                match self.charging_candidate {
                    Some((candidate, count)) if candidate == reported => {
                        let count = count.saturating_add(1);
                        if count >= CHARGING_CONFIRM_READS {
                            self.charging_candidate = None;
                            self.is_charging = reported;
                            self.on_charging_changed();
                            true
                        } else {
                            self.charging_candidate = Some((reported, count));
                            false
                        }
                    }
                    _ => {
                        self.charging_candidate = Some((reported, 1));
                        false
                    }
                }
            }
            None => {
                self.charging_failures = self.charging_failures.saturating_add(1);
                self.charging_candidate = None;
                if self.charging_failures >= CHARGING_CONFIRM_READS && self.is_charging {
                    self.is_charging = false;
                    self.reported_charging = false;
                    self.on_charging_changed();
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Drop estimates that belonged to the charging state we just left.
    fn on_charging_changed(&mut self) {
        if self.is_charging {
            self.discharge_history.clear();
            self.eta_empty_min = None;
        } else {
            self.charge_history.clear();
            self.eta_full_min = None;
            // Make the tooltip useful immediately after undocking; the next
            // full read replaces this with a measured slope.
            self.eta_empty_min = self.battery_level.and_then(fallback_discharge_eta);
        }
    }

    /// True while a fast charging-only probe can still teach us something.
    fn wants_charge_probe(&self) -> bool {
        (self.is_charging || self.charging_candidate.is_some())
            && self.charging_failures < CHARGING_CONFIRM_READS
    }
}

enum WorkerCommand {
    Refresh,
    /// Close every HID handle and rebuild the device list. Useful after the
    /// dock or mouse has re-enumerated and a stale handle keeps failing.
    Reconnect,
}

pub struct TrayInner {
    tray_icon: Rc<Mutex<Option<TrayIcon>>>,
}

struct MenuItems {
    /// Live status line, not clickable.
    status: MenuItem,
    refresh: MenuItem,
    reconnect: MenuItem,
    open_log: MenuItem,
    diagnostics: MenuItem,
    autostart: CheckMenuItem,
    notifications: CheckMenuItem,
    version: MenuItem,
    quit: MenuItem,
}

/// Version label for the menu. Leaked once because the tray-icon API wants a
/// `&'static str`, and this lives for the whole process anyway.
fn version_text() -> &'static str {
    Box::leak(
        format!(
            "v{} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("BASILISK_GIT_HASH")
        )
        .into_boxed_str(),
    )
}

impl TrayInner {
    fn new() -> Self {
        Self {
            tray_icon: Rc::new(Mutex::new(None)),
        }
    }

    fn create_menu() -> (Menu, MenuItems) {
        let tray_menu = Menu::new();
        let items = MenuItems {
            status: MenuItem::new("Starting…", false, None),
            refresh: MenuItem::new("Refresh now", true, None),
            reconnect: MenuItem::new("Reconnect devices", true, None),
            open_log: MenuItem::new("Open log", true, None),
            diagnostics: MenuItem::new("Export diagnostics…", true, None),
            autostart: CheckMenuItem::new(
                "Start with Windows",
                true,
                system::autostart_enabled(),
                None,
            ),
            notifications: CheckMenuItem::new("Notifications", true, true, None),
            version: MenuItem::new(version_text(), false, None),
            quit: MenuItem::new("Exit", true, None),
        };

        if let Err(e) = tray_menu.append_items(&[
            &items.status,
            &PredefinedMenuItem::separator(),
            &items.refresh,
            &items.reconnect,
            &items.open_log,
            &items.diagnostics,
            &items.autostart,
            &items.notifications,
            &PredefinedMenuItem::separator(),
            &items.version,
            &items.quit,
        ]) {
            warn!("Failed to append menu items: {}", e);
        }
        (tray_menu, items)
    }

    fn build_tray(
        tray_icon: &Rc<Mutex<Option<TrayIcon>>>,
        tray_menu: &Menu,
        icon: tray_icon::Icon,
    ) {
        let tray_builder = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu.clone()))
            .with_tooltip("Searching for Razer devices…")
            .with_icon(icon)
            .build();

        match tray_builder {
            Ok(tray) => *tray_icon.lock() = Some(tray),
            Err(err) => error!("Failed to create tray icon: {}", err),
        }
    }
}

pub struct TrayApp {
    devices: Arc<Mutex<HashMap<u32, MemoryDevice>>>,
    tray_inner: TrayInner,
    notify: Arc<Notify>,
    /// Toggled from the tray menu; gates every toast.
    notifications_enabled: Arc<AtomicBool>,
    options: Options,
}

#[derive(Debug)]
enum TrayEvent {
    DeviceUpdate,
    MenuEvent(MenuEvent),
}

impl TrayApp {
    pub fn new(options: Options) -> Self {
        Self {
            devices: Arc::new(Mutex::new(HashMap::new())),
            tray_inner: TrayInner::new(),
            notify: Arc::new(Notify::new()),
            notifications_enabled: Arc::new(AtomicBool::new(true)),
            options,
        }
    }

    pub fn run(&self) {
        let event_loop = EventLoopBuilder::with_user_event().build();
        let icon = match Self::get_battery_icon(None, false, self.options.digits) {
            Ok(icon) => icon,
            Err(e) => {
                error!("{}", e);
                return;
            }
        };
        let (tray_menu, menu_items) = TrayInner::create_menu();

        let proxy = event_loop.create_proxy();
        let (cmd_tx, cmd_rx) = mpsc::channel();

        self.spawn_worker_thread(
            proxy.clone(),
            cmd_rx,
            Arc::clone(&self.notifications_enabled),
        );

        self.run_event_loop(
            event_loop,
            icon,
            tray_menu,
            menu_items,
            proxy,
            cmd_tx,
            self.options.digits,
            Arc::clone(&self.notifications_enabled),
        );
    }

    fn spawn_worker_thread(
        &self,
        proxy: EventLoopProxy<TrayEvent>,
        cmd_rx: Receiver<WorkerCommand>,
        notifications_enabled: Arc<AtomicBool>,
    ) {
        let devices = Arc::clone(&self.devices);
        let notify = Arc::clone(&self.notify);
        let options = self.options;

        thread::spawn(move || {
            let mut manager = DeviceManager::new();
            let mut last_devices = HashSet::new();
            let mut debouncer = Debouncer::new(CONNECTION_DEBOUNCE);
            let mut next_battery_poll = Instant::now();
            let mut next_charge_probe = Instant::now();
            let mut force_refresh = false;
            let mut last_light_theme = system::taskbar_is_light();
            let mut system_was_idle = false;
            // The dock re-asserts its charging LED state whenever the mouse is
            // docked or starts/stops charging, which shows up as the LED
            // snapping back to the colour it had while charging. Remember each
            // device's last charging state so those transitions can put the
            // saved static colours back (see the led module).
            let mut last_charging: HashMap<u32, bool> = HashMap::new();
            // Separate from last_charging: the LED resync below keeps its own
            // view so it still fires when the widget refresh consumes the
            // transition first.
            let mut led_last_charging: HashMap<u32, bool> = HashMap::new();
            // Colours re-applied at least once this run. Without this the
            // first reading of every device would re-apply on each start.
            let mut led_synced = false;
            let mut activity = Activity::default();
            // Disk hygiene runs on a slow timer rather than every tick.
            let mut next_maintenance = Instant::now() + Duration::from_secs(60);
            let mut feed = Feed::new();
            let reminder_unit = if options.fast_reminders {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(60)
            };

            loop {
                let (removed_devices, connected_devices) = manager.fetch_devices();
                let now = Instant::now();

                if now >= next_maintenance {
                    if let Some(dir) = system::data_dir() {
                        crate::maintenance::run_once(&dir);
                    }
                    next_maintenance = now + Duration::from_secs(60 * 60);
                }
                let mut ui_changed = false;

                let mut guard = devices.lock();

                for id in &removed_devices {
                    if let Some(device) = guard.remove(id) {
                        info!("Device removed: {}", device.name);
                        debouncer.push(Kind::Disconnected, &device.name, now);
                    }
                }

                for &id in &connected_devices {
                    if let Entry::Vacant(e) = guard.entry(id) {
                        if let Some(name) = manager.get_device_name(id) {
                            info!("New device: {}", name);
                            debouncer.push(Kind::Connected, &name, now);
                            e.insert(MemoryDevice::new(name));
                        } else {
                            error!("Failed to get device name for id: {}", id);
                        }
                    }
                }

                if options.simulate_battery.is_some() {
                    let has_real = guard.keys().any(|&id| id != SIMULATED_PID);
                    if !has_real && !guard.contains_key(&SIMULATED_PID) {
                        guard.insert(
                            SIMULATED_PID,
                            MemoryDevice::new("Simulated Razer Mouse".to_owned()),
                        );
                        ui_changed = true;
                    } else if has_real {
                        ui_changed |= guard.remove(&SIMULATED_PID).is_some();
                    }
                }

                // A mouse that stopped answering is retried as soon as the user
                // touches the PC again (moving the mouse wakes it), with backoff.
                let idle_time = system::idle_time();
                let input = activity.sample(now, idle_time);
                let idle_now = idle_time.is_some_and(|t| t >= DOCK_DETECT_IDLE);
                if input.returned {
                    trace!("User input after idle, retrying sleeping devices");
                    guard.values_mut().for_each(|d| d.retry.reset_backoff());
                    // Picking the mouse up (or plugging the cable) must show
                    // in the tray and the widget right away.
                    force_refresh = true;
                }
                // The moment input stops, the mouse was likely just docked —
                // probe now instead of waiting out the next scheduled poll.
                if idle_now && !system_was_idle {
                    info!("Input idle, probing for dock drop");
                    force_refresh = true;
                }
                system_was_idle = idle_now;

                let poll_due = force_refresh || now >= next_battery_poll;
                let mut read_this_round = false;
                for (&id, device) in guard.iter_mut() {
                    let needs_first_read = device.battery_level.is_none() && !device.stale;
                    let wake_retry = device.stale && device.retry.due(now, input.last_input);
                    if poll_due || connected_devices.contains(&id) || needs_first_read || wake_retry
                    {
                        read_battery(&mut manager, id, device, options, now);
                        device.retry.record(now, !device.stale);
                        check_full(device, &notify, &notifications_enabled);
                        ui_changed = true;
                        read_this_round = true;
                    }
                }

                if poll_due {
                    force_refresh = false;
                    // Poll as often as the most demanding device needs: a low
                    // or charging device tightens the interval, a healthy full
                    // battery stretches it to hours. Devices that stopped
                    // answering are retried via wake_retry (on user input);
                    // if nothing answers at all, fall back to the default.
                    let min_wait = guard
                        .values()
                        .filter(|d| !d.stale)
                        .map(|d| adaptive_wait(d.battery_level, d.is_charging, idle_now))
                        .min()
                        .unwrap_or(BATTERY_UPDATE_INTERVAL);
                    next_battery_poll = now + min_wait;
                }

                // Reminders tick every loop, independent of battery polling.
                // Cheap check, but skip it entirely while nothing is low.
                let any_candidate = guard.values().any(|d| {
                    !d.stale && !d.is_charging && d.battery_level.is_some_and(|l| l <= LOW_LEVEL)
                });
                let game_running =
                    any_candidate && (options.simulate_game || system::fullscreen_app_running());
                for device in guard.values_mut() {
                    // While the mouse doesn't answer, freeze the schedule; a due
                    // reminder fires once it responds again.
                    if device.stale {
                        continue;
                    }
                    let Some(level) = device.battery_level else {
                        continue;
                    };
                    if let Some(alert) = device.reminder.update(
                        now,
                        level,
                        device.is_charging,
                        game_running,
                        reminder_unit,
                    ) {
                        info!(
                            "{}: battery {} ({}%), alert #{}",
                            device.name,
                            if alert.critical { "critical" } else { "low" },
                            level,
                            alert.count
                        );
                        if notifications_enabled.load(Ordering::Relaxed) {
                            notify.battery_low(&device.name, level, alert);
                        }
                    }
                }
                if game_running {
                    trace!("Fullscreen app running, holding reminders");
                }

                // While charging, the battery level moves slowly; the event
                // that must be noticed quickly is the mouse leaving the dock.
                // Probe only the charging flag on the fast loop, at most once
                // per CHARGE_PROBE_INTERVAL. The full battery read (with its
                // level and time-to-full estimate) stays on the adaptive
                // schedule. A confirmed flip below forces the next full read.
                if now >= next_charge_probe {
                    if !read_this_round && options.simulate_battery.is_none() {
                        for (&id, device) in guard.iter_mut() {
                            if device.stale || !device.wants_charge_probe() {
                                continue;
                            }
                            let raw = manager.probe_charging(id);
                            if device.apply_charging_reading(raw) {
                                info!(
                                    "{}: charging state {} (fast probe)",
                                    device.name,
                                    if device.is_charging {
                                        "started"
                                    } else {
                                        "stopped"
                                    }
                                );
                                check_full(device, &notify, &notifications_enabled);
                                ui_changed = true;
                            }
                        }
                    }
                    next_charge_probe = now + CHARGE_PROBE_INTERVAL;
                }

                // Charging changes are the interesting ones for the widget: the
                // user docking or lifting the mouse should show up immediately,
                // not whenever the adaptive interval next allows (up to 4 hours
                // on a full battery). Remember what each device reported last
                // tick and short-circuit the wait when it flips.
                let mut charging_changed = false;
                for (&id, device) in guard.iter() {
                    if device.stale {
                        continue;
                    }
                    match last_charging.get(&id) {
                        Some(previous) if *previous == device.is_charging => {}
                        _ => {
                            last_charging.insert(id, device.is_charging);
                            charging_changed = true;
                        }
                    }
                }
                if charging_changed {
                    info!("charging state changed, refreshing the widget now");
                    force_refresh = true;
                    next_battery_poll = now;
                }

                // Put the saved static colours back after a charging
                // transition, which is when the dock re-asserts its own LED
                // state. Checked per device so a transition on one is not
                // masked by another.
                let mut led_resync = false;
                for (&id, device) in guard.iter() {
                    if device.stale {
                        continue;
                    }
                    // A first reading after a reconnect is as good a moment to
                    // re-apply as a charging flip: the dock re-asserts its LED
                    // state when the RF link comes back too.
                    match led_last_charging.get(&id) {
                        Some(previous) if *previous == device.is_charging => {}
                        // Previous state was charging and now it is not: the
                        // mouse was lifted off the dock, so any colour the
                        // dock left behind should be replaced with ours.
                        Some(true) => led_resync = true,
                        // Previous state was not charging and now it is: the
                        // dock is starting its own breathing effect. Leave it
                        // alone -- re-applying here is what made the dock
                        // snap back to purple instead of breathing.
                        Some(false) => {}
                        // First reading after startup: the dock may have left
                        // a stale colour, so put ours back.
                        None => led_resync = true,
                    }
                }
                // Once every live device has answered, put the saved colours
                // down once so a tray restart cannot leave the LEDs on
                // whatever the dock was last showing.
                let all_have_readings = !guard.is_empty()
                    && guard
                        .values()
                        .all(|d| !d.stale && d.battery_level.is_some());
                if read_this_round && !led_synced && all_have_readings {
                    led_synced = true;
                    led_resync = true;
                }

                if led_resync {
                    // Re-apply through the controllers the tray already holds:
                    // a fresh handle opened beside them does not reach the
                    // dock's LED pipeline.
                    let pending: Vec<(led::Target, led::Rgb)> =
                        [led::Target::Mouse, led::Target::Dock]
                            .into_iter()
                            .filter_map(|target| led::saved_color(target).map(|rgb| (target, rgb)))
                            .collect();
                    if !pending.is_empty() {
                        drop(guard);
                        for (target, rgb) in pending {
                            let Some(controller) = manager.led_controller(target) else {
                                warn!("led: no open controller for {}", target.key());
                                continue;
                            };
                            let color = led::reapply(controller, target, rgb);
                            info!(
                                "led: re-applied {} #{:02X}{:02X}{:02X}; read back {}",
                                target.key(),
                                rgb.0,
                                rgb.1,
                                rgb.2,
                                color
                                    .map(|c| format!("#{:02X}{:02X}{:02X}", c.0, c.1, c.2))
                                    .unwrap_or_else(|| "no colour".to_string())
                            );
                        }
                        guard = devices.lock();
                    }
                    led_last_charging = guard
                        .iter()
                        .filter(|(_, d)| !d.stale)
                        .map(|(&id, d)| (id, d.is_charging))
                        .collect();
                }

                for (kind, name) in debouncer.drain_due(now) {
                    if notifications_enabled.load(Ordering::Relaxed) {
                        match kind {
                            Kind::Connected => notify.device_connected(&name),
                            Kind::Disconnected => notify.device_disconnected(&name),
                        }
                    }
                }

                // Status feed for the Rainmeter skin + rolling history CSV.
                // The feed shows the same device the tray icon shows.
                let primary = guard
                    .values()
                    .filter(|d| !d.stale && d.battery_level.is_some())
                    .min_by_key(|d| (!d.is_charging, d.battery_level))
                    .map(|d| DeviceSnapshot {
                        name: &d.name,
                        level: d.battery_level,
                        charging: d.is_charging,
                        eta_full_min: d.eta_full_min,
                        // "dock" means the mouse is on the dock and
                        // charging; off the dock it is just wireless, even
                        // though the dock still relays the battery reading.
                        source: feed::source_of(&d.name, d.is_charging),
                        // How soon this device will next be read. The skin
                        // uses it to decide its own read cadence.
                        // Event-driven hint for the skin. A fresh reading
                        // means either the level moved or the charge state
                        // flipped -- both already forced a re-poll -- so tell
                        // the skin to look right now instead of waiting out
                        // the adaptive interval.
                        poll_seconds: poll_hint(
                            d.battery_level,
                            d.is_charging,
                            idle_now,
                            read_this_round,
                        ),
                    });
                feed.write_status(primary.as_ref(), now);
                feed.append_history(primary.as_ref(), now);

                let current_devices: HashSet<u32> = guard.keys().copied().collect();
                let light_theme = system::taskbar_is_light();
                if ui_changed || current_devices != last_devices || light_theme != last_light_theme
                {
                    let _ = proxy.send_event(TrayEvent::DeviceUpdate);
                }
                last_devices = current_devices;
                last_light_theme = light_theme;

                drop(guard);

                match cmd_rx.recv_timeout(DEVICE_FETCH_INTERVAL) {
                    Ok(WorkerCommand::Refresh) => {
                        info!("Manual refresh requested");
                        force_refresh = true;
                    }
                    Ok(WorkerCommand::Reconnect) => {
                        info!("Reconnect requested; reopening HID devices");
                        manager.reset();
                        force_refresh = true;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)] // event-loop plumbing, all used
    fn run_event_loop(
        &self,
        event_loop: tao::event_loop::EventLoop<TrayEvent>,
        icon: tray_icon::Icon,
        tray_menu: Menu,
        menu_items: MenuItems,
        proxy: EventLoopProxy<TrayEvent>,
        cmd_tx: Sender<WorkerCommand>,
        digits: bool,
        notifications_enabled: Arc<AtomicBool>,
    ) {
        let devices = Arc::clone(&self.devices);
        let tray_icon = Rc::clone(&self.tray_inner.tray_icon);

        let menu_channel = MenuEvent::receiver();
        let tray_event_channel = TrayIconEvent::receiver();
        let mut last_hover_refresh: Option<Instant> = None;

        event_loop.run(move |event, _, control_flow| {
            *control_flow = tao::event_loop::ControlFlow::Wait;

            match event {
                tao::event::Event::NewEvents(tao::event::StartCause::Init) => {
                    TrayInner::build_tray(&tray_icon, &tray_menu, icon.clone());
                }
                tao::event::Event::UserEvent(TrayEvent::DeviceUpdate) => {
                    Self::update_tray_ui(&devices, &tray_icon, &menu_items, digits);
                }
                tao::event::Event::UserEvent(TrayEvent::MenuEvent(event)) => {
                    if event.id == menu_items.refresh.id() {
                        let _ = cmd_tx.send(WorkerCommand::Refresh);
                    }

                    if event.id == menu_items.reconnect.id() {
                        let _ = cmd_tx.send(WorkerCommand::Reconnect);
                    }

                    if event.id == menu_items.open_log.id() {
                        if let Some(path) = system::log_path() {
                            if let Err(e) = system::open_path(&path) {
                                error!("Failed to open log: {e}");
                            }
                        }
                    }

                    if event.id == menu_items.diagnostics.id() {
                        match crate::diagnostics::write_bundle() {
                            Ok(path) => {
                                info!("Diagnostics written to {}", path.display());
                                if let Err(e) = system::open_path(&path) {
                                    error!("Failed to open diagnostics: {e}");
                                }
                            }
                            Err(e) => {
                                error!("Failed to write diagnostics: {e}");
                                system::show_error(&format!("Could not write diagnostics:\n{e}"));
                            }
                        }
                    }

                    if event.id == menu_items.autostart.id() {
                        let want = !menu_items.autostart.is_checked();
                        match system::set_autostart(want) {
                            Ok(()) => {
                                menu_items.autostart.set_checked(want);
                                info!("Autostart {}", if want { "enabled" } else { "disabled" });
                            }
                            Err(e) => {
                                error!("Failed to change autostart: {e}");
                                menu_items.autostart.set_checked(!want);
                            }
                        }
                    }

                    if event.id == menu_items.notifications.id() {
                        let now_checked = !notifications_enabled.load(Ordering::Relaxed);
                        notifications_enabled.store(now_checked, Ordering::Relaxed);
                        menu_items.notifications.set_checked(now_checked);
                        info!(
                            "Notifications {}",
                            if now_checked { "enabled" } else { "disabled" }
                        );
                    }

                    if event.id == menu_items.quit.id() {
                        *control_flow = tao::event_loop::ControlFlow::Exit;
                    }
                }
                _ => (),
            }

            if let Ok(event) = menu_channel.try_recv() {
                let _ = proxy.send_event(TrayEvent::MenuEvent(event));
            }

            // Hovering the icon asks for the latest charging data: trigger a
            // battery read (throttled) so the tooltip refreshes within a
            // second or two.
            if let Ok(tray_event) = tray_event_channel.try_recv() {
                let hover = matches!(
                    tray_event,
                    TrayIconEvent::Enter { .. } | TrayIconEvent::Move { .. }
                );
                let due = last_hover_refresh.is_none_or(|t| t.elapsed() >= HOVER_REFRESH_INTERVAL);
                if hover && due {
                    last_hover_refresh = Some(Instant::now());
                    trace!("Tray icon hover, requesting fresh battery data");
                    let _ = cmd_tx.send(WorkerCommand::Refresh);
                }
            }
        });
    }

    fn update_tray_ui(
        devices: &Arc<Mutex<HashMap<u32, MemoryDevice>>>,
        tray_icon: &Rc<Mutex<Option<TrayIcon>>>,
        menu_items: &MenuItems,
        digits: bool,
    ) {
        let guard = devices.lock();
        let mut tray = tray_icon.lock();
        let Some(tray) = tray.as_mut() else {
            return;
        };

        if guard.is_empty() {
            menu_items.status.set_text("No device connected");
            if let Ok(icon) = Self::get_battery_icon(None, false, digits) {
                let _ = tray.set_icon(Some(icon));
            }
            let _ = tray.set_tooltip(Some("No Razer devices detected"));
            return;
        }

        let mut list: Vec<&MemoryDevice> = guard.values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));

        // Icon: prefer a charging device (the same mouse on cable/dock), else
        // the lowest battery among devices that answer.
        let shown = list
            .iter()
            .filter(|d| !d.stale && d.battery_level.is_some())
            .min_by_key(|d| (!d.is_charging, d.battery_level));
        let icon = match shown {
            Some(d) => Self::get_battery_icon(d.battery_level, d.is_charging, digits),
            None => Self::get_battery_icon(None, false, digits),
        };
        if let Ok(icon) = icon {
            let _ = tray.set_icon(Some(icon));
        }

        let status_text = match shown {
            Some(d) => {
                let name = d.name.strip_prefix("Razer ").unwrap_or(&d.name);
                match (d.battery_level, d.is_charging) {
                    (Some(level), true) => format!("{name}: {level}% ⚡"),
                    (Some(level), false) => format!("{name}: {level}%"),
                    _ => format!("{name}: reading…"),
                }
            }
            None => "No battery reading".to_owned(),
        };
        menu_items.status.set_text(status_text);

        let tooltip = list
            .iter()
            .map(|d| tooltip_line(d))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = tray.set_tooltip(Some(truncate_chars(&tooltip, TOOLTIP_MAX_CHARS)));
    }

    /// Battery glyph (or raw digits) at the tray's pixel size, colored for
    /// the current taskbar theme. `None` draws the gray "unknown" state.
    fn get_battery_icon(
        level: Option<i32>,
        is_charging: bool,
        digits: bool,
    ) -> Result<tray_icon::Icon, String> {
        let size = system::tray_icon_size();
        let light = system::taskbar_is_light();
        let rgba = if digits {
            battery_icon::render_digits(size, level, is_charging, light)
        } else {
            let style = battery_icon::tray_style(level, is_charging, light);
            battery_icon::render(size, level.unwrap_or(0), style)
        };

        tray_icon::Icon::from_rgba(rgba, size, size)
            .map_err(|e| format!("Failed to create icon: {}", e))
    }
}

/// A 0 % report from the dock relay is an RF link blip, not an empty battery
/// (a really empty mouse would not answer at all). Simulated runs are the
/// exception: there, 0 % is a deliberate test input.
fn zero_is_link_blip(simulated: bool) -> bool {
    !simulated
}

fn read_battery(
    manager: &mut DeviceManager,
    id: u32,
    device: &mut MemoryDevice,
    options: Options,
    now: Instant,
) {
    let simulated = options.simulate_battery;

    // Charging is read first, while the dock's RF relay is idle. Reading the
    // battery first leaves the link busy and the charging read then fails with
    // status 0x05, which used to freeze the previous charging state.
    let charging = if simulated.is_some() {
        Some(false)
    } else {
        manager.is_device_charging(id)
    };
    let level = simulated.or_else(|| manager.get_device_battery_level(id));
    let mut trusted_charging = charging;

    match level {
        Some(0) if zero_is_link_blip(simulated.is_some()) => {
            // A SUCCESS report with 0% (and usually charging=false) shows up
            // on the Mouse Dock Pro relay when the RF link to the mouse
            // blips. A truly empty battery means the mouse is off — it
            // wouldn't answer at all — so 0% while it answers is a glitch:
            // keep the last known value, don't trust the charging flag from
            // the same report, and show "not responding" only if we never
            // had a real reading.
            warn!(
                "{}: 0% reading (link blip?), keeping last known value",
                device.name
            );
            trusted_charging = None;
            if device.battery_level.is_none() {
                device.stale = true;
            }
        }
        Some(level) => {
            info!("{}  battery level: {}%", device.name, level);
            if device.battery_level.is_none() {
                // Don't announce "fully charged" for a mouse that already was.
                device.full_notified = level >= 100;
            }
            device.battery_level = Some(level);
            device.stale = false;
        }
        None => {
            if !device.stale {
                // One line per transition: a mouse that dozed off or went out
                // of range. The per-poll HID retries themselves stay at debug.
                info!("{}: no battery reading (asleep?)", device.name);
            }
            device.stale = true;
        }
    }

    // The dock relays the mouse's real charging flag on transaction 0x1F.
    // Measured on this hardware it is reliable and flips promptly:
    //   1 while the mouse is on the dock, 0 as soon as it is lifted.
    //
    // The old code OR-ed this with a level-slope inference whose latch, once
    // set, was never cleared -- that is what made the tray and widget keep
    // showing "charging" long after the mouse was off. A failed read must not
    // freeze the old value either, so it clears the state instead.
    let previous_reported = device.reported_charging;
    let charging_changed = device.apply_charging_reading(trusted_charging);
    match trusted_charging {
        Some(reported) => {
            if previous_reported != reported {
                info!(
                    "{}: charging flag {} (raw)",
                    device.name,
                    if reported { 1 } else { 0 }
                );
            }
        }
        None => {
            // A failed read is not a state. Keep the last one until the
            // failure repeats, so one busy relay reply does not make the
            // widget flicker to wireless.
            debug!(
                "{}: charging flag unavailable ({} consecutive)",
                device.name, device.charging_failures
            );
        }
    }
    if charging_changed {
        info!(
            "{}: charging state {} (confirmed)",
            device.name,
            if device.is_charging {
                "started"
            } else {
                "stopped"
            }
        );
    } else if trusted_charging.is_some() && device.charging_candidate.is_some() {
        debug!("{}: charging change waiting for confirmation", device.name);
    }

    // Time-to-full estimate from the recent level slope while charging.
    // Time-to-full estimate.
    //
    // Samples are the battery level in percent. Charging this mouse moves
    // roughly a percent every few minutes, so the slope needs a few samples
    // before it means anything -- but requiring a long window made the ETA
    // blank in practice, since a dock charge is often interrupted. Use the
    // oldest and newest sample within the window and accept a shorter span.
    if device.is_charging {
        if let Some(l) = level.filter(|l| *l > 0) {
            device.charge_history.push((now, l));
        }
    } else {
        device.charge_history.clear();
    }
    device
        .charge_history
        .retain(|(t, _)| now.duration_since(*t) <= CHARGE_HISTORY_WINDOW);

    device.eta_full_min = None;
    device.eta_empty_min = None;
    if device.is_charging {
        // At 100% there is nothing left to wait for, and the skin hides the
        // line entirely. At 99% the last percent can take far longer than the
        // slope suggests, so say one minute rather than extrapolating.
        if level == Some(99) {
            device.eta_full_min = Some(1);
        }

        // Prefer the measured slope, but only once the level has actually been
        // seen climbing across two samples. Waiting for that left the line
        // blank for the first several minutes of a dock charge, which is
        // exactly when it is wanted.
        let mut measured: Option<i32> = None;
        if device.charge_history.len() >= 2 {
            let (t0, l0) = device.charge_history[0];
            let (t1, l1) = *device.charge_history.last().unwrap();
            let span_min = (t1 - t0).as_secs_f64() / 60.0;
            let gained = l1 - l0;
            // A minute of span is enough to divide by without amplifying noise.
            if span_min >= 1.0 && gained > 0 {
                let rate = gained as f64 / span_min; // % per minute
                let remaining = (100 - l1) as f64;
                if remaining > 0.0 {
                    let eta = (remaining / rate).round() as i32;
                    measured = Some(eta.clamp(1, 600));
                }
            }
        }

        device.eta_full_min = measured.or_else(|| {
            // Fallback: this mouse charges at roughly a percent a minute on the
            // dock. Enough for a useful line while the real slope is measured.
            level
                .map(|l| ((100 - l).max(1) as f64 / TYPICAL_CHARGE_RATE).round() as i32)
                .map(|eta| eta.clamp(1, 600))
        });

        // A charge session invalidates any discharge slope.
        device.discharge_history.clear();
    } else if let Some(level) = device.battery_level.filter(|l| *l > 0) {
        let due = match device.discharge_history.last() {
            None => true,
            Some((t, previous)) => {
                *previous != level || now.duration_since(*t) >= DISCHARGE_SAMPLE_INTERVAL
            }
        };
        if due {
            device.discharge_history.push((now, level));
        }
        device
            .discharge_history
            .retain(|(t, _)| now.duration_since(*t) <= DISCHARGE_HISTORY_WINDOW);

        device.eta_empty_min = estimate_discharge(&device.discharge_history, level)
            .or_else(|| fallback_discharge_eta(level));
    }
}

/// Time until empty from the discharge samples, in minutes.
///
/// Uses the oldest and newest sample so slow, steady drain still produces an
/// answer. Returns `None` until the level has actually fallen over a long
/// enough span; the caller then falls back to a typical drain rate.
fn estimate_discharge(samples: &[(Instant, i32)], level: i32) -> Option<i32> {
    let (t0, l0) = *samples.first()?;
    let (t1, l1) = *samples.last()?;
    let span_min = (t1 - t0).as_secs_f64() / 60.0;
    let dropped = l0 - l1;
    if span_min < DISCHARGE_MIN_SPAN_MIN || dropped <= 0 || level <= 0 {
        return None;
    }
    let rate_per_min = dropped as f64 / span_min;
    let eta = (level as f64 / rate_per_min).round() as i32;
    Some(eta.clamp(1, 48 * 60))
}

/// Immediate estimate used until a real slope exists.
fn fallback_discharge_eta(level: i32) -> Option<i32> {
    if level <= 0 {
        return None;
    }
    let eta = (level as f64 / TYPICAL_DISCHARGE_RATE * 60.0).round() as i32;
    Some(eta.clamp(1, 48 * 60))
}

fn check_full(device: &mut MemoryDevice, notify: &Notify, enabled: &AtomicBool) {
    let Some(level) = device.battery_level else {
        return;
    };
    if level < 100 {
        device.full_notified = false;
    } else if device.is_charging && !device.full_notified {
        info!("{}: Battery fully charged", device.name);
        if enabled.load(Ordering::Relaxed) {
            notify.battery_full(&device.name);
        }
        device.full_notified = true;
    }
}

fn tooltip_line(device: &MemoryDevice) -> String {
    let name = device.name.strip_prefix("Razer ").unwrap_or(&device.name);
    match (device.battery_level, device.stale) {
        (_, true) => format!("{name}: not responding"),
        (None, false) => format!("{name}: reading…"),
        (Some(level), false) if device.is_charging => match device.eta_full_min {
            Some(mins) => format!("{name}: {level}% ⚡ ({})", format_eta(mins)),
            None => format!("{name}: {level}% ⚡"),
        },
        (Some(level), false) => match device.eta_empty_min {
            Some(mins) => format!("{name}: {level}% ({} left)", format_eta(mins)),
            None => format!("{name}: {level}%"),
        },
    }
}

/// "75" → "≈ 1 h 15 min", "40" → "≈ 40 min".
fn format_eta(minutes: i32) -> String {
    if minutes >= 60 {
        format!("≈ {} h {:02} min", minutes / 60, minutes % 60)
    } else {
        format!("≈ {minutes} min")
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => s[..idx].to_owned(),
        None => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(minutes_ago: u64, level: i32) -> (Instant, i32) {
        (
            Instant::now() - Duration::from_secs(minutes_ago * 60),
            level,
        )
    }

    #[test]
    fn discharge_eta_from_measured_slope() {
        // 4 points lost over 60 minutes = 4%/h; 80% left = 20 h.
        let samples = [sample(60, 84), sample(30, 82), sample(0, 80)];
        assert_eq!(estimate_discharge(&samples, 80), Some(20 * 60));
    }

    #[test]
    fn discharge_eta_ignores_short_or_rising_history() {
        let rising = [sample(20, 70), sample(0, 80)];
        assert_eq!(estimate_discharge(&rising, 80), None);
        let short = [sample(5, 80), sample(0, 79)];
        assert_eq!(estimate_discharge(&short, 79), None);
    }

    #[test]
    fn discharge_eta_is_bounded() {
        // A huge measured rate still reports a sane upper bound.
        let fast = [sample(60, 100), sample(0, 1)];
        assert_eq!(estimate_discharge(&fast, 1), Some(1));
    }

    #[test]
    fn fallback_scales_with_level() {
        assert_eq!(fallback_discharge_eta(80), Some(10 * 60));
        assert_eq!(fallback_discharge_eta(0), None);
    }

    #[test]
    fn tooltip_shows_time_left_when_idle() {
        let mut device = MemoryDevice::new("Razer Basilisk V3 Pro".to_string());
        device.battery_level = Some(80);
        device.eta_empty_min = Some(10 * 60);
        assert_eq!(
            tooltip_line(&device),
            "Basilisk V3 Pro: 80% (≈ 10 h 00 min left)"
        );

        device.eta_empty_min = None;
        assert_eq!(tooltip_line(&device), "Basilisk V3 Pro: 80%");
    }

    #[test]
    fn tooltip_prefers_charging_eta() {
        let mut device = MemoryDevice::new("Razer Basilisk V3 Pro".to_string());
        device.battery_level = Some(50);
        device.is_charging = true;
        device.eta_full_min = Some(40);
        let line = tooltip_line(&device);
        assert!(line.contains('⚡'), "line was {line}");
        assert!(line.contains("40 min"), "line was {line}");
    }

    #[test]
    fn charging_transition_needs_two_reads() {
        let mut device = MemoryDevice::new("Mouse".to_string());
        assert!(device.apply_charging_reading(Some(true)));
        assert!(device.is_charging);

        assert!(!device.apply_charging_reading(Some(false)));
        assert!(device.is_charging);

        assert!(device.apply_charging_reading(Some(false)));
        assert!(!device.is_charging);
    }

    #[test]
    fn charging_flap_is_ignored() {
        let mut device = MemoryDevice::new("Mouse".to_string());
        assert!(device.apply_charging_reading(Some(true)));

        for _ in 0..4 {
            assert!(!device.apply_charging_reading(Some(false)));
            assert!(!device.apply_charging_reading(Some(true)));
            assert!(device.is_charging);
        }
    }

    #[test]
    fn failed_charging_reads_clear_only_after_confirmation() {
        let mut device = MemoryDevice::new("Mouse".to_string());
        assert!(device.apply_charging_reading(Some(true)));

        assert!(!device.apply_charging_reading(None));
        assert!(device.is_charging);

        assert!(device.apply_charging_reading(None));
        assert!(!device.is_charging);
    }

    #[test]
    fn zero_percent_is_only_a_blip_on_real_hardware() {
        assert!(zero_is_link_blip(false));
        assert!(!zero_is_link_blip(true));
    }

    #[test]
    fn charge_probe_is_only_wanted_while_docked() {
        let mut device = MemoryDevice::new("Mouse".to_string());
        assert!(!device.wants_charge_probe());

        assert!(device.apply_charging_reading(Some(true)));
        assert!(device.wants_charge_probe());

        assert!(!device.apply_charging_reading(Some(false)));
        assert!(device.wants_charge_probe());

        assert!(device.apply_charging_reading(Some(false)));
        assert!(!device.wants_charge_probe());
    }

    #[test]
    fn failed_probe_stops_after_confirmation() {
        let mut device = MemoryDevice::new("Mouse".to_string());
        assert!(device.apply_charging_reading(Some(true)));

        assert!(!device.apply_charging_reading(None));
        assert!(device.wants_charge_probe());

        assert!(device.apply_charging_reading(None));
        assert!(!device.wants_charge_probe());
    }

    #[test]
    fn undock_clears_charging_eta_immediately() {
        let mut device = MemoryDevice::new("Mouse".to_string());
        device.battery_level = Some(75);
        assert!(device.apply_charging_reading(Some(true)));
        device.eta_full_min = Some(25);

        assert!(!device.apply_charging_reading(Some(false)));
        assert!(device.apply_charging_reading(Some(false)));

        assert!(!device.is_charging);
        assert_eq!(device.eta_full_min, None);
        assert!(device.eta_empty_min.is_some());
    }
}
