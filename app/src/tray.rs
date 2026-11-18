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
    devices,
    feed::{self, DeviceSnapshot, Feed},
    manager::DeviceManager,
    notify::Notify,
    options::Options,
    reminder::{Reminder, LOW_LEVEL},
    system,
    wake::{Activity, WakeRetry},
};
use log::{error, info, trace, warn};
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
/// A charging state inferred from the level climbing expires this long
/// after the last rise — by then the mouse must be off the dock, since
/// real charging adds roughly a percent every few minutes.
const CHARGE_LATCH_WINDOW: Duration = Duration::from_secs(6 * 60);
/// Input idle at which the mouse was likely just docked (or parked):
/// probe immediately instead of waiting for the next scheduled poll.
const DOCK_DETECT_IDLE: Duration = Duration::from_secs(8);
/// Window of level samples used for the time-to-full estimate.
const CHARGE_HISTORY_WINDOW: Duration = Duration::from_secs(45 * 60);

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
    /// Mouse Dock Pro only: the relayed charging flag lies (it read false
    /// while the mouse physically charged through the pogo pins), so
    /// charging is also inferred from the level climbing between reads.
    inferred_charging: bool,
    /// When the level last rose while docked — the inferred latch expires
    /// this long after (a real plateau on a charging mouse re-latches on
    /// the next climb).
    last_rise: Option<Instant>,
    /// The charging flag exactly as the last read reported it, before the
    /// inference is OR-ed in — latch expiry must never undo a reported
    /// charge.
    reported_charging: bool,
    /// Recent (time, level) samples while charging, for the time-to-full
    /// estimate.
    charge_history: Vec<(Instant, i32)>,
    /// Minutes to full while charging, from the level slope. `None` when
    /// unknown or not charging.
    pub eta_full_min: Option<i32>,
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
            inferred_charging: false,
            last_rise: None,
            reported_charging: false,
            charge_history: Vec::new(),
            eta_full_min: None,
        }
    }
}

enum WorkerCommand {
    Refresh,
}

pub struct TrayInner {
    tray_icon: Rc<Mutex<Option<TrayIcon>>>,
}

