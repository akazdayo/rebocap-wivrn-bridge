//! SolarXR IPC subset consumed by WiVRn 26.9 / Monado f037264d.
//! Field numbers follow SlimeVR/SolarXR-Protocol and Monado's solarxr/protocol.c.
use crate::{protocol::message_envelope::Body, Pose};
use std::{
    collections::HashMap,
    env, fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

// Rebocap role, SolarXR BodyPart, display name. Head/hand replacement is not FBT.
const ROLES: &[(i32, u8, &str)] = &[
    (0, 4, "waist"),
    (1, 6, "left_upper_leg"),
    (2, 7, "right_upper_leg"),
    (3, 8, "left_lower_leg"),
    (4, 9, "right_lower_leg"),
    (5, 10, "left_foot"),
    (6, 11, "right_foot"),
    (7, 3, "chest"),
    (9, 16, "left_upper_arm"),
    (10, 17, "right_upper_arm"),
    (11, 14, "left_lower_arm"),
    (12, 15, "right_lower_arm"),
];
const MAX_FRAME: usize = 1024 * 1024;
const POSE_TIMEOUT: Duration = Duration::from_millis(500);
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

pub fn parse_roles(value: &str) -> io::Result<Vec<i32>> {
    let roles: Vec<i32> = value
        .split(',')
        .map(|s| s.parse().map_err(|_| invalid()))
        .collect::<io::Result<_>>()?;
    if roles.is_empty()
        || roles
            .iter()
            .enumerate()
            .any(|(i, r)| !ROLES.iter().any(|(role, _, _)| role == r) || roles[..i].contains(r))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tracker roles must be unique values from 0..7,9..12",
        ));
    }
    Ok(roles)
}

#[derive(Default)]
struct Slot {
    status: Option<(bool, Instant)>,
    pose: Option<(Pose, Instant)>,
}

impl Slot {
    fn pose_at(&self, now: Instant) -> Option<&Pose> {
        let (ok, status_time) = self.status?;
        let (pose, pose_time) = self.pose.as_ref()?;
        (ok && now.duration_since(status_time) < STATUS_TIMEOUT
            && now.duration_since(*pose_time) < POSE_TIMEOUT)
            .then_some(pose)
    }
}

struct State {
    roles: Vec<i32>,
    ids: HashMap<i32, i32>,
    slots: HashMap<i32, Slot>,
    offset: [f32; 3],
    yaw: f64,
}

impl State {
    fn receive(&mut self, body: &Body) {
        if let Body::TrackerAdded(d) = body {
            if let Some(old) = self.ids.insert(d.tracker_id, d.tracker_role) {
                if old != d.tracker_role {
                    self.slots.remove(&old);
                }
            }
            return;
        }
        let id = match body {
            Body::TrackerStatus(s) => s.tracker_id,
            Body::Position(p) => p.tracker_id,
            _ => return,
        };
        let Some(role) = self
            .ids
            .get(&id)
            .copied()
            .filter(|r| self.roles.contains(r))
        else {
            return;
        };
        let slot = self.slots.entry(role).or_default();
        match body {
            Body::TrackerStatus(s) => {
                slot.status = Some((s.status == 1, Instant::now()));
                if s.status != 1 {
                    slot.pose = None;
                }
            }
            Body::Position(p) => {
                slot.pose = validated_pose(&p.pos, &p.q)
                    .map(|pose| transform(pose, self.offset, self.yaw))
                    .filter(|pose| pose.position.iter().all(|x| x.is_finite()))
                    .map(|pose| (pose, Instant::now()));
            }
            _ => {}
        }
    }

    fn clear(&mut self) {
        self.ids.clear();
        self.slots.clear();
    }
}

fn validated_pose(pos: &[f32], q: &[f64]) -> Option<Pose> {
    if pos.len() != 3
        || q.len() != 4
        || !pos.iter().all(|x| x.is_finite())
        || !q.iter().all(|x| x.is_finite())
    {
        return None;
    }
    let norm = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !norm.is_finite() || norm < 1e-6 {
        return None;
    }
    Some(Pose {
        position: [pos[0], pos[1], pos[2]],
        rotation: std::array::from_fn(|i| q[i] / norm),
    })
}

