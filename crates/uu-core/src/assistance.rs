//! Official UU assistance is an ephemeral invitation, independent of native
//! permanent device grants. Codes stay in memory and cross process stdin only.
use anyhow::{ensure, Context, Result};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Request {
    pub id: String,
    code: Zeroizing<String>,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assistance")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl Request {
    pub fn new(id: &str, code: &str) -> Result<Self> {
        let id = id.trim();
        let code = code.trim();
        ensure!(
            !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_digit()),
            "请输入对方分享的数字设备 ID"
        );
        ensure!(
            !code.is_empty() && code.len() <= 64 && code.bytes().all(|b| b.is_ascii_alphanumeric()),
            "请输入有效的设备验证码（字母或数字）"
        );
        Ok(Self {
            id: id.into(),
            code: Zeroizing::new(code.into()),
        })
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn read(id: &str, input: impl std::io::Read) -> Result<Self> {
        use std::io::Read;
        let mut text = Zeroizing::new(String::new());
        input
            .take(66)
            .read_to_string(&mut text)
            .context("无法接收远程协助验证码")?;
        Self::new(id, &text)
    }
    pub fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= 4096, "分享文本过长");
        let (mut id, mut code) = (None, None);
        for line in text.lines() {
            let Some((label, value)) = line.split_once([':', '：']) else {
                continue;
            };
            let label: String = label
                .chars()
                .filter(|c| !c.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect();
            match label.as_str() {
                "设备id" | "设备编号" | "deviceid" => id = Some(value.trim()),
                "设备验证码" | "验证码" | "connectcode" => code = Some(value.trim()),
                _ => {}
            }
        }
        Self::new(
            id.context("分享文本中没有设备 ID")?,
            code.context("分享文本中没有设备验证码")?,
        )
    }
}

pub(crate) fn endpoint_fallback(version: u8, status: u16) -> bool {
    version == 2 && matches!(status, 404 | 405)
}

pub(crate) fn room(
    status: u16,
    mut body: serde_json::Value,
    request: &Request,
    account_token: &str,
) -> Result<serde_json::Value> {
    let code = body["code"]
        .as_i64()
        .context("远程协助服务返回了无效响应")?;
    if code != 0 || status != 200 {
        let message = body["msg"]
            .as_str()
            .unwrap_or("连接请求被拒绝")
            .replace(request.code(), "[已隐藏]");
        let message = if account_token.is_empty() {
            message
        } else {
            message.replace(account_token, "[已隐藏]")
        };
        let message: String = message
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect();
        anyhow::bail!("远程协助失败：{message}（code {code}，HTTP {status}）");
    }
    let data = body
        .get_mut("data")
        .and_then(serde_json::Value::as_object_mut)
        .context("远程协助服务没有返回房间")?;
    ensure!(
        data.get("token")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "远程协助房间缺少访问凭据"
    );
    if !data
        .get("signaling_server")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
    {
        let server = data
            .get("signaling_list")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .context("远程协助房间缺少信令地址")?
            .to_owned();
        data.insert("signaling_server".into(), server.into());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn share_text_validation_and_response_never_expose_the_code() {
        let request = Request::parse(
            "使用 网易UU远程 即可对我发起远程协助\n设备ID：123456789\n设备验证码：TEST79",
        )
        .unwrap();
        assert_eq!(request.id, "123456789");
        assert_eq!(request.code(), "TEST79");
        assert!(!format!("{request:?}").contains("TEST79"));
        assert!(Request::new("id", "TEST79").is_err());
        assert!(Request::new("123", "TEST\n79").is_err());
        let error = room(
            400,
            json!({"code":2007,"msg":"TEST79 session-secret\n拒绝"}),
            &request,
            "session-secret",
        )
        .unwrap_err()
        .to_string();
        assert!(
            !error.contains("TEST79")
                && !error.contains("session-secret")
                && error.contains("2007")
        );
        assert!(
            !endpoint_fallback(2, 400) && !endpoint_fallback(2, 200) && !endpoint_fallback(1, 404)
        );
        assert!(endpoint_fallback(2, 404) && endpoint_fallback(2, 405));
        let fixture = json!({"code":0,"data":{"token":"fixture-ticket","signaling_list":["https://signal.example.invalid"]}});
        assert_eq!(
            room(200, fixture.clone(), &request, "").unwrap()["data"]["signaling_server"],
            "https://signal.example.invalid"
        );
        let mut missing = fixture;
        missing["data"]["token"] = json!("");
        assert!(room(200, missing, &request, "").is_err());
    }
}
