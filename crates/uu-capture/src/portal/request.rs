//! Screen selection never requests persistence. ashpd 0.11's convenience method
//! always serializes persist_mode, which is invalid for a RemoteDesktop session.
//! Keep the rest of the portal/session lifecycle in ashpd and issue only this
//! method through its existing D-Bus connection.
use super::{error, CaptureError};
use ashpd::{
    desktop::screencast::{CursorMode, Screencast, SourceType},
    zbus,
    zvariant::{OwnedObjectPath, OwnedValue, Type, Value},
};
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

// Dropping a response future does not cancel the portal's dialog. The request
// owns that interaction until Response arrives or Request.Close is sent.
struct PendingRequest(Option<(zbus::Connection, String)>);
impl Drop for PendingRequest {
    fn drop(&mut self) {
        let Some((connection, path)) = self.0.take() else { return; };
        tokio::spawn(async move {
            let close = connection.call_method(
                Some("org.freedesktop.portal.Desktop"), path.as_str(),
                Some("org.freedesktop.portal.Request"), "Close", &(),
            );
            let _ = tokio::time::timeout(Duration::from_secs(2), close).await;
        });
    }
}

pub(super) async fn select_sources<T: serde::Serialize + Type>(
    cast: &Screencast<'_>,
    session: &T,
) -> Result<(), CaptureError> {
    static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
    let token = format!(
        "eclipse_{}_{}",
        std::process::id(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    let connection = cast.connection();
    let sender = connection
        .unique_name()
        .ok_or_else(|| error("桌面门户连接没有总线身份"))?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = zbus::Proxy::new(
        connection,
        "org.freedesktop.portal.Desktop",
        path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await
    .map_err(error)?;
    // Subscribe before the method call: a fast portal may answer immediately.
    let mut responses = request.receive_signal("Response").await.map_err(error)?;
    let options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("types", Value::from(SourceType::Monitor as u32)),
        ("multiple", Value::from(false)),
        ("cursor_mode", Value::from(CursorMode::Embedded as u32)),
    ]);
    // Arm before sending: cancellation can occur while SelectSources itself is
    // still waiting for its method reply, not only while waiting for Response.
    let mut pending = PendingRequest(Some((connection.clone(), path.clone())));
    let returned: OwnedObjectPath = cast
        .call("SelectSources", &(session, options))
        .await
        .map_err(error)?;
    if returned.as_str() != path {
        return Err(error("桌面门户返回了非预期的请求身份"));
    }
    let response = responses
        .next()
        .await
        .ok_or_else(|| error("屏幕选择请求意外结束"))?;
    pending.0 = None; // Response ends the request, including denial/error codes.
    let (code, _): (u32, HashMap<String, OwnedValue>) =
        response.body().deserialize().map_err(error)?;
    match code {
        0 => Ok(()),
        1 => Err(CaptureError::Denied),
        _ => Err(error("系统未能完成屏幕选择")),
    }
}
