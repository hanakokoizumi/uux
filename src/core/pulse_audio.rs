//! Native PulseAudio / PipeWire-Pulse routing. All I/O is on audio workers.
use anyhow::{Context as _, Result, bail, ensure};
use libpulse_binding as pulse;
use pulse::{
    context::{Context, State as ContextState},
    mainloop::standard::{IterateResult, Mainloop},
    sample::{Format, Spec},
    stream::{FlagSet, PeekResult, State, Stream},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

pub fn available() -> bool {
    std::env::var_os("PULSE_SERVER").is_some()
        || std::env::var_os("XDG_RUNTIME_DIR")
            .is_some_and(|p| std::path::PathBuf::from(p).join("pulse/native").exists())
}
/// Outer Option selects this backend; inner Option follows the server default.
pub fn selection(id: Option<&str>) -> Option<Option<&str>> {
    match id {
        Some(id) => id
            .strip_prefix("pulse:")
            .map(|name| if name == "default" { None } else { Some(name) }),
        None if available() => Some(None),
        _ => None,
    }
}
struct Connection {
    context: Context,
    mainloop: Mainloop,
}
impl Connection {
    fn new() -> Result<Self> {
        let mainloop = Mainloop::new().context("无法创建音频事件循环")?;
        let mut context = Context::new(&mainloop, "UUX").context("无法创建音频连接")?;
        context.connect(None, pulse::context::FlagSet::NOAUTOSPAWN, None)?;
        let mut value = Self { context, mainloop };
        let start = Instant::now();
        loop {
            value.pump()?;
            match value.context.get_state() {
                ContextState::Ready => {
                    value.pump()?;
                    return Ok(value);
                }
                ContextState::Failed | ContextState::Terminated => bail!("音频服务不可用"),
                _ => {}
            }
            ensure!(start.elapsed() < Duration::from_secs(1), "连接音频服务超时");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn pump(&mut self) -> Result<()> {
        for _ in 0..16 {
            match self.mainloop.iterate(false) {
                IterateResult::Success(0) => break,
                IterateResult::Success(_) => {}
                _ => bail!("音频事件循环已关闭"),
            }
        }
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

/// Selecting a concrete device forbids automatic migration to an unrelated one.
pub struct NativeStream {
    stream: Stream,
    connection: Connection,
    requested: Option<String>,
}
impl NativeStream {
    pub fn open(input: bool, device: Option<&str>) -> Result<Self> {
        let mut connection = Connection::new()?;
        let spec = Spec {
            format: Format::FLOAT32NE,
            channels: 2,
            rate: 48000,
        };
        let mut properties = pulse::proplist::Proplist::new().context("无法创建音频属性")?;
        properties
            .set_str("application.name", "UUX")
            .ok()
            .context("无法设置应用名称")?;
        if device.is_some() {
            properties
                .set_str("node.dont-fallback", "true")
                .ok()
                .context("无法设置设备绑定属性")?;
            properties
                .set_str("node.dont-reconnect", "true")
                .ok()
                .context("无法设置设备恢复属性")?;
        }
        let mut stream = Stream::new_with_proplist(
            &mut connection.context,
            if input {
                "UUX 麦克风"
            } else {
                "UUX 桌面声音"
            },
            &spec,
            None,
            &mut properties,
        )
        .context("无法创建音频流")?;
        let attr = pulse::def::BufferAttr {
            maxlength: 48000 * 8 * 120 / 1000,
            tlength: 48000 * 8 * 40 / 1000,
            prebuf: 0,
            minreq: 48000 * 8 * 10 / 1000,
            fragsize: 48000 * 8 * 10 / 1000,
        };
        let flags = FlagSet::ADJUST_LATENCY
            | if device.is_some() {
                FlagSet::DONT_MOVE
            } else {
                FlagSet::empty()
            };
        if input {
            stream.connect_record(device, Some(&attr), flags)?;
        } else {
            stream.connect_playback(device, Some(&attr), flags, None, None)?;
        }
        let mut value = Self {
            stream,
            connection,
            requested: device.map(str::to_owned),
        };
        let start = Instant::now();
        loop {
            value.connection.pump()?;
            match value.stream.get_state() {
                State::Ready => {
                    value.pump()?;
                    return Ok(value);
                }
                State::Failed | State::Terminated => bail!("所选音频设备不可用"),
                _ => {}
            }
            ensure!(start.elapsed() < Duration::from_secs(1), "打开音频设备超时");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pub fn id(&self) -> String {
        format!(
            "pulse:{}",
            self.stream
                .get_device_name()
                .as_deref()
                .unwrap_or("default")
        )
    }
    fn pump(&mut self) -> Result<()> {
        self.connection.pump()?;
        ensure!(
            self.connection.context.get_state() == ContextState::Ready,
            "音频服务已断开"
        );
        ensure!(self.stream.get_state() == State::Ready, "音频设备已断开");
        ensure!(
            self.requested.as_ref().is_none_or(|requested| self
                .stream
                .get_device_name()
                .as_deref()
                == Some(requested.as_str())),
            "所选设备已断开；已拒绝音频服务自动改用其他设备"
        );
        Ok(())
    }
    pub fn capture(&mut self, queue: &mut VecDeque<f32>) -> Result<()> {
        self.pump()?;
        for _ in 0..8 {
            match self.stream.peek()? {
                PeekResult::Empty => break,
                PeekResult::Hole(bytes) => {
                    queue.extend(std::iter::repeat_n(0., (bytes / 8).min(4800)))
                }
                PeekResult::Data(data) => {
                    // Copy bounded stereo frames before releasing the server buffer.
                    let skip = data.len().saturating_sub(4800 * 8) / 8 * 8;
                    for frame in data[skip..].as_chunks::<8>().0 {
                        let a = f32::from_ne_bytes(frame[..4].try_into().unwrap());
                        let b = f32::from_ne_bytes(frame[4..].try_into().unwrap());
                        let value = (a + b) * 0.5;
                        queue.push_back(if value.is_finite() {
                            value.clamp(-1., 1.)
                        } else {
                            0.
                        });
                    }
                }
            }
            self.stream.discard()?;
        }
        let excess = queue.len().saturating_sub(4800);
        queue.drain(..excess);
        Ok(())
    }
    pub fn playback(&mut self, queue: &mut VecDeque<f32>) -> Result<()> {
        self.pump()?;
        let count = (self.stream.writable_size().unwrap_or(0) / 8 * 2)
            .min(queue.len() / 2 * 2)
            .min(9600);
        if count > 0 {
            let data: Vec<_> = queue.drain(..count).flat_map(f32::to_ne_bytes).collect();
            self.stream
                .write_copy(&data, 0, pulse::stream::SeekMode::Relative)?;
        }
        Ok(())
    }
}
impl Drop for NativeStream {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}

pub fn devices() -> Result<Value> {
    let mut connection = Connection::new()?;
    let rows = Rc::new(RefCell::new(
        json!({"input":[],"output":[],"default_input":null,"default_output":null}),
    ));
    let mut ops = Vec::new();
    let r = rows.clone();
    ops.push(
        connection
            .context
            .introspect()
            .get_server_info(move |info| {
                let mut rows = r.borrow_mut();
                rows["default_input"] = json!(info.default_source_name.as_deref());
                rows["default_output"] = json!(info.default_sink_name.as_deref());
            }),
    );
    // Operation callbacks have different types, so poll their completion separately.
    let r = rows.clone();
    let inputs=connection.context.introspect().get_source_info_list(move |result| {
        if let pulse::callbacks::ListResult::Item(info)=result && let Some(name)=info.name.as_deref() {
            let mut rows=r.borrow_mut();let list=rows["input"].as_array_mut().unwrap();
            if list.len()<128 {list.push(json!({"id":format!("pulse:{name}"),"name":info.description.as_deref().unwrap_or(name),"native_name":name,"monitor":info.monitor_of_sink.is_some()}));}
        }
    });
    let r = rows.clone();
    let outputs=connection.context.introspect().get_sink_info_list(move |result| {
        if let pulse::callbacks::ListResult::Item(info)=result && let Some(name)=info.name.as_deref() {
            let mut rows=r.borrow_mut();let list=rows["output"].as_array_mut().unwrap();
            if list.len()<128 {list.push(json!({"id":format!("pulse:{name}"),"name":info.description.as_deref().unwrap_or(name),"native_name":name}));}
        }
    });
    let start = Instant::now();
    while ops[0].get_state() == pulse::operation::State::Running
        || inputs.get_state() == pulse::operation::State::Running
        || outputs.get_state() == pulse::operation::State::Running
    {
        connection.pump()?;
        ensure!(start.elapsed() < Duration::from_secs(1), "查询音频设备超时");
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut rows = rows.borrow().clone();
    for direction in ["input", "output"] {
        let default = rows[format!("default_{direction}")].clone();
        for device in rows[direction].as_array_mut().unwrap() {
            device["default"] = json!(device["native_name"] == default);
            device.as_object_mut().unwrap().remove("native_name");
        }
    }
    Ok(rows)
}
