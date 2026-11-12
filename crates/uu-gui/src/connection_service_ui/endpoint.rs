//! Shared endpoint presentation. Display labels never become route identities.
use super::Names;
use eframe::egui;
use uu_core::{
    device_internet::EndpointStatus,
    host_runtime::{EndpointIdentity, EndpointSnapshot},
};

pub(crate) type Owner = Option<(u32, Option<String>)>;
#[derive(Default)]
pub(crate) struct Cache {
    pub(crate) values: Option<Vec<EndpointSnapshot>>,
    pub(crate) error: String,
    owner: Owner,
    live: bool,
}
impl Cache {
    pub(crate) fn observe(&mut self, owner: Owner, live: bool) {
        if self.owner != owner || !live {
            self.values = None;
            self.error.clear();
        }
        self.owner = owner;
        self.live = live;
    }
    pub(crate) fn update(&mut self, owner: Owner, result: Result<Vec<EndpointSnapshot>, String>) {
        if !self.live || self.owner != owner {
            return;
        }
        match result {
            Ok(values) => {
                self.values = Some(values);
                self.error.clear();
            }
            Err(error) => {
                self.values = None;
                self.error = error;
            }
        }
    }
    pub(crate) fn retire(&mut self) {
        self.live = false;
        self.values = None;
        self.error.clear();
    }
}

/// Optional management extension: old hosts close unknown requests without a
/// response. Keep that uncertainty out of the frequent main status poll.
#[cfg(any(windows, test))]
pub(crate) struct Poll {
    owner: Owner,
    revision: Option<String>,
    live: bool,
    supported: Option<bool>,
    next: std::time::Instant,
}
#[cfg(any(windows, test))]
#[derive(Clone, Copy)]
pub(crate) enum PollResult {
    Supported,
    Unsupported,
    Closed,
    Transient,
}
#[cfg(any(windows, test))]
impl Poll {
    pub(crate) fn new(now: std::time::Instant) -> Self {
        Self {
            owner: None,
            revision: None,
            live: false,
            supported: None,
            next: now,
        }
    }
    pub(crate) fn observe(
        &mut self,
        owner: Owner,
        revision: Option<String>,
        live: bool,
        now: std::time::Instant,
    ) {
        if self.owner != owner || self.revision != revision || self.live != live {
            self.supported = None;
            self.next = now;
        }
        self.owner = owner;
        self.revision = revision;
        self.live = live;
    }
    pub(crate) fn due(&self, now: std::time::Instant) -> bool {
        self.live && self.supported != Some(false) && now >= self.next
    }
    pub(crate) fn completed(&mut self, result: PollResult, now: std::time::Instant) {
        let wait = match result {
            PollResult::Supported => {
                self.supported = Some(true);
                5
            }
            PollResult::Unsupported => {
                self.supported = Some(false);
                5
            }
            // An EOF on an unconfirmed extension may mean an older host, but
            // could also be a temporary rejection. Probe slowly, not forever
            // disabled; once confirmed, use normal recovery cadence.
            PollResult::Closed if self.supported.is_none() => 60,
            PollResult::Closed | PollResult::Transient => 5,
        };
        self.next = now + std::time::Duration::from_secs(wait);
    }
}