fn transform(mut pose: Pose, offset: [f32; 3], yaw: f64) -> Pose {
    let (sin, cos) = yaw.sin_cos();
    let [x, y, z] = pose.position.map(f64::from);
    pose.position = [
        (cos * x + sin * z) as f32 + offset[0],
        y as f32 + offset[1],
        (-sin * x + cos * z) as f32 + offset[2],
    ];
    let (s, c) = (yaw / 2.0).sin_cos();
    let [w, x, y, z] = pose.rotation;
    pose.rotation = [c * w - s * y, c * x + s * z, c * y + s * w, c * z - s * x];
    pose
}

pub struct Server {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    paths: Vec<(PathBuf, u64)>,
}

impl Server {
    pub fn start(roles: Vec<i32>, offset: [f32; 3], yaw: f64) -> io::Result<Self> {
        let dir = env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "--solarxr requires XDG_RUNTIME_DIR",
            )
        })?;
        let state = Arc::new(Mutex::new(State {
            roles,
            ids: HashMap::new(),
            slots: HashMap::new(),
            offset,
            yaw,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let mut server = Self {
            state,
            stop,
            worker: None,
            paths: vec![],
        };
        let mut listeners = vec![];
        for name in ["SlimeVRRpc", "SlimeVRInput"] {
            let path = PathBuf::from(&dir).join(name);
            // Never unlink an existing service or an unfamiliar stale socket.
            let listener = UnixListener::bind(&path).map_err(|e| {
                io::Error::new(e.kind(), format!("cannot bind {}: {e}", path.display()))
            })?;
            server
                .paths
                .push((path.clone(), fs::symlink_metadata(&path)?.ino()));
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            listeners.push(listener);
        }
        let state = server.state.clone();
        let stop = server.stop.clone();
        server.worker = Some(thread::spawn(move || serve(listeners, state, stop)));
        eprintln!(
            "SolarXR ready at {}; connect the headset after this message",
            server.paths[0].0.display()
        );
        Ok(server)
    }

    pub fn receive(&self, body: &Body) {
        self.state.lock().unwrap().receive(body);
    }
    pub fn clear(&self) {
        self.state.lock().unwrap().clear();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        for (path, inode) in &self.paths {
            if fs::symlink_metadata(path).is_ok_and(|m| m.ino() == *inode) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

struct Client {
    socket: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    pending_since: Instant,
    feeder: bool,
    streaming: bool,
    settings: bool,
    poll: bool,
}

impl Client {
    fn tick(&mut self, state: &State) -> io::Result<()> {
        let mut buf = [0; 4096];
        loop {
            match self.socket.read(&mut buf) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    self.input.extend_from_slice(&buf[..n]);
                    if self.input.len() > MAX_FRAME {
                        return Err(invalid());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        while self.input.len() >= 4 {
            let size = u32::from_le_bytes(self.input[..4].try_into().unwrap()) as usize;
            if !(8..=MAX_FRAME).contains(&size) {
                return Err(invalid());
            }
            if self.input.len() < size {
                break;
            }
            if !self.feeder {
                let request = requests(&self.input[4..size])?;
                self.settings |= request.settings || request.start;
                self.poll |= request.poll;
                self.streaming |= request.start;
            }
            // WiVRn's feeder sends native HMD/controller protobufs. The CLI already
            // reads them through OpenXR, so consume them without feeding them back.
            self.input.drain(..size);
        }
        if self.written == self.output.len() && (self.poll || self.settings || self.streaming) {
            self.output = packet(state, self.poll, self.settings);
            self.written = 0;
            self.pending_since = Instant::now();
            self.poll = false;
            self.settings = false;
        }
        while self.written < self.output.len() {
            match self.socket.write(&self.output[self.written..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.written += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        if self.written < self.output.len() && self.pending_since.elapsed() > Duration::from_secs(1)
        {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

fn serve(listeners: Vec<UnixListener>, state: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    // ponytail: one worker and up to eight clients; enough for one WiVRn runtime.
    let mut clients: Vec<Client> = vec![];
    while !stop.load(Ordering::Relaxed) && !crate::INTERRUPTED.load(Ordering::Relaxed) {
        for (i, listener) in listeners.iter().enumerate() {
            if let Ok((socket, _)) = listener.accept() {
                if clients.len() < 8 && socket.set_nonblocking(true).is_ok() {
                    eprintln!(
                        "SolarXR {} connected",
                        if i == 0 { "runtime" } else { "pose feeder" }
                    );
                    clients.push(Client {
                        socket,
                        input: vec![],
                        output: vec![],
                        written: 0,
                        pending_since: Instant::now(),
                        feeder: i == 1,
                        streaming: false,
                        settings: false,
                        poll: false,
                    });
                }
            }
        }
        let state = state.lock().unwrap();
        clients.retain_mut(|client| match client.tick(&state) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("SolarXR client disconnected: {e}");
                false
            }
        });
        drop(state);
        thread::sleep(Duration::from_millis(10));
    }
    let mut state = state.lock().unwrap();
    state.clear();
    let invalid_poses = packet(&state, false, false);
    for client in clients.iter_mut().filter(|c| !c.feeder && c.streaming) {
        // Finish any partial frame before the final invalidation. A bounded write
        // avoids hanging shutdown on a runtime that has stopped reading.
        let _ = (|| -> io::Result<()> {
            client.socket.set_nonblocking(false)?;
            client
                .socket
                .set_write_timeout(Some(Duration::from_millis(200)))?;
            client.socket.write_all(&client.output[client.written..])?;
            client.socket.write_all(&invalid_poses)
        })();
    }
}

// Small forward-only FlatBuffers encoder: only the tables WiVRn consumes.
// Offsets and structs are four-byte aligned; absent fields have vtable offset 0.
struct Buffer(Vec<u8>);
impl Buffer {
    fn align(&mut self) {
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
    }
    fn put(&mut self, at: usize, data: &[u8]) {
        self.0[at..at + data.len()].copy_from_slice(data);
    }
    fn link(&mut self, at: usize, target: usize) {
        self.put(at, &((target - at) as u32).to_le_bytes());
    }
    fn table(&mut self, fields: &[u16], size: u16) -> usize {
        self.align();
        let vtable = self.0.len();
        self.0
            .extend_from_slice(&((fields.len() * 2 + 4) as u16).to_le_bytes());
        self.0.extend_from_slice(&size.to_le_bytes());
        for field in fields {
            self.0.extend_from_slice(&field.to_le_bytes());
        }
        self.align();
        let table = self.0.len();
        self.0.resize(table + usize::from(size), 0);
        self.put(table, &((table - vtable) as i32).to_le_bytes());
        table
    }
    fn vector(&mut self, len: usize) -> usize {
        self.align();
        let at = self.0.len();
        self.0.extend_from_slice(&(len as u32).to_le_bytes());
        self.0.resize(at + 4 + len * 4, 0);
        at
    }
    fn string(&mut self, value: &str) -> usize {
        self.align();
        let at = self.0.len();
        self.0
            .extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.0.extend_from_slice(value.as_bytes());
        self.0.push(0);
        at
    }
    fn floats(&mut self, at: usize, values: &[f32]) {
        for (i, value) in values.iter().enumerate() {
            self.put(at + i * 4, &value.to_le_bytes());
        }
    }
}

fn packet(state: &State, info: bool, settings: bool) -> Vec<u8> {
    let mut b = Buffer(vec![0; 8]); // IPC length followed by FlatBuffers root offset
    let root = b.table(&[4, if settings { 8 } else { 0 }], 12);
    b.link(4, root);
    let feeds = b.vector(1);
    b.link(root + 4, feeds);
    let header = b.table(&[4, 8], 12);
    b.link(feeds + 4, header);
    b.0[header + 4] = 3; // DataFeedUpdate
    let update = b.table(&[0, 4, 0], 8);
    b.link(header + 8, update);
    let trackers = b.vector(state.roles.len());
    b.link(update + 4, trackers);
    let now = Instant::now();
    for (i, role) in state.roles.iter().enumerate() {
        let (_, part, name) = ROLES.iter().find(|(r, _, _)| r == role).unwrap();
        let pose = state.slots.get(role).and_then(|slot| slot.pose_at(now));
        let t = b.table(
            &[
                4,
                if info { 8 } else { 0 },
                0,
                if pose.is_some() { 12 } else { 0 },
                if pose.is_some() { 28 } else { 0 },
                if pose.is_none() { 40 } else { 0 },
            ],
            52,
        );
        b.link(trackers + 4 + i * 4, t);
        if let Some(pose) = pose {
            let [w, x, y, z] = pose.rotation.map(|x| x as f32);
            b.floats(t + 12, &[x, y, z, w]);
            b.floats(t + 28, &pose.position);
        }
        // Monado ignores TrackerData.status and missing poses don't clear history.
        // A velocity-only sample pushes a relation with NO valid pose flags instead.
        let id = b.table(&[0, 4], 8); // no device_id: a synthetic (solved) tracker
        b.0[id + 4] = (*role + 3) as u8;
        b.link(t + 4, id);
        if info {
            let details = b.table(&[0, 4, 0, 0, 0, 0, 0, 8], 12);
            b.0[details + 4] = *part;
            b.link(t + 8, details);
            let name = b.string(&format!("rebocap_{name}"));
            b.link(details + 8, name);
        }
    }
    if settings {
        let rpc = b.vector(1);
        b.link(root + 8, rpc);
        let header = b.table(&[0, 4, 8], 12);
        b.link(rpc + 4, header);
        b.0[header + 4] = 7; // SettingsResponse
        let response = b.table(&[4], 8);
        b.link(header + 8, response);
        let flags = b.table(&[4, 5, 0, 0, 0, 0, 0, 6, 7, 8, 9, 10, 11, 12, 13], 16);
        b.link(response + 4, flags);
        // Enable all role groups. Per-tracker validity is carried by relation flags.
        b.0[flags + 4..flags + 14].fill(1);
    }
    let size = b.0.len() as u32;
    b.put(0, &size.to_le_bytes());
    b.0
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid SolarXR frame")
}

// Bounds-checked reader for the request envelope (unknown message types are ignored).
struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn u16(&self, at: usize) -> io::Result<usize> {
        Ok(u16::from_le_bytes(
            self.0
                .get(at..at.checked_add(2).ok_or_else(invalid)?)
                .ok_or_else(invalid)?
                .try_into()
                .unwrap(),
        ) as usize)
    }
    fn u32(&self, at: usize) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.0
                .get(at..at.checked_add(4).ok_or_else(invalid)?)
                .ok_or_else(invalid)?
                .try_into()
                .unwrap(),
        ))
    }
    fn target(&self, at: usize) -> io::Result<usize> {
        let offset = self.u32(at)? as usize;
        let target = at.checked_add(offset).ok_or_else(invalid)?;
        if offset == 0 || target >= self.0.len() {
            return Err(invalid());
        }
        Ok(target)
    }
    fn field(&self, table: usize, index: usize, width: usize) -> io::Result<Option<usize>> {
        let distance = self.u32(table)? as i32;
        let vtable = (table as i64 - i64::from(distance))
            .try_into()
            .map_err(|_| invalid())?;
        let len = self.u16(vtable)?;
        let size = self.u16(vtable + 2)?;
        if len < 4
            || len % 2 != 0
            || size < 4
            || vtable + len > self.0.len()
            || table + size > self.0.len()
        {
            return Err(invalid());
        }
        if 4 + index * 2 >= len {
            return Ok(None);
        }
        let offset = self.u16(vtable + 4 + index * 2)?;
        if offset == 0 {
            return Ok(None);
        }
        if offset < 4 || offset + width > size {
            return Err(invalid());
        }
        Ok(Some(table + offset))
    }
    fn headers(&self, root: usize, index: usize, type_field: usize) -> io::Result<Vec<u8>> {
        let Some(field) = self.field(root, index, 4)? else {
            return Ok(vec![]);
        };
        let vector = self.target(field)?;
        let len = self.u32(vector)? as usize;
        if len > (self.0.len() - vector - 4) / 4 {
            return Err(invalid());
        }
        (0..len)
            .map(|i| {
                let table = self.target(vector + 4 + i * 4)?;
                let message = self.field(table, type_field + 1, 4)?.ok_or_else(invalid)?;
                self.field(self.target(message)?, 0, 0)?; // validate the payload's table bounds
                self.field(table, type_field, 1)
                    .map(|field| field.map_or(0, |at| self.0[at]))
            })
            .collect()
    }
}

#[derive(Default)]
struct Requests {
    settings: bool,
    poll: bool,
    start: bool,
}
fn requests(bytes: &[u8]) -> io::Result<Requests> {
    let r = Reader(bytes);
    let root = r.target(0)?;
    let feeds = r.headers(root, 0, 0)?;
    Ok(Requests {
        settings: r.headers(root, 1, 1)?.contains(&6),
        poll: feeds.contains(&1),
        start: feeds.contains(&2),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DeviceNew, DeviceStatus, Position};

    fn state() -> State {
        State {
            roles: vec![0, 5, 6],
            ids: HashMap::new(),
            slots: HashMap::new(),
            offset: [0.5, 0.0, -0.5],
            yaw: std::f64::consts::FRAC_PI_2,
        }
    }

    fn track(state: &mut State) {
        // Actual Rebocap IDs need not equal role+3. SolarXR serials stay role-based.
        state.receive(&Body::TrackerAdded(DeviceNew {
            tracker_id: 42,
            tracker_role: 0,
            tracker_name: "waist".into(),
        }));
        state.receive(&Body::TrackerStatus(DeviceStatus {
            tracker_id: 42,
            status: 1,
            battery_level: 0.8,
        }));
        state.receive(&Body::Position(Position {
            tracker_id: 42,
            q: vec![2.0, 0.0, 0.0, 0.0],
            pos: vec![1.0, 2.0, 3.0],
        }));
    }

    // One settings request + poll, as WiVRn sends before device enumeration.
    fn request(start: bool) -> Vec<u8> {
        let mut b = Buffer(vec![0; 8]);
        let root = b.table(&[4, 8], 12);
        b.link(4, root);
        let feeds = b.vector(1);
        b.link(root + 4, feeds);
        let header = b.table(&[4, 8], 12);
        b.link(feeds + 4, header);
        b.0[header + 4] = if start { 2 } else { 1 };
        let config = b.table(&[], 4);
        b.link(header + 8, config);
        let rpc = b.vector(1);
        b.link(root + 8, rpc);
        let header = b.table(&[0, 4, 8], 12);
        b.link(rpc + 4, header);
        b.0[header + 4] = 6;
        let settings = b.table(&[], 4);
        b.link(header + 8, settings);
        let len = b.0.len() as u32;
        b.put(0, &len.to_le_bytes());
        b.0
    }

    fn read_packet(socket: &mut UnixStream) -> Vec<u8> {
        let mut header = [0; 4];
        socket.read_exact(&mut header).unwrap();
        let len = u32::from_le_bytes(header) as usize;
        let mut packet = vec![0; len];
        packet[..4].copy_from_slice(&header);
        socket.read_exact(&mut packet[4..]).unwrap();
        packet
    }

    #[test]
    fn ipc_registration_streaming_and_invalidation() {
        let mut state = state();
        let (mut runtime, socket) = UnixStream::pair().unwrap();
        runtime
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut client = Client {
            socket,
            input: vec![],
            output: vec![],
            written: 0,
            pending_since: Instant::now(),
            feeder: false,
            streaming: false,
            settings: false,
            poll: false,
        };
        let poll = request(false);
        // Fragment the frame header/payload: nonblocking IPC must not lose bytes.
        runtime.write_all(&poll[..2]).unwrap();
        client.tick(&state).unwrap();
        assert!(client.output.is_empty());
        runtime.write_all(&poll[2..]).unwrap();
        client.tick(&state).unwrap();
        assert_eq!(read_packet(&mut runtime), packet(&state, true, true));
        runtime.write_all(&request(true)).unwrap();
        client.tick(&state).unwrap();
        assert_eq!(read_packet(&mut runtime), packet(&state, false, true));
        track(&mut state);
        let pose = state.slots[&0].pose_at(Instant::now()).unwrap();
        assert_eq!(pose.position, [3.5, 2.0, -1.5]);
        assert!((pose.rotation[0] - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((pose.rotation[2] - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        client.tick(&state).unwrap();
        assert_eq!(read_packet(&mut runtime), packet(&state, false, false));
        let stale = Instant::now() + POSE_TIMEOUT;
        assert!(state.slots[&0].pose_at(stale).is_none());
        state.receive(&Body::TrackerStatus(DeviceStatus {
            tracker_id: 42,
            status: 0,
            battery_level: 0.0,
        }));
        assert!(state.slots[&0].pose.is_none());
        track(&mut state);
        state.receive(&Body::Position(Position {
            tracker_id: 42,
            q: vec![0.0; 4],
            pos: vec![1.0; 3],
        }));
        assert!(state.slots[&0].pose.is_none());
        assert!(validated_pose(&[f32::NAN, 0.0, 0.0], &[1.0, 0.0, 0.0, 0.0]).is_none());
        track(&mut state);
        state.clear();
        assert!(state.slots.is_empty());
        assert!(state.ids.is_empty());
        // Every truncation must be rejected without a panic.
        for end in 0..poll.len() - 4 {
            assert!(requests(&poll[4..4 + end]).is_err());
        }
        runtime.write_all(&u32::MAX.to_le_bytes()).unwrap();
        assert!(client.tick(&state).is_err());
        assert_eq!(parse_roles("0,5,6").unwrap(), vec![0, 5, 6]);
        for bad in ["", "0,0", "8", "13", "-1", "0,no"] {
            assert!(parse_roles(bad).is_err());
        }

        let opts = crate::options(
            [
                "--solarxr",
                "--tracker-roles",
                "0,5,6",
                "--solarxr-offset",
                "1,2,3",
                "--solarxr-yaw",
                "-90",
            ]
            .map(String::from)
            .into_iter(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(opts.offset, [1.0, 2.0, 3.0]);
        assert!((opts.yaw - 3.0 * std::f64::consts::FRAC_PI_2).abs() < 1e-6);
        for args in [
            vec!["--solarxr-yaw", "0"],
            vec!["--solarxr", "--solarxr-offset", "NaN,0,0"],
            vec!["--solarxr", "--solarxr-yaw", "inf"],
            vec!["--bridge"],
            vec!["--unknown"],
        ] {
            assert!(crate::options(args.into_iter().map(String::from)).is_err());
        }

        // Exercise the real worker and verify that shutdown ends with invalid poses.
        let dir = env::temp_dir().join(format!("monad2steamvr-ipc-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("rpc");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        track(&mut state);
        let shared = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let shared = shared.clone();
            let stop = stop.clone();
            thread::spawn(move || serve(vec![listener], shared, stop))
        };
        let mut runtime = UnixStream::connect(&path).unwrap();
        runtime
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        runtime.write_all(&request(false)).unwrap();
        assert_eq!(
            read_packet(&mut runtime),
            packet(&shared.lock().unwrap(), true, true)
        );
        runtime.write_all(&request(true)).unwrap();
        read_packet(&mut runtime);
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        let mut remaining = vec![];
        runtime.read_to_end(&mut remaining).unwrap();
        let mut at = 0;
        let mut last = None;
        while at < remaining.len() {
            let size = u32::from_le_bytes(remaining[at..at + 4].try_into().unwrap()) as usize;
            last = Some(&remaining[at..at + size]);
            at += size;
        }
        assert_eq!(last.unwrap(), packet(&shared.lock().unwrap(), false, false));
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    #[ignore = "requires MONADO_SOURCE and a C compiler; validates against the actual WiVRn parser"]
    fn monado_wire_compatibility() {
        use std::process::{Command, Stdio};
        let source = PathBuf::from(
            env::var_os("MONADO_SOURCE").expect("set MONADO_SOURCE to WiVRn's Monado source"),
        );
        let exe =
            env::temp_dir().join(format!("monad2steamvr-monado-check-{}", std::process::id()));
        let result = Command::new(env::var("CC").unwrap_or_else(|_| "cc".into()))
            .args([
                "-std=c11",
                "-D_DEFAULT_SOURCE",
                "-Wall",
                "-Wextra",
                "-Werror",
            ])
            .arg(format!("-I{}", source.join("src/xrt/include").display()))
            .arg(format!("-I{}", source.join("src/xrt/auxiliary").display()))
            .arg(format!(
                "-I{}",
                source.join("src/xrt/drivers/solarxr").display()
            ))
            .arg(source.join("src/xrt/drivers/solarxr/protocol.c"))
            .arg("tests/monado-solarxr.c")
            .arg("-o")
            .arg(&exe)
            .status()
            .unwrap();
        assert!(result.success());
        let mut child = Command::new(&exe).stdin(Stdio::piped()).spawn().unwrap();
        let mut state = state();
        let input = child.stdin.as_mut().unwrap();
        input.write_all(&packet(&state, true, true)).unwrap();
        track(&mut state);
        input.write_all(&packet(&state, false, true)).unwrap();
        state.slots.get_mut(&0).unwrap().pose.as_mut().unwrap().1 -= POSE_TIMEOUT;
        input.write_all(&packet(&state, false, false)).unwrap();
        drop(child.stdin.take());
        let status = child.wait().unwrap();
        fs::remove_file(exe).unwrap();
        assert!(status.success());
    }
}
