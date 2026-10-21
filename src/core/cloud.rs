use anyhow::{Context, Result, bail, ensure};
use hmac::{Hmac, Mac};
use reqwest::{Client, Method};
use rsa::{RsaPublicKey, pkcs1v15::VerifyingKey, pkcs8::DecodePublicKey, signature::Verifier};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    fs::{self, Permissions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const KEY: &[u8] = b"alWiSzXZTLu3WfFnw13uBru3";
const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAxFwOA5SFhnnn3AbEa9IJ
t3dd7wuyMj3vuowmxM6xHogzjFY96OuwUmyGaBKd91nZla5/lbUZ/6rnsOQGo3Tq
BbiA4arexZULy129zwvTsA0jz6m6h/xjIxho3YkmAHKcK+AuGXPsy3r9fIqAqRx/
VGB5vRtnJ0Hz/K1BZIUJgQAm0zhwqjPPTpQSgi7kWm61sNXwIrsYwetf1sZm0KEe
vUeBYZvkHCdFvTb0NqsS45VR+XaczUScssMdwkRJSIV8MAjdvx0ldM8u15HdynNU
N7zFMuR7buqigCdJAhuVp+ZUFcXT34W05neUR8M4743qVU2Gn7HgpvTvQlyTzDYC
GwIDAQAB
-----END PUBLIC KEY-----";

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn state_directory() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directories::BaseDirs::new()
                .unwrap()
                .home_dir()
                .join(".local/state")
        })
        .join("uu-linux")
}

fn read_json(path: &std::path::Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    ensure!(
        !path.symlink_metadata()?.file_type().is_symlink(),
        "拒绝读取会话符号链接"
    );
    serde_json::from_slice(&fs::read(path)?).context("本机状态文件损坏")
}

pub fn write_private(path: &std::path::Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("无效状态路径")?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, Permissions::from_mode(0o700))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file()
        .set_permissions(Permissions::from_mode(0o600))?;
    temp.write_all(&serde_json::to_vec(value)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|_| anyhow::anyhow!("无法保存本机状态"))?;
    Ok(())
}

pub fn canonical(headers: &BTreeMap<String, String>) -> String {
    headers
        .iter()
        .filter(|(k, _)| k.starts_with("x-param-") && *k != "x-param-sign")
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn sign(method: &str, path: &str, headers: &BTreeMap<String, String>, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY).expect("HMAC key");
    mac.update(format!("{method}{path}{}", canonical(headers)).as_bytes());
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

struct State {
    directory: PathBuf,
    identity: String,
    session: Value,
}

#[derive(Clone)]
pub struct Cloud {
    client: Client,
    state: Arc<Mutex<State>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub mode: u8,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}
impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            mode: 0,
            scheme: "http".into(),
            host: String::new(),
            port: 8080,
            username: String::new(),
            password: String::new(),
        }
    }
}
impl ProxySettings {
    pub fn validate(&self) -> Result<()> {
        self.client().map(|_| ())
    }
    fn client(&self) -> Result<Client> {
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none());
        match self.mode {
            0 => {}
            1 => builder = builder.no_proxy(),
            2 => {
                ensure!(
                    ["http", "socks5h"].contains(&self.scheme.as_str()),
                    "不支持的代理类型"
                );
                ensure!(
                    !self.host.is_empty() && self.port != 0,
                    "请输入代理地址和端口"
                );
                let mut url = url::Url::parse(&format!("{}://localhost", self.scheme))
                    .context("代理类型无效")?;
                url.set_host(Some(&self.host))
                    .map_err(|_| anyhow::anyhow!("代理地址无效"))?;
                url.set_port(Some(self.port))
                    .map_err(|_| anyhow::anyhow!("代理端口无效"))?;
                url.set_username(&self.username)
                    .map_err(|_| anyhow::anyhow!("代理用户名无效"))?;
                url.set_password((!self.password.is_empty()).then_some(self.password.as_str()))
                    .map_err(|_| anyhow::anyhow!("代理密码无效"))?;
                builder = builder
                    .no_proxy()
                    .proxy(reqwest::Proxy::all(url).context("代理配置无效")?);
            }
            _ => bail!("代理模式无效"),
        }
        builder.build().context("无法创建网络连接")
    }
}

/// Authentication material is kept only for the connection attempt.
/// Deliberately has no Debug/Serialize implementation.
pub enum Target {
    Device(String),
    Assistance { id: String, code: String },
}

pub fn assistance_id(value: &str) -> Result<String> {
    let id: String = value
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    ensure!(
        id.len() == 9 && id.bytes().all(|c| c.is_ascii_digit()),
        "请输入 9 位伙伴设备 ID"
    );
    Ok(id)
}