struct MenuItems {
    /// Live status line, not clickable.
    status: MenuItem,
    refresh: MenuItem,
    open_log: MenuItem,
    notifications: CheckMenuItem,
    quit: MenuItem,
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
            open_log: MenuItem::new("Open log", true, None),
            notifications: CheckMenuItem::new("Notifications", true, true, None),
            quit: MenuItem::new("Exit", true, None),
        };

        if let Err(e) = tray_menu.append_items(&[
            &items.status,
            &PredefinedMenuItem::separator(),
            &items.refresh,
            &items.open_log,
            &items.notifications,
            &PredefinedMenuItem::separator(),
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
            let mut force_refresh = false;
            let mut last_light_theme = system::taskbar_is_light();
            let mut system_was_idle = false;
            let mut activity = Activity::default();
            let mut feed = Feed::new();
            let reminder_unit = if options.fast_reminders {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(60)
            };

            loop {
                let (removed_devices, connected_devices) = manager.fetch_devices();
                let now = Instant::now();
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
                for (&id, device) in guard.iter_mut() {
                    let needs_first_read = device.battery_level.is_none() && !device.stale;
                    let wake_retry = device.stale && device.retry.due(now, input.last_input);
                    if poll_due || connected_devices.contains(&id) || needs_first_read || wake_retry
                    {
                        read_battery(&mut manager, id, device, options, now);
                        device.retry.record(now, !device.stale);
                        check_full(device, &notify, &notifications_enabled);
                        ui_changed = true;
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

                // Time-based expiry of the inferred charging latch: evaluated
                // every tick so a mouse taken off the dock flips to "not
                // charging" within the window even when the next scheduled
                // poll is far away. A mouse that stopped answering keeps its
                // last known state until it responds again.
                for device in guard.values_mut() {
                    if device.stale || !device.inferred_charging || device.reported_charging {
                        continue;
                    }
                    let expired = device
                        .last_rise
                        .map_or(false, |t| now.duration_since(t) > CHARGE_LATCH_WINDOW);
                    if expired {
                        device.inferred_charging = false;
                        device.last_rise = None;
                        device.is_charging = false;
                        ui_changed = true;
                        info!("{}: no level rise, no longer charging", device.name);
                    }
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
                        source: feed::source_of(&d.name),
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
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    }

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

                    if event.id == menu_items.open_log.id() {
                        if let Some(path) = system::log_path() {
                            if let Err(e) = system::open_path(&path) {
                                error!("Failed to open log: {e}");
                            }
                        }
                    }

                    if event.id == menu_items.notifications.id() {
                        let now_checked = !notifications_enabled.load(Ordering::Relaxed);
                        notifications_enabled.store(now_checked, Ordering::Relaxed);
                        let _ = menu_items.notifications.set_checked(now_checked);
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
                let due =
                    last_hover_refresh.map_or(true, |t| t.elapsed() >= HOVER_REFRESH_INTERVAL);
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
            menu_items.status.set_text("No device connected".to_owned());
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

fn read_battery(
    manager: &mut DeviceManager,
    id: u32,
    device: &mut MemoryDevice,
    options: Options,
    now: Instant,
) {
    let simulated = options.simulate_battery;
    let level = simulated.or_else(|| manager.get_device_battery_level(id));
    let charging = if simulated.is_some() {
        Some(false)
    } else if level.is_some() {
        manager.is_device_charging(id)
    } else {
        None
    };
    let mut trusted_charging = charging;
    let prev_level = device.battery_level;

    match level {
        Some(0) => {
            // A SUCCESS report with 0% (and usually charging=false) shows up
            // on the Mouse Dock Pro relay when the RF link to the mouse
            // blips. A truly empty battery means the mouse is off — it
            // wouldn't answer at all — so 0% while it answers is a glitch:
            // keep the last known value, don't trust the charging flag from
            // the same report, and show "not responding" only if we never
            // had a real reading.
            warn!("{}: 0% reading (link blip?), keeping last known value", device.name);
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
                warn!("{}: no battery reading (asleep?)", device.name);
            }
            device.stale = true;
        }
    }

    if let Some(reported) = trusted_charging {
        // The dock's relayed charging flag lies (it read false while the
        // mouse physically charged through the pogo pins), so for the dock
        // the flag is OR-ed with level-slope inference. The latch is
        // anchored to the last level rise and expires a few minutes later:
        // a mouse that isn't charging can't keep gaining percent.
        if id == devices::RAZER_MOUSE_DOCK_PRO.pid as u32 {
            device.reported_charging = reported;
            let cur = device.battery_level;
            let rose = matches!((prev_level, cur), (Some(p), Some(n)) if n > p);
            let fell = matches!((prev_level, cur), (Some(p), Some(n)) if n < p);
            if rose {
                device.inferred_charging = true;
                device.last_rise = Some(now);
            }
            if reported {
                device.inferred_charging = true;
                if device.last_rise.is_none() {
                    device.last_rise = Some(now);
                }
            }
            if fell {
                // A falling battery is definitive proof of discharge.
                device.inferred_charging = false;
                device.last_rise = None;
            }
            let latch_expired = device
                .last_rise
                .map_or(false, |t| now.duration_since(t) > CHARGE_LATCH_WINDOW);
            if latch_expired && !reported && !rose {
                device.inferred_charging = false;
                device.last_rise = None;
            }
            let effective = reported || device.inferred_charging;
            info!(
                "{}  charging status: {} (reported {}, inferred {})",
                device.name, effective, reported, device.inferred_charging
            );
            device.is_charging = effective;
        } else {
            device.reported_charging = reported;
            info!("{}  charging status: {}", device.name, reported);
            device.is_charging = reported;
        }
    }

    // Time-to-full estimate from the recent level slope while charging.
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
    if device.is_charging {
        if let (Some((t0, l0)), Some((_, l1))) =
            (device.charge_history.first(), device.charge_history.last())
        {
            let span_min = now.duration_since(*t0).as_secs_f64() / 60.0;
            let rate = (*l1 - *l0) as f64 / span_min; // % per minute
            let remaining = (100 - *l1) as f64;
            if span_min >= 5.0 && rate > 0.02 && remaining > 0.5 {
                let eta = (remaining / rate).round() as i32;
                device.eta_full_min = Some(eta.clamp(1, 600));
            }
        }
    }
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
        (Some(level), false) => format!("{name}: {level}%"),
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
