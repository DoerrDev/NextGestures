use std::borrow::Cow;
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use gesture_core::{Action, AiSettings};
use serde_json::{Value, json};
use tauri::AppHandle;
#[cfg(windows)]
use tauri::Manager;

#[cfg(windows)]
mod native;
#[cfg(windows)]
mod selection;

const RESPONSE_LIMIT: u64 = 2 * 1024 * 1024;
static IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Polish,
    Translate,
    Jyutping,
    CustomBubble(usize),
    CustomReplace(usize),
}

impl Mode {
    fn for_action(action: &Action, ai: &AiSettings) -> Result<Self> {
        match action {
            Action::AiPolish => Ok(Self::Polish),
            Action::AiTranslate => Ok(Self::Translate),
            Action::AiJyutping => Ok(Self::Jyutping),
            Action::AiCustom { id } => {
                let (index, action) = ai.actions.iter().enumerate().find(|(_, a)| a.id == *id).context("AI 动作已删除")?;
                Ok(match action.mode {
                    gesture_core::ResultMode::Bubble => Self::CustomBubble(index),
                    gesture_core::ResultMode::Replace => Self::CustomReplace(index),
                })
            }
            _ => bail!("动作类型错误"),
        }
    }

    fn prompt(self, show_polish_notes: bool) -> Cow<'static, str> {
        match self {
            Self::Polish => {
                let prompt = include_str!("quickformater/polish.txt");
                if show_polish_notes {
                    format!(
                        "{prompt}\n{}",
                        include_str!("quickformater/polish_notes.txt")
                    )
                    .into()
                } else {
                    format!("{prompt}\nOutput only the corrected text. Do not include notes.")
                        .into()
                }
            }
            Self::CustomBubble(_) | Self::CustomReplace(_) => unreachable!("custom actions use a user template"),
            Self::Translate => include_str!("quickformater/translate.txt").into(),
            Self::Jyutping => include_str!("quickformater/yueping.txt").into(),
        }
    }
}

fn endpoint(base_url: &str) -> Result<reqwest::Url> {
    let base = base_url.trim().trim_end_matches('/');
    let mut url = reqwest::Url::parse(base).context("AI 接口地址格式错误")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "AI 接口地址须使用 HTTP 或 HTTPS"
    );
    ensure!(url.host().is_some(), "AI 接口地址缺少主机名");
    ensure!(
        url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "AI 接口请填写基本地址"
    );
    if !url.path().ends_with("/chat/completions") {
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("AI 接口地址格式错误"))?
            .push("chat")
            .push("completions");
    }
    Ok(url)
}

fn request(
    config: &AiSettings,
    endpoint: reqwest::Url,
    mode: Mode,
    source: &str,
    on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    ensure!(source.len() <= 256 * 1024, "选中的文字过长，请缩短选区");
    let mut client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none());
    if matches!(
        endpoint.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]")
    ) {
        client = client.no_proxy();
    }
    let client = client.build()?;
    let messages = match mode {
        Mode::CustomBubble(i) | Mode::CustomReplace(i) => {
            let template = &config.actions[i].prompt;
            let count = template.matches("{{selection}}").count();
            ensure!(count > 0 && template.len().saturating_add(source.len().saturating_mul(count)) <= 2 * 1024 * 1024, "请检查 Prompt 占位符或缩短选文");
            json!([{"role":"user", "content":template.replace("{{selection}}", source)}])
        }
        _ => json!([
            {"role": "system", "content": mode.prompt(config.show_polish_notes).trim()},
            {"role": "user", "content": format!("<text>\n{source}\n</text>")},
        ]),
    };
    let body = json!({
        "model": config.model.trim(),
        "stream": true,
        "messages": messages,
    });
    let key = config.api_key.trim();
    let mut req = client
        .post(endpoint)
        .header(
            reqwest::header::ACCEPT,
            "text/event-stream, application/json",
        )
        .json(&body);
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let response = req.send().map_err(|error| {
        if error.is_timeout() {
            anyhow::anyhow!("AI 请求超时")
        } else {
            anyhow::anyhow!("连接 AI 接口失败，请检查地址及网络")
        }
    })?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        match status {
            401 | 403 => bail!("AI 接口鉴权失败（HTTP {status}），请检查 API Key"),
            404 => bail!("AI 接口地址或模型不存在（HTTP 404）"),
            429 => bail!("AI 接口请求受限（HTTP 429），请稍后重试"),
            _ => bail!("AI 接口请求失败（HTTP {status}）"),
        }
    }
    parse_response(BufReader::new(response.take(RESPONSE_LIMIT + 1)), on_delta)
}

