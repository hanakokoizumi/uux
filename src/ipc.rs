//! Private same-user local transport. The lifetime socket owns all client leases.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;
pub fn directory(state: Option<&Path>) -> Result<PathBuf> {
    let path = state
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::core::cloud::state_directory);
    std::fs::create_dir_all(&path)?;
    let path = std::fs::canonicalize(path)?;
    let dir = path.join("runtime");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    // sockaddr_un has a small platform-specific limit.
    ensure!(
        dir.as_os_str().len() + 16 < 108,
        "状态目录路径太长，无法创建本地服务套接字"
    );
    Ok(dir)
}
pub fn lock(path: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    let code = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if code == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(error.into())
    }
}
pub fn write(stream: &mut UnixStream, value: &Value) -> Result<()> {
    let data = serde_json::to_vec(value)?;
    ensure!(data.len() <= MAX_MESSAGE, "本地消息过大");
    stream.write_all(&(data.len() as u32).to_le_bytes())?;
    stream.write_all(&data)?;
    Ok(())
}
pub fn read(stream: &mut UnixStream) -> Result<Value> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let size = u32::from_le_bytes(header) as usize;
    ensure!(size <= MAX_MESSAGE, "本地消息超出限制");
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn peer_is_owner(stream: &UnixStream) -> bool {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        ) == 0
            && cred.uid == libc::geteuid()
    }
}
pub struct Client {
    path: PathBuf,
    pub id: String,
    lifetime: Mutex<UnixStream>,
    frame_stream: Mutex<Option<UnixStream>>,
}
impl Client {
    pub fn connect(state: Option<&Path>, kind: &str) -> Result<Self> {
        let dir = directory(state)?;
        let path = dir.join("core.sock");
        let state = dir.parent().unwrap();
        let start = Instant::now();
        let mut spawned = false;
        let mut stream = loop {
            match UnixStream::connect(&path) {
                Ok(s) => break s,
                Err(_) if start.elapsed() < Duration::from_secs(12) => {
                    if !spawned {
                        let log = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .mode(0o600)
                            .open(dir.join("service.log"))?;
                        std::process::Command::new(std::env::current_exe()?)
                            .args(["--service", "--state-dir"])
                            .arg(state)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(log)
                            .spawn()?;
                        spawned = true;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
                Err(e) => return Err(e).context("无法连接共享核心"),
            }
        };
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        write(&mut stream, &json!({"op":"register","kind":kind}))?;
        let response = read(&mut stream)?;
        if let Some(error) = response["_ipc_error"].as_str() {
            bail!("{error}");
        }
        let id = response["client_id"]
            .as_str()
            .context("共享核心未返回客户端编号")?
            .to_owned();
        stream.set_read_timeout(None)?;
        Ok(Self {
            path,
            id,
            lifetime: Mutex::new(stream),
            frame_stream: Mutex::new(None),
        })
    }
    fn socket(&self, mut request: Value) -> Result<UnixStream> {
        let mut stream = UnixStream::connect(&self.path)?;
        stream.set_read_timeout(Some(Duration::from_secs(65)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        request["client_id"] = json!(self.id);
        write(&mut stream, &request)?;
        Ok(stream)
    }
    pub fn detach(&self) {
        if let Ok(stream) = self.lifetime.lock() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
    pub fn call(&self, request: Value) -> Result<Value> {
        let mut stream = self.socket(request)?;
        let value = read(&mut stream)?;
        if let Some(error) = value["_ipc_error"].as_str() {
            bail!("{error}");
        }
        Ok(value)
    }
    pub fn frame(&self, last: (u64, u64)) -> Result<Option<crate::backend::VideoFrame>> {
        let mut connection = self.frame_stream.lock().unwrap();
        if connection.is_none() {
            let stream = UnixStream::connect(&self.path)?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            stream.set_write_timeout(Some(Duration::from_secs(3)))?;
            *connection = Some(stream);
        }
        let stream = connection.as_mut().unwrap();
        let value = (|| -> Result<Value> {
            write(
                stream,
                &json!({"op":"frame","client_id":self.id,"generation":last.0,"sequence":last.1}),
            )?;
            read(stream)
        })();
        let value = match value {
            Ok(v) => v,
            Err(e) => {
                *connection = None;
                return Err(e);
            }
        };
        if value["frame"] != true {
            return Ok(None);
        }
        let width = value["width"].as_u64().context("帧宽无效")? as usize;
        let height = value["height"].as_u64().context("帧高无效")? as usize;
        let bytes = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(4))
            .context("帧过大")?;
        ensure!(bytes <= 128 * 1024 * 1024, "帧过大");
        let mut rgba = vec![0; bytes];
        if let Err(e) = stream.read_exact(&mut rgba) {
            *connection = None;
            return Err(e.into());
        }
        let now = Instant::now();
        Ok(Some(crate::backend::VideoFrame {
            geometry: serde_json::from_value(value["geometry"].clone())?,
            generation: value["generation"].as_u64().unwrap_or(0),
            sequence: value["sequence"].as_u64().unwrap_or(0),
            width,
            height,
            rgba,
            assembled_at: now
                .checked_sub(Duration::from_micros(
                    value["assembled_age_us"].as_u64().unwrap_or(0),
                ))
                .unwrap_or(now),
            decoded_at: now
                .checked_sub(Duration::from_micros(
                    value["decoded_age_us"].as_u64().unwrap_or(0),
                ))
                .unwrap_or(now),
        }))
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_frames_reject_oversized_lengths() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(&u32::MAX.to_le_bytes()).unwrap();
        assert!(read(&mut b).is_err());
    }
    #[test]
    fn only_one_lock_owner() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("lock");
        let a = lock(&p).unwrap().unwrap();
        assert!(lock(&p).unwrap().is_none());
        drop(a);
        assert!(lock(&p).unwrap().is_some());
    }
}
