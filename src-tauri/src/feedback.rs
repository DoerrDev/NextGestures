//! 统计心跳 + 用户反馈：对接 SayHeyServer。
//! 所有网络失败静默（记日志），不影响主功能。

use std::thread;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

const APP_ID: &str = "nextgesture";
/// 轮询未读回复（顺带心跳）的间隔；后台按 30 秒内有心跳算在线，所以要短于 30 秒
const POLL_INTERVAL: Duration = Duration::from_secs(20);
/// 未读数变化事件，payload 为数量
pub const UNREAD_EVENT: &str = "feedback:unread";

pub struct Backend {
    client: Client,
    machine_id: String,
    version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: i64,
    pub content: String,
    pub created_at: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub is_read: bool,
    #[serde(default)]
    pub via: String,
}

#[derive(Deserialize)]
struct Msg {
    id: i64,
}

impl Backend {
    pub fn new(version: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .expect("构建 HTTP 客户端失败");
        Self { client, machine_id: machine_id_hash(), version }
    }

    fn base(app: &AppHandle) -> String {
        let state = app.state::<AppState>();
        let base = state.config.read().unwrap().settings.api_base.clone();
        base.trim_end_matches('/').to_string()
    }

    fn headers(&self, req: RequestBuilder) -> RequestBuilder {
        req.header("X-App-Id", APP_ID)
            .header("X-Machine-Id", &self.machine_id)
            .header("X-Client-Version", &self.version)
            .header("X-Client-Os", "windows")
    }

    fn get<T: for<'de> Deserialize<'de>>(&self, app: &AppHandle, path: &str) -> Result<T, String> {
        let url = format!("{}{}", Self::base(app), path);
        let resp = self.headers(self.client.get(&url)).send().map_err(|e| e.to_string())?;
        let resp = resp.error_for_status().map_err(|e| e.to_string())?;
        resp.json().map_err(|e| e.to_string())
    }

    fn post<T: for<'de> Deserialize<'de>>(
        &self,
        app: &AppHandle,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, String> {
        let url = format!("{}{}", Self::base(app), path);
        let resp =
            self.headers(self.client.post(&url)).json(body).send().map_err(|e| e.to_string())?;
        let resp = resp.error_for_status().map_err(|e| e.to_string())?;
        resp.json().map_err(|e| e.to_string())
    }

    pub fn heartbeat(&self, app: &AppHandle) -> Result<(), String> {
        let body = serde_json::json!({
            "machine_id_hash": self.machine_id,
            "app_id": APP_ID,
            "version": self.version,
            "os": "windows",
        });
        self.post::<serde_json::Value>(app, "/api/heartbeat", &body).map(|_| ())
    }

    pub fn unread_count(&self, app: &AppHandle) -> Result<u32, String> {
        #[derive(Deserialize)]
        struct Count {
            count: u32,
        }
        self.get::<Count>(app, "/api/messages/unread-count").map(|c| c.count)
    }

    pub fn chat(&self, app: &AppHandle) -> Result<Vec<ChatItem>, String> {
        self.get(app, "/api/chat")
    }

    /// 提交反馈，返回服务端的 ack 文案
    pub fn submit(&self, app: &AppHandle, name: &str, message: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Ack {
            ack: String,
        }
        let body = serde_json::json!({
            "name": name,
            "message": message,
            "machine_id_hash": self.machine_id,
            "app_id": APP_ID,
        });
        self.post::<Ack>(app, "/api/feature-request", &body).map(|a| a.ack)
    }

    /// 拉取全部未读回复并标记已读，然后把未读数 0 广播给前端
    pub fn ack_unread(&self, app: &AppHandle) -> Result<(), String> {
        let msgs: Vec<Msg> = self.get(app, "/api/messages")?;
        if !msgs.is_empty() {
            let ids: Vec<i64> = msgs.iter().map(|m| m.id).collect();
            self.post::<serde_json::Value>(
                app,
                "/api/messages/ack",
                &serde_json::json!({ "ids": ids }),
            )?;
        }
        let _ = app.emit(UNREAD_EVENT, 0u32);
        Ok(())
    }
}

/// 启动：先发一次心跳，之后定时查未读（服务端会顺带记心跳）
pub fn start(app: &AppHandle) {
    let app = app.clone();
    thread::Builder::new()
        .name("feedback-poll".into())
        .spawn(move || {
            let backend = app.state::<Backend>();
            if let Err(e) = backend.heartbeat(&app) {
                log::warn!("心跳失败: {e}");
            }
            loop {
                match backend.unread_count(&app) {
                    Ok(n) => {
                        let _ = app.emit(UNREAD_EVENT, n);
                    }
                    Err(e) => log::warn!("查询未读回复失败: {e}"),
                }
                thread::sleep(POLL_INTERVAL);
            }
        })
        .expect("启动反馈轮询线程失败");
}

/// sha256("nextgesture:" + 机器码)，机器码取注册表 MachineGuid，取不到退回主机名
fn machine_id_hash() -> String {
    let raw = machine_guid().unwrap_or_else(|| std::env::var("COMPUTERNAME").unwrap_or_default());
    let digest = Sha256::digest(format!("{APP_ID}:{raw}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn machine_guid() -> Option<String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    use windows::core::w;

    unsafe {
        let mut hkey = Default::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\Microsoft\\Cryptography"),
            None,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = [0u16; 128];
        let mut len = (buf.len() * 2) as u32;
        let ok = RegQueryValueExW(
            hkey,
            w!("MachineGuid"),
            None,
            None,
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut len),
        ) == ERROR_SUCCESS;
        let _ = RegCloseKey(hkey);
        if !ok {
            return None;
        }
        let n = (len as usize / 2).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string();
        (!s.is_empty()).then_some(s)
    }
}