pub(crate) fn tone(status: &EndpointStatus) -> crate::theme::Tone {
    use crate::theme::Tone;
    match status {
        EndpointStatus::Ready | EndpointStatus::Streaming => Tone::Success,
        EndpointStatus::Starting | EndpointStatus::Authenticating => Tone::Warning,
        EndpointStatus::Error(_) => Tone::Danger,
        EndpointStatus::Stopped => Tone::Neutral,
    }
}
pub(crate) fn name(snapshot: &EndpointSnapshot, names: &Names) -> String {
    match &snapshot.identity {
        EndpointIdentity::Ip => uu_i18n::text("IP 直连").into(),
        EndpointIdentity::Plugin { .. } => snapshot.name.clone(),
        EndpointIdentity::Server { address, .. } => {
            super::route_name(names, address, &snapshot.name)
        }
    }
}
pub(crate) fn show(
    ui: &mut egui::Ui,
    snapshot: &EndpointSnapshot,
    names: &Names,
) -> egui::Response {
    let color = crate::theme::palette(ui.visuals())
        .status(tone(&snapshot.status))
        .0;
    let label = match &snapshot.status {
        EndpointStatus::Error(_) => uu_i18n::text("连接失败").to_owned(),
        status => status.label(),
    };
    let response = ui.colored_label(color, format!("{} · {label}", name(snapshot, names)));
    match &snapshot.status {
        EndpointStatus::Ready | EndpointStatus::Streaming => response.on_hover_text(uu_i18n::text(
            "入口已就绪；从其它网络连接仍取决于网络路由、防火墙和连接服务。",
        )),
        EndpointStatus::Error(message) => response.on_hover_text(message),
        _ => response,
    }
}
pub(crate) fn unknown(ui: &mut egui::Ui, name: &str) -> egui::Response {
    ui.colored_label(
        crate::theme::palette(ui.visuals()).muted,
        format!("{name} · {}", uu_i18n::text("状态未知")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_states_have_consistent_semantic_colors() {
        use crate::theme::Tone;
        assert_eq!(tone(&EndpointStatus::Ready), Tone::Success);
        assert_eq!(tone(&EndpointStatus::Streaming), Tone::Success);
        assert_eq!(tone(&EndpointStatus::Starting), Tone::Warning);
        assert_eq!(tone(&EndpointStatus::Authenticating), Tone::Warning);
        assert_eq!(tone(&EndpointStatus::Stopped), Tone::Neutral);
        // Error text resembling a ready label must remain an error.
        assert_eq!(
            tone(&EndpointStatus::Error("可连接 / Ready".into())),
            Tone::Danger
        );
    }
    #[test]
    fn labels_use_the_active_route_and_keep_its_identity() {
        let address = uu_core::internet::enrollment::group_url(
            "wss://signal.example:9443/v1/signal",
            &uuid::Uuid::new_v4(),
        )
        .unwrap();
        let identity = EndpointIdentity::Server {
            id: "runtime-id".into(),
            address: address.clone(),
        };
        let snapshot = EndpointSnapshot {
            identity: identity.clone(),
            name: "Runtime group".into(),
            status: EndpointStatus::Ready,
        };
        let names = Names::from([(super::super::address_key(&address), "Local alias".into())]);
        let label = name(&snapshot, &names);
        assert!(label.contains("自建服务器 · Local alias"));
        assert!(label.contains("wss://signal.example:9443") && label.contains("Runtime group"));
        assert_eq!(snapshot.identity, identity);
    }
    #[test]
    fn snapshots_retire_on_stop_owner_change_and_query_failure() {
        let old = Some((12, Some("old-profile".into())));
        let new = Some((13, Some("new-profile".into())));
        let ready = || {
            vec![EndpointSnapshot {
                identity: EndpointIdentity::Ip,
                name: "runtime".into(),
                status: EndpointStatus::Ready,
            }]
        };
        let mut cache = Cache::default();
        cache.observe(old.clone(), true);
        cache.update(old.clone(), Ok(ready()));
        assert!(cache.values.is_some());
        cache.observe(old.clone(), false);
        cache.update(old.clone(), Ok(ready()));
        assert!(cache.values.is_none());
        cache.observe(new.clone(), true);
        cache.update(old.clone(), Ok(ready()));
        assert!(cache.values.is_none());
        cache.update(new.clone(), Ok(ready()));
        assert!(cache.values.is_some());
        cache.update(new.clone(), Err("temporary query failure".into()));
        assert!(cache.values.is_none());
        cache.update(new.clone(), Ok(ready()));
        assert!(
            cache.values.is_some(),
            "a later successful query restores actual state"
        );
        cache.retire();
        cache.update(new, Ok(ready()));
        assert!(cache.values.is_none());
    }
    #[test]
    fn optional_query_poll_limits_old_hosts_and_recovers_by_owner_and_capability() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let owner = Some((12, Some("profile".into())));
        let mut poll = Poll::new(now);
        poll.observe(owner.clone(), Some("old".into()), true, now);
        assert!(poll.due(now));
        poll.completed(PollResult::Closed, now);
        assert!(!poll.due(now + Duration::from_secs(5)));
        assert!(
            poll.due(now + Duration::from_secs(60)),
            "ambiguous EOF must remain recoverable"
        );
        poll.completed(PollResult::Supported, now);
        assert!(!poll.due(now + Duration::from_millis(750)));
        assert!(poll.due(now + Duration::from_secs(5)));
        poll.completed(PollResult::Closed, now);
        assert!(poll.due(now + Duration::from_secs(5)));
        poll.completed(PollResult::Transient, now);
        assert!(poll.due(now + Duration::from_secs(5)));
        poll.completed(PollResult::Unsupported, now);
        assert!(!poll.due(now + Duration::from_secs(3600)));
        poll.observe(owner.clone(), Some("new".into()), true, now);
        assert!(poll.due(now));
        poll.completed(PollResult::Unsupported, now);
        poll.observe(
            Some((13, Some("profile".into()))),
            Some("new".into()),
            true,
            now,
        );
        assert!(poll.due(now));
        poll.observe(owner, Some("new".into()), false, now);
        assert!(!poll.due(now));
    }
}