fn parse_response(
    reader: impl BufRead,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let (mut raw, mut text, mut sse) = (String::new(), String::new(), false);
    let mut finished = false;
    let mut received = 0;
    for line in reader.lines() {
        let line = line.context("读取 AI 响应失败")?;
        received += line.len() as u64 + 1;
        ensure!(received <= RESPONSE_LIMIT, "AI 响应超过大小限制");
        if let Some(data) = line.strip_prefix("data:") {
            sse = true;
            let data = data.trim();
            if data == "[DONE]" {
                finished = true;
                break;
            }
            if data.is_empty() {
                continue;
            }
            let chunk: Value =
                serde_json::from_str(data).map_err(|_| anyhow::anyhow!("AI 流式响应格式错误"))?;
            ensure!(chunk.get("error").is_none(), "AI 服务返回错误");
            if let Some(reason) = chunk.pointer("/choices/0/finish_reason").and_then(Value::as_str) {
                ensure!(reason == "stop", "AI 回复未完整生成");
                finished = true;
            }
            if let Some(delta) = chunk
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                text.push_str(delta);
                on_delta(delta)?;
            } else if let Some(content) = chunk
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
            {
                text.push_str(content);
                on_delta(content)?;
            }
        } else if !sse {
            raw.push_str(&line);
            raw.push('\n');
        }
    }
    ensure!(!sse || finished, "AI 流式响应提前中断");
    if !sse {
        let response: Value =
            serde_json::from_str(&raw).map_err(|_| anyhow::anyhow!("AI 响应格式错误"))?;
        ensure!(response.get("error").is_none(), "AI 服务返回错误");
        ensure!(response.pointer("/choices/0/finish_reason").and_then(Value::as_str).is_none_or(|reason| reason == "stop"), "AI 回复未完整生成");
        text = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .context("AI 响应缺少文本内容")?
            .to_owned();
    }
    ensure!(!text.trim().is_empty(), "AI 返回了空内容");
    Ok(text)
}

fn split_polish_notes(output: &str) -> (&str, &str) {
    let mut offset = 0;
    for line in output.split_inclusive('\n') {
        if line.trim() == "<notes>" {
            let notes = &output[offset + line.len()..];
            return (
                &output[..offset],
                notes.trim().trim_end_matches("</notes>").trim(),
            );
        }
        offset += line.len();
    }
    (output, "")
}

fn clean_output(original: &str, output: &str) -> String {
    let mut body = output.trim();
    if let Some(text) = body
        .strip_prefix("<text>")
        .and_then(|text| text.strip_suffix("</text>"))
    {
        body = text.trim();
    }
    let original_trimmed = original.trim();
    if !original_trimmed.starts_with("```")
        && let Some(inner) = body
            .strip_prefix("```")
            .and_then(|text| text.strip_suffix("```"))
    {
        body = match inner.split_once('\n') {
            Some((language, text)) if !language.contains(' ') => text.trim(),
            _ => inner.trim(),
        };
    }
    for (open, close) in [("\"", "\""), ("“", "”"), ("'", "'")] {
        if !(original_trimmed.starts_with(open) && original_trimmed.ends_with(close))
            && let Some(inner) = body
                .strip_prefix(open)
                .and_then(|text| text.strip_suffix(close))
        {
            body = inner.trim();
        }
    }
    if body.is_empty() {
        return String::new();
    }
    let leading = &original[..original.len() - original.trim_start().len()];
    let trailing = &original[original.trim_end().len()..];
    format!("{leading}{body}{trailing}")
}

struct InProgress;
impl Drop for InProgress {
    fn drop(&mut self) {
        IN_PROGRESS.store(false, Ordering::Release);
    }
}

