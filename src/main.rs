mod protocol;
mod solarxr;

use openxr as xr;
use protocol::{message_envelope::Body, DeviceStatus, Position, UniverseChange};
use serde::Serialize;
use std::{
    collections::HashMap,
    env,
    error::Error,
    io::{self, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

fn handle_signals() -> io::Result<()> {
    // The handler only stores a lock-free atomic; cleanup happens on the main thread.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = interrupt as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct Pose {
    position: [f32; 3],
    // OpenVR / Rebocap ordering: w, x, y, z. Coordinates: +Y up, -Z forward.
    rotation: [f64; 4],
}

impl From<xr::Posef> for Pose {
    fn from(p: xr::Posef) -> Self {
        Self {
            position: [p.position.x, p.position.y, p.position.z],
            rotation: [
                f64::from(p.orientation.w),
                f64::from(p.orientation.x),
                f64::from(p.orientation.y),
                f64::from(p.orientation.z),
            ],
        }
    }
}

#[derive(Serialize)]
struct Output<'a> {
    device: &'a str,
    id: i32,
    role: Option<i32>,
    name: Option<&'a str>,
    connected: bool,
    battery: Option<f32>,
    pose: Option<&'a Pose>,
}

fn emit(value: &Output<'_>) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")
}

#[derive(Default)]
struct Tracker {
    role: Option<i32>,
    name: String,
    connected: bool,
    battery: Option<f32>,
}

fn receive_trackers(
    rx: &Receiver<Option<Body>>,
    trackers: &mut HashMap<i32, Tracker>,
    solarxr: Option<&solarxr::Server>,
) -> (bool, bool) {
    let mut received = false;
    loop {
        let msg = match rx.try_recv() {
            Ok(msg) => {
                received = true;
                msg
            }
            Err(mpsc::TryRecvError::Empty) => return (true, received),
            Err(mpsc::TryRecvError::Disconnected) => return (false, received),
        };
        if let (Some(server), Some(body)) = (solarxr, &msg) {
            server.receive(body);
        }
        match msg {
            Some(Body::TrackerAdded(d)) => {
                let t = trackers.entry(d.tracker_id).or_default();
                t.role = Some(d.tracker_role);
                t.name = d.tracker_name;
            }
            Some(Body::TrackerStatus(s)) => {
                let t = trackers.entry(s.tracker_id).or_default();
                t.connected = s.status == 1;
                t.battery = Some(s.battery_level);
                let _ = emit(&Output {
                    device: "tracker",
                    id: s.tracker_id,
                    role: t.role,
                    name: Some(&t.name),
                    connected: t.connected,
                    battery: t.battery,
                    pose: None,
                });
            }
            Some(Body::Position(p)) if p.pos.len() == 3 && p.q.len() == 4 => {
                let t = trackers.entry(p.tracker_id).or_default();
                let pose = Pose {
                    position: [p.pos[0], p.pos[1], p.pos[2]],
                    rotation: [p.q[0], p.q[1], p.q[2], p.q[3]],
                };
                let _ = emit(&Output {
                    device: "tracker",
                    id: p.tracker_id,
                    role: t.role,
                    name: Some(&t.name),
                    connected: t.connected,
                    battery: t.battery,
                    pose: Some(&pose),
                });
            }
            _ => {}
        }
    }
}

fn connect(addr: SocketAddr) -> io::Result<(TcpStream, Receiver<Option<Body>>)> {
    let socket = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    socket.set_nodelay(true)?;
    let mut reader = socket.try_clone()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(message) = protocol::receive(&mut reader) {
            if tx.send(message.body).is_err() {
                break;
            }
        }
    });
    Ok((socket, rx))
}