fn assistance_body(id: &str, code: &str) -> Result<Value> {
    let id = assistance_id(id)?;
    ensure!(
        !code.is_empty() && code.len() <= 64 && !code.chars().any(char::is_control),
        "请输入有效的设备验证码"
    );
    Ok(json!({"connect_id":id,"connect_code":code}))
}

impl Cloud {
    pub fn with_proxy(&self, settings: &ProxySettings) -> Result<Self> {
        Ok(Self {
            client: settings.client()?,
            state: self.state.clone(),
        })
    }
    pub async fn test_network(&self) -> Result<()> {
        self.client
            .get("https://api.nrd.nie.163.com")
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("连接失败，请检查代理地址、认证和网络"))?;
        Ok(())
    }
    pub fn new(directory: Option<PathBuf>, proxy: Option<&str>) -> Result<Self> {
        let directory = directory.unwrap_or_else(state_directory);
        let identity_path = directory.join("identity.json");
        let identity = if identity_path.exists() {
            let value = read_json(&identity_path)?;
            Uuid::parse_str(value["system_id"].as_str().context("本机标识损坏")?)?.to_string()
        } else {
            let id = Uuid::new_v4().to_string();
            write_private(&identity_path, &json!({"system_id": id}))?;
            id
        };
        let session = read_json(&directory.join("session.json"))?;
        ensure!(session.is_object(), "会话格式无效");
        let mut builder = Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = proxy.filter(|s| !s.is_empty()) {
            builder = builder.proxy(reqwest::Proxy::all(proxy).context("代理地址无效")?);
        }
        Ok(Self {
            client: builder.build()?,
            state: Arc::new(Mutex::new(State {
                directory,
                identity,
                session,
            })),
        })
    }

    pub fn public_state(&self) -> (bool, String) {
        let s = self.state.lock().unwrap();
        (
            s.session["access_token"]
                .as_str()
                .is_some_and(|v| !v.is_empty()),
            s.session["nickname"].as_str().unwrap_or("UU 用户").into(),
        )
    }

    pub fn device_id(&self) -> String {
        self.state.lock().unwrap().session["device_id"]
            .as_str()
            .unwrap_or("")
            .into()
    }

    pub fn logout(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.session = json!({"device_id":s.session["device_id"],"client_id":s.session["client_id"]});
        write_private(&s.directory.join("session.json"), &s.session)
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        auth: bool,
    ) -> Result<Value> {
        ensure!(
            path.starts_with("/api/") && !path.contains("://"),
            "无效 API 路径"
        );
        let nonce = Uuid::new_v4().simple().to_string();
        let mut headers: BTreeMap<String, String> = [
            ("content-type", "application/json"),
            ("x-param-plat", "1"),
            ("x-param-vn", "4.33.0.8907"),
            ("x-param-vc", "8907"),
            ("x-param-pkgn", "com.netease.uuremote"),
            ("x-param-chn", "nochannel"),
            ("x-param-lang", "zh-CN"),
            ("x-param-cnt", "zh-CN"),
            ("x-param-rel", "prod"),
            ("x-param-opr", ""),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        {
            let s = self.state.lock().unwrap();
            headers.insert("x-param-system-id".into(), s.identity.clone());
            headers.insert(
                "x-param-device-id".into(),
                s.session["device_id"].as_str().unwrap_or("").into(),
            );
            headers.insert(
                "x-param-client-id".into(),
                s.session["client_id"]
                    .as_str()
                    .unwrap_or(&s.identity)
                    .into(),
            );
            if auth {
                if let Some(token) = s.session["access_token"].as_str() {
                    headers.insert("authorization".into(), format!("Bearer {token}"));
                }
                if let Some(id) = s.session.get("user_id") {
                    headers.insert(
                        "x-param-user-id".into(),
                        id.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| id.to_string()),
                    );
                }
            }
        }
        headers.insert("x-param-nonce".into(), nonce.clone());
        headers.insert("x-param-ts".into(), (now_ms() / 1000).to_string());
        let bytes = body
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()?
            .unwrap_or_default();
        headers.insert(
            "x-param-sign".into(),
            sign(method.as_str(), path, &headers, &bytes),
        );
        let device_action_endpoint = super::device_actions::unsigned_endpoint(&method, path);
        let mut request = self
            .client
            .request(method, format!("https://api.nrd.nie.163.com{path}"))
            .body(bytes);
        for (k, v) in &headers {
            request = request.header(k, v);
        }
        if path.ends_with("/qrcode/login/status") {
            request = request.timeout(Duration::from_secs(70));
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("无法连接 UU 云端服务"))?;
        let status = response.status();
        let rh: BTreeMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().into(), v.into())))
            .collect();
        ensure!(
            response.content_length().unwrap_or(0) < 8 * 1024 * 1024,
            "云端响应过大"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("云端响应读取失败"))?
        {
            ensure!(bytes.len() + chunk.len() <= 8 * 1024 * 1024, "云端响应过大");
            bytes.extend_from_slice(&chunk);
        }
        let known_unsigned = matches!(
            path,
            "/api/v1/device/windows/init"
                | "/api/v1/qrcode/gen/login"
                | "/api/v1/qrcode/login/status"
                | "/api/v1/login/by_qrcode"
                | "/api/v1/device/list"
                | "/api/v2/room/share/control_mode"
                | "/api/v2/room/join/share/by_code"
        ) || path.starts_with("/api/v1/room/join/")
            || device_action_endpoint;
        if !known_unsigned || rh.contains_key("x-param-nonce") || rh.contains_key("x-param-sign") {
            ensure!(
                rh.get("x-param-nonce") == Some(&nonce),
                "服务端响应随机数不匹配"
            );
            let signature = hex::decode(rh.get("x-param-sign").context("服务端缺少完整性签名")?)
                .context("服务端签名格式错误")?;
            let mut signed = canonical(&rh).into_bytes();
            signed.extend_from_slice(&bytes);
            VerifyingKey::<Sha1>::new(RsaPublicKey::from_public_key_pem(PUBLIC_KEY)?)
                .verify(
                    &signed,
                    &signature
                        .as_slice()
                        .try_into()
                        .context("服务端签名格式错误")?,
                )
                .map_err(|_| anyhow::anyhow!("服务端响应完整性校验失败"))?;
        }
        let value: Value = serde_json::from_slice(&bytes).context("云端响应格式错误")?;
        if !status.is_success() || value["code"].as_i64() != Some(0) {
            let code = value["code"].as_i64().unwrap_or(-1);
            if code == 1190 {
                bail!("登录二维码已过期，请刷新");
            }
            if code == 1130 {
                bail!("该设备与当前账号相同，请从「我的设备」连接。远程协助仅用于其他账号的设备。");
            }
            bail!("UU 请求失败（code={code}, HTTP={}）", status.as_u16());
        }
        Ok(value["data"].clone())
    }

    pub async fn initialize(&self) -> Result<()> {
        if !self.device_id().is_empty() {
            return Ok(());
        }
        let identity = self.state.lock().unwrap().identity.clone();
        let read = |p: &str| fs::read_to_string(p).unwrap_or_default().trim().to_string();
        let cpu = read("/proc/cpuinfo")
            .lines()
            .find_map(|l| {
                l.strip_prefix("model name")
                    .and_then(|v| v.split_once(':'))
                    .map(|(_, v)| v.trim().to_string())
            })
            .unwrap_or_default();
        let memory = read("/proc/meminfo")
            .lines()
            .find_map(|l| {
                l.strip_prefix("MemTotal:")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .unwrap_or(0)
            / 1024;
        let data = json!({"name":read("/proc/sys/kernel/hostname"),"client_id":identity,"system_id":identity,
            "machine_guid":read("/etc/machine-id"),"os":format!("Linux {}",read("/proc/sys/kernel/osrelease")),
            "base_board":read("/sys/class/dmi/id/board_name"),"cpu":cpu,"video":[],"mac":"","screen":"",
            "memory":memory,"platform":1,"controllable":false});
        let result = self
            .request(
                Method::POST,
                "/api/v1/device/windows/init",
                Some(data),
                false,
            )
            .await?;
        let device = result["device_id"].as_str().context("初始化缺少设备标识")?;
        let mut s = self.state.lock().unwrap();
        s.session["device_id"] = json!(device);
        s.session["client_id"] = json!(identity);
        write_private(&s.directory.join("session.json"), &s.session)
    }

    pub async fn qr(&self) -> Result<Value> {
        self.initialize().await?;
        self.request(Method::POST, "/api/v1/qrcode/gen/login", None, false)
            .await
    }

    pub async fn poll_qr(&self, url: &str, token: &str, status: i64) -> Result<i64> {
        let data = self
            .request(
                Method::POST,
                "/api/v1/qrcode/login/status",
                Some(json!({
                    "qrcode_jump_url":url,"token":token,"login_status":status
                })),
                false,
            )
            .await?;
        data["status"].as_i64().context("扫码状态无效")
    }

    pub async fn login(&self, url: &str, token: &str) -> Result<()> {
        let data = self
            .request(
                Method::POST,
                "/api/v1/login/by_qrcode",
                Some(json!({
                    "qrcode_jump_url":url,"token":token
                })),
                false,
            )
            .await?;
        let token = data["token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("登录缺少凭据")?
            .to_string();
        let mut s = self.state.lock().unwrap();
        let device = s.session["device_id"].clone();
        let client = s.session["client_id"].clone();
        s.session = data;
        s.session["access_token"] = json!(token);
        s.session["device_id"] = device;
        s.session["client_id"] = client;
        write_private(&s.directory.join("session.json"), &s.session)
    }

    pub async fn devices(&self) -> Result<Value> {
        ensure!(self.public_state().0, "请先扫码登录");
        self.request(Method::GET, "/api/v1/device/list", None, true)
            .await
    }

    pub async fn preview(&self, url: &str) -> Result<Vec<u8>> {
        let parsed = url::Url::parse(url)?;
        ensure!(
            parsed.scheme() == "https"
                && parsed
                    .host_str()
                    .is_some_and(|h| h.ends_with(".netease.com")),
            "不支持的设备缩略图地址"
        );
        let mut response = self.client.get(parsed).send().await?.error_for_status()?;
        ensure!(
            response.content_length().unwrap_or(0) <= 4 * 1024 * 1024,
            "设备缩略图过大"
        );
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                "设备缩略图过大"
            );
            bytes.extend_from_slice(&chunk);
        }
        let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?;
        let (width, height) = reader.into_dimensions()?;
        ensure!(
            width > 0
                && height > 0
                && width <= 8192
                && height <= 8192
                && u64::from(width) * u64::from(height) <= 16_777_216,
            "缩略图尺寸无效"
        );
        Ok(bytes)
    }

    pub async fn join(&self, id: &str) -> Result<Value> {
        ensure!(
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "设备标识无效"
        );
        self.request(
            Method::POST,
            &format!("/api/v1/room/join/by_device/{id}"),
            Some(json!({"force_join":false})),
            true,
        )
        .await
    }

    pub async fn join_target(&self, target: &Target) -> Result<Value> {
        match target {
            Target::Device(id) => self.join(id).await,
            Target::Assistance { id, code } => {
                ensure!(self.public_state().0, "请先扫码登录");
                // The official request takes the user-supplied code directly;
                // authorization and attempt limits remain server enforced.
                self.request(
                    Method::POST,
                    "/api/v2/room/join/share/by_code",
                    Some(assistance_body(id, code)?),
                    true,
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_modes_validate_without_network_and_reject_bad_addresses() {
        for mode in [0, 1] {
            assert!(
                ProxySettings {
                    mode,
                    ..Default::default()
                }
                .validate()
                .is_ok()
            );
        }
        for scheme in ["http", "socks5h"] {
            let mut proxy = ProxySettings {
                mode: 2,
                scheme: scheme.into(),
                host: "127.0.0.1".into(),
                username: "user@example".into(),
                password: "p:a/ss#word".into(),
                ..Default::default()
            };
            assert!(proxy.validate().is_ok());
            proxy.host = "bad/host".into();
            assert!(proxy.validate().is_err());
            proxy.host = "localhost".into();
            proxy.port = 0;
            assert!(proxy.validate().is_err());
        }
        assert!(
            ProxySettings {
                mode: 3,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn assistance_request_matches_official_builder() {
        assert_eq!(
            assistance_body("123 456-789", "ExampleCode8").unwrap(),
            json!({"connect_id":"123456789","connect_code":"ExampleCode8"})
        );
        for invalid in ["123", "../123456", "abcdefgh1", "1234567890"] {
            assert!(assistance_id(invalid).is_err());
        }
        assert!(assistance_body("123456789", "").is_err());
        assert!(assistance_body("123456789", "a\nb").is_err());
    }
    #[test]
    fn signatures_and_private_state_are_compatible() {
        let headers = BTreeMap::from([
            ("x-param-ts".into(), "1700000000".into()),
            ("x-param-nonce".into(), "abc".into()),
        ]);
        assert_eq!(
            canonical(&headers),
            "x-param-nonce=abc&x-param-ts=1700000000"
        );
        let directory = tempfile::tempdir().unwrap();
        let c = Cloud::new(Some(directory.path().to_owned()), None).unwrap();
        assert!(!c.public_state().0);
        assert_eq!(
            fs::metadata(directory.path().join("identity.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            sign("POST", "/api/v1/example", &headers, b"{}"),
            "c7a383ccd26c85847cbcd79bfa386fa15bcec72a079fea3bf1ea29e5bc15a1d2"
        );
    }
}