pub fn spawn(app: &AppHandle, action: &Action, ai: &AiSettings) -> Result<()> {
    let mode = Mode::for_action(action, ai)?;
    #[cfg(windows)]
    {
        let target = selection::Target::current().context("没有可获取选中文本的前台窗口")?;
        let owner = app
            .get_webview_window("main")
            .context("找不到剪贴板所属窗口")?
            .hwnd()?
            .0 as usize;
        let cursor = gesture_platform::input::cursor_pos();
        ensure!(
            !IN_PROGRESS.swap(true, Ordering::Acquire),
            "已有 AI 动作正在处理，请稍候"
        );
        let progress = InProgress;
        let config = ai.clone();
        std::thread::Builder::new()
            .name("quick-formater".into())
            .spawn(move || {
                let _progress = progress;
                native::run(mode, config, owner, target, cursor);
            })?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (app, mode, ai);
        bail!("此平台暂未支持选中文字动作");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_prompt_respects_notes_setting() {
        assert!(Mode::Polish.prompt(true).contains("Simplified Chinese"));
        assert!(Mode::Polish.prompt(false).contains("Do not include notes"));
        assert!(!Mode::Polish.prompt(false).contains("<notes>"));
    }

    #[test]
    fn accepts_stream_and_json() {
        let stream = b"data: {\"choices\":[{\"delta\":{\"content\":\"hello \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\n\ndata: [DONE]\n";
        assert_eq!(
            parse_response(&stream[..], |_| Ok(())).unwrap(),
            "hello world"
        );
        let json = br#"{"choices":[{"message":{"content":"test"}}]}"#;
        assert_eq!(parse_response(&json[..], |_| Ok(())).unwrap(), "test");
        assert!(
            parse_response(
                &b"data: {\"error\":{\"message\":\"bad key\"}}\n"[..],
                |_| Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn keeps_selected_whitespace_and_normalizes_endpoint() {
        assert_eq!(
            clean_output("  hello\r\n", "<text>\nHello\n</text>"),
            "  Hello\r\n"
        );
        assert_eq!(
            endpoint("https://api.example.test/v1/").unwrap().path(),
            "/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:8000/v1/chat/completions")
                .unwrap()
                .path(),
            "/v1/chat/completions"
        );
        assert_eq!(
            endpoint("http://192.168.1.3:8080/v1").unwrap().as_str(),
            "http://192.168.1.3:8080/v1/chat/completions"
        );
        assert!(endpoint("https://api.example.test/v1?token=secret").is_err());
        assert!(endpoint("https://user:secret@api.example.test/v1").is_err());
    }

    #[test]
    fn posts_selected_text_with_the_correct_prompt_for_each_action() {
        use std::io::Write;
        use std::net::TcpListener;

        for (action, prompt_fragment, answer) in [
            (Action::AiCustom { id: "fixture".into() }, "前選中文字后", "OK"),
            (Action::AiPolish, "English writing tutor", "Hello."),
            (Action::AiTranslate, "professional translator", "你好。"),
            (
                Action::AiJyutping,
                "standard Hong Kong Cantonese pronunciation",
                "學(hok6)",
            ),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let config = AiSettings {
                base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
                actions: vec![gesture_core::AiAction { id: "fixture".into(), name: "测试".into(), prompt: "前{{selection}}后".into(), mode: gesture_core::ResultMode::Bubble, icon: String::new() }],
                model: "fixture-model".into(),
                api_key: "fixture-key".into(),
                ..Default::default()
            };
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = BufReader::new(&mut stream);
                let mut first_line = String::new();
                request.read_line(&mut first_line).unwrap();
                assert_eq!(first_line.trim_end(), "POST /v1/chat/completions HTTP/1.1");
                let mut content_length = None;
                let mut authenticated = false;
                loop {
                    let mut line = String::new();
                    assert_ne!(request.read_line(&mut line).unwrap(), 0);
                    if line.trim_end().is_empty() {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap();
                    if name.eq_ignore_ascii_case("content-length") {
                        content_length = Some(value.trim().parse::<usize>().unwrap());
                    }
                    if name.eq_ignore_ascii_case("authorization") {
                        authenticated = value.trim() == "Bearer fixture-key";
                    }
                }
                assert!(authenticated);
                let mut body = vec![0; content_length.unwrap()];
                request.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["model"], "fixture-model");
                assert_eq!(body["stream"], true);
                assert!(
                    body["messages"][0]["content"]
                        .as_str()
                        .unwrap()
                        .contains(prompt_fragment)
                );
                if answer == "OK" {
                    assert_eq!(body["messages"], json!([{"role":"user","content":"前選中文字后"}]));
                } else {
                    assert_eq!(body["messages"][1]["content"], "<text>\n選中文字\n</text>");
                }
                let body = json!({"choices": [{"message": {"content": answer}}]}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let mode = Mode::for_action(&action, &config).unwrap();
            let result = request(
                &config,
                endpoint(&config.base_url).unwrap(),
                mode,
                "選中文字",
                |_| Ok(()),
            )
            .unwrap();
            worker.join().unwrap();
            assert_eq!(result, answer);
        }
    }
}