fn configure_hands(
    instance: &xr::Instance,
    session: &xr::Session<xr::Headless>,
) -> Result<(xr::ActionSet, [xr::Space; 2])> {
    let hands = [
        instance.string_to_path("/user/hand/left")?,
        instance.string_to_path("/user/hand/right")?,
    ];
    let set = instance.create_action_set("tracking", "Tracking", 0)?;
    let action = set.create_action::<xr::Posef>("grip_pose", "Grip pose", &hands)?;
    // The runtime chooses whichever suggested profile matches the connected controllers.
    for profile in [
        "/interaction_profiles/oculus/touch_controller",
        "/interaction_profiles/valve/index_controller",
        "/interaction_profiles/htc/vive_controller",
        "/interaction_profiles/khr/simple_controller",
    ] {
        let profile = instance.string_to_path(profile)?;
        let bindings = [
            xr::Binding::new(
                &action,
                instance.string_to_path("/user/hand/left/input/grip/pose")?,
            ),
            xr::Binding::new(
                &action,
                instance.string_to_path("/user/hand/right/input/grip/pose")?,
            ),
        ];
        // A runtime can reject profiles it doesn't support. Other profiles remain usable.
        let _ = instance.suggest_interaction_profile_bindings(profile, &bindings);
    }
    session.attach_action_sets(&[&set])?;
    let spaces = [
        action.create_space(session, hands[0], xr::Posef::IDENTITY)?,
        action.create_space(session, hands[1], xr::Posef::IDENTITY)?,
    ];
    Ok((set, spaces))
}

fn run(addr: SocketAddr, solarxr: Option<&solarxr::Server>) -> Result<()> {
    let entry = xr::Entry::linked();
    let available = entry.enumerate_extensions()?;
    if !available.mnd_headless || !available.khr_convert_timespec_time {
        return Err(
            "OpenXR runtime requires XR_MND_headless and XR_KHR_convert_timespec_time".into(),
        );
    }
    let mut extensions = xr::ExtensionSet::default();
    extensions.mnd_headless = true;
    extensions.khr_convert_timespec_time = true;
    let instance = entry.create_instance(
        &xr::ApplicationInfo {
            application_name: "monad2steamvr",
            application_version: 1,
            engine_name: "monad2steamvr",
            engine_version: 1,
            api_version: xr::Version::new(1, 0, 0),
        },
        &extensions,
        &[],
    )?;
    let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let (session, _, _) = unsafe {
        instance.create_session::<xr::Headless>(system, &xr::headless::SessionCreateInfo {})?
    };
    let available_spaces = session.enumerate_reference_spaces()?;
    if !available_spaces.contains(&xr::ReferenceSpaceType::STAGE) {
        return Err(
            "WiVRn does not expose STAGE space; cannot provide floor-relative Rebocap poses".into(),
        );
    }
    let stage =
        session.create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)?;
    let view = session.create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)?;
    let (actions, hands) = configure_hands(&instance, &session)?;
    let mut event_buffer = xr::EventDataBuffer::new();
    let mut started = false;
    let mut connected: Option<(TcpStream, Receiver<Option<Body>>)> = None;
    let mut last_connect = Instant::now() - Duration::from_secs(2);
    let mut connect_error_reported = false;
    let mut last_status: Option<bool> = None;
    let mut last_head_valid: Option<bool> = None;
    let mut handshake_sent = false;
    let mut first_reply_seen = false;
    let mut bridge_wait_started = Instant::now();
    let mut bridge_wait_reported = false;
    let mut trackers = HashMap::new();
    eprintln!("Waiting for WiVRn headset and Rebocap bridge at {addr}");

    loop {
        if INTERRUPTED.load(Ordering::Relaxed) {
            return Ok(());
        }
        while let Some(event) = instance.poll_event(&mut event_buffer)? {
            if let xr::Event::SessionStateChanged(e) = event {
                if e.state() == xr::SessionState::READY && !started {
                    session.begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                    started = true;
                } else if e.state() == xr::SessionState::STOPPING && started {
                    session.end()?;
                    started = false;
                    last_status = None;
                    if let Some(server) = solarxr {
                        server.clear();
                    }
                } else if matches!(
                    e.state(),
                    xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING
                ) {
                    return Ok(());
                }
            }
        }

        if connected.is_none() && last_connect.elapsed() >= Duration::from_secs(1) {
            last_connect = Instant::now();
            match connect(addr) {
                Ok((socket, rx)) => {
                    connected = Some((socket, rx));
                    connect_error_reported = false;
                    last_status = None;
                    handshake_sent = false;
                    first_reply_seen = false;
                    bridge_wait_started = Instant::now();
                    bridge_wait_reported = false;
                    trackers.clear();
                    if let Some(server) = solarxr {
                        server.clear();
                    }
                    eprintln!("TCP bridge connected; Wine named pipe not yet confirmed");
                }
                Err(e) if !connect_error_reported => {
                    eprintln!("Rebocap bridge unavailable: {e}; no pose data reaches Rebocap. Start bridge.exe with the same WINEPREFIX as rebocap.exe (see README.md).");
                    connect_error_reported = true;
                }
                Err(_) => {}
            }
        }

        if !started {
            thread::sleep(Duration::from_millis(20));
            continue;
        }

        // Monado's headless xrWaitFrame does not produce a frame or a display time.
        // Locate spaces at the runtime's monotonic time instead of using frame APIs.
        let time = instance.now()?;
        let head = view.locate(&stage, time)?;
        let valid = head
            .location_flags
            .contains(xr::SpaceLocationFlags::POSITION_VALID)
            && head
                .location_flags
                .contains(xr::SpaceLocationFlags::ORIENTATION_VALID);
        if last_head_valid != Some(valid) {
            eprintln!("WiVRn headset tracking valid: {valid}");
            last_head_valid = Some(valid);
        }
        let pose = valid.then(|| Pose::from(head.pose));

        if let Some((socket, rx)) = &mut connected {
            // A disconnected socket is detected by the read thread even if no writes are pending.
            let (alive, received) = receive_trackers(rx, &mut trackers, solarxr);
            if received && !first_reply_seen {
                eprintln!("Received a Rebocap protocol message via the Wine pipe");
                first_reply_seen = true;
            }
            if !alive {
                connected = None;
                connect_error_reported = false;
                last_status = None;
                if let Some(server) = solarxr {
                    server.clear();
                }
                eprintln!("Rebocap bridge disconnected");
            } else {
                let result = (|| -> io::Result<()> {
                    if last_status != Some(valid) {
                        protocol::send(
                            socket,
                            Body::TrackerStatus(DeviceStatus {
                                tracker_id: 0,
                                status: i32::from(valid),
                                battery_level: 0.0,
                            }),
                        )?;
                        if last_status.is_none() {
                            // STAGE is floor-relative: the Rebocap standing-space offset is zero.
                            protocol::send(
                                socket,
                                Body::UniverseChange(UniverseChange {
                                    yaw: 0.0,
                                    pos: vec![0.0; 3],
                                }),
                            )?;
                        }
                        last_status = Some(valid);
                        if !handshake_sent {
                            eprintln!("Sent Rebocap headset status and standing-space handshake to TCP bridge (headset valid: {valid})");
                            handshake_sent = true;
                        }
                    }
                    if let Some(p) = &pose {
                        protocol::send(
                            socket,
                            Body::Position(Position {
                                tracker_id: 0,
                                q: p.rotation.to_vec(),
                                pos: p.position.to_vec(),
                            }),
                        )?;
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    eprintln!("Rebocap bridge write failed: {e}");
                    connected = None;
                    connect_error_reported = false;
                    last_status = None;
                    if let Some(server) = solarxr {
                        server.clear();
                    }
                } else if !first_reply_seen
                    && !bridge_wait_reported
                    && bridge_wait_started.elapsed() >= Duration::from_secs(5)
                {
                    eprintln!("No reply from Rebocap yet. Check bridge.exe says 'pipe open, relaying'; Rebocap may also remain silent before VR mode/calibration.");
                    bridge_wait_reported = true;
                }
            }
        }

        emit(&Output {
            device: "headset",
            id: 0,
            role: None,
            name: None,
            connected: valid,
            battery: None,
            pose: pose.as_ref(),
        })?;
        if session
            .sync_actions(&[xr::ActiveActionSet::new(&actions)])
            .is_ok()
        {
            for (i, hand) in hands.iter().enumerate() {
                let loc = hand.locate(&stage, time)?;
                let tracked = loc
                    .location_flags
                    .contains(xr::SpaceLocationFlags::POSITION_VALID)
                    && loc
                        .location_flags
                        .contains(xr::SpaceLocationFlags::ORIENTATION_VALID);
                let hand_pose = tracked.then(|| Pose::from(loc.pose));
                emit(&Output {
                    device: if i == 0 {
                        "left_controller"
                    } else {
                        "right_controller"
                    },
                    id: i as i32 + 1,
                    role: None,
                    name: None,
                    connected: tracked,
                    battery: None,
                    pose: hand_pose.as_ref(),
                })?;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

const USAGE: &str = "Usage: monad2steamvr [--bridge IP:PORT] [--solarxr]
  --solarxr                  Publish Rebocap FBT to WiVRn via SolarXR IPC
  --tracker-roles 0,5,6       Rebocap roles to register (default: waist and feet)
  --solarxr-offset X,Y,Z      Output translation in metres (default: 0,0,0)
  --solarxr-yaw DEGREES       Output yaw about +Y (default: 0)
Start with --solarxr BEFORE connecting the WiVRn headset. Requires XDG_RUNTIME_DIR.
HMD/controller and Rebocap poses are also emitted as NDJSON.";

struct Options {
    addr: SocketAddr,
    solarxr: bool,
    roles: Vec<i32>,
    offset: [f32; 3],
    yaw: f64,
}

fn options(args: impl Iterator<Item = String>) -> Result<Option<Options>> {
    let mut args = args;
    let mut opts = Options {
        addr: "127.0.0.1:36850".parse()?,
        solarxr: false,
        roles: vec![0, 5, 6],
        offset: [0.0; 3],
        yaw: 0.0,
    };
    let mut output_options = false;
    while let Some(flag) = args.next() {
        if flag == "--help" || flag == "-h" {
            return Ok(None);
        }
        if flag == "--solarxr" {
            opts.solarxr = true;
            continue;
        }
        if !matches!(
            flag.as_str(),
            "--bridge" | "--tracker-roles" | "--solarxr-offset" | "--solarxr-yaw"
        ) {
            return Err(format!("Unknown option: {flag}").into());
        }
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {flag}"))?;
        match flag.as_str() {
            "--bridge" => opts.addr = value.parse()?,
            "--tracker-roles" => {
                opts.roles = solarxr::parse_roles(&value)?;
                output_options = true;
            }
            "--solarxr-offset" => {
                let values: Vec<f32> = value
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<_, _>>()?;
                opts.offset = values
                    .try_into()
                    .map_err(|_| "--solarxr-offset needs X,Y,Z")?;
                if !opts.offset.iter().all(|x| x.is_finite()) {
                    return Err("offset must be finite".into());
                }
                output_options = true;
            }
            "--solarxr-yaw" => {
                let degrees: f64 = value.parse()?;
                if !degrees.is_finite() {
                    return Err("yaw must be finite".into());
                }
                opts.yaw = degrees.rem_euclid(360.0).to_radians();
                output_options = true;
            }
            _ => unreachable!(),
        }
    }
    if output_options && !opts.solarxr {
        return Err("SolarXR options require --solarxr".into());
    }
    Ok(Some(opts))
}

fn main() {
    let opts = match options(env::args().skip(1)) {
        Ok(Some(opts)) => opts,
        Ok(None) => {
            println!("{USAGE}");
            return;
        }
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };
    let result = (|| -> Result<()> {
        handle_signals()?;
        // Binding and enumeration must work before OpenXR waits for a headset.
        let server = opts
            .solarxr
            .then(|| solarxr::Server::start(opts.roles, opts.offset, opts.yaw))
            .transpose()?;
        let mut waiting_reported = false;
        loop {
            if INTERRUPTED.load(Ordering::Relaxed) {
                return Ok(());
            }
            match run(opts.addr, server.as_ref()) {
                Ok(()) if server.is_none() || INTERRUPTED.load(Ordering::Relaxed) => return Ok(()),
                Ok(()) => {}
                Err(e) => {
                    let retry = server.is_some()
                        && e.downcast_ref::<xr::sys::Result>().is_some_and(|e| {
                            matches!(
                                *e,
                                xr::sys::Result::ERROR_RUNTIME_UNAVAILABLE
                                    | xr::sys::Result::ERROR_RUNTIME_FAILURE
                                    | xr::sys::Result::ERROR_INITIALIZATION_FAILED
                                    | xr::sys::Result::ERROR_FORM_FACTOR_UNAVAILABLE
                                    | xr::sys::Result::ERROR_INSTANCE_LOST
                                    | xr::sys::Result::ERROR_SESSION_LOST
                            )
                        });
                    if !retry {
                        return Err(e);
                    }
                    if !waiting_reported {
                        eprintln!("WiVRn session unavailable: {e}; SolarXR remains ready while waiting for the headset");
                        waiting_reported = true;
                    }
                }
            }
            if let Some(server) = &server {
                server.clear();
            }
            thread::sleep(Duration::from_secs(1));
        }
    })();
    if let Err(e) = result {
        eprintln!("monad2steamvr: {e}");
        std::process::exit(1);
    }
}
