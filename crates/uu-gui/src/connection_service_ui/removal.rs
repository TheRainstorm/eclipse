//! Check client references before owner removal, then recheck under its lock.
//! A failed second save retains the owner receipt for explicit local cleanup.
use uu_core::{
    device_book::{Destination, Store},
    service_admin::connections::Snapshot,
    Config,
};

pub(crate) struct Failure {
    pub(crate) owner_snapshot: Option<Snapshot>,
    pub(crate) message: String,
}
pub(crate) fn confirm_owner_absent(snapshot: &Snapshot, address: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !snapshot
            .entries
            .iter()
            .any(|entry| super::address_key(&entry.address) == super::address_key(address)),
        "{}",
        uu_i18n::text("服务器已重新添加，已保留客户端配置；请刷新服务器列表")
    );
    Ok(())
}
pub(crate) fn remove(
    config: &Config,
    address: &str,
    owner: impl FnOnce() -> anyhow::Result<Option<Snapshot>>,
) -> Result<Option<Snapshot>, Failure> {
    let mut snapshot = None;
    let result = (|| -> anyhow::Result<()> {
        let store = Store::open(config.at("device-trust"))?;
        let before = store.read()?;
        let originals: Vec<_> = before
            .services
            .iter()
            .filter(|service| super::address_key(&service.address) == super::address_key(address))
            .cloned()
            .collect();
        let referenced = |book: &uu_core::device_book::Book| {
            book.devices.iter().filter(|profile| profile.redirect.is_none()).any(|profile| profile.routes.iter().any(|route| matches!(&route.destination, Destination::Vps { service, .. } if originals.iter().any(|original| &original.id == service))))
        };
        anyhow::ensure!(
            !referenced(&before),
            "{}",
            uu_i18n::text("该服务器仍用于设备连接，请先移除对应的连接地址")
        );
        // Do not hold a local configuration lock while the IPC is in flight.
        snapshot = owner()?;
        store.edit(|book| {
            anyhow::ensure!(
                !referenced(book),
                "{}",
                uu_i18n::text("该服务器仍用于设备连接，请先移除对应的连接地址")
            );
            anyhow::ensure!(
                !book.services.iter().any(|service| originals
                    .iter()
                    .any(|original| original.id == service.id && original != service)),
                "{}",
                uu_i18n::text("客户端服务器配置已被其他窗口修改，已保留该配置，请刷新后重试")
            );
            book.services
                .retain(|service| !originals.iter().any(|original| original.id == service.id));
            Ok(())
        })
    })();
    match result {
        Ok(()) => Ok(snapshot),
        Err(error) => Err(Failure {
            owner_snapshot: snapshot,
            message: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_checks_the_address_of_a_reimported_owner_instead_of_only_the_old_id() {
        let address = uu_core::internet::enrollment::group_url(
            "wss://203.0.113.10:9443/v1/signal",
            &uuid::Uuid::new_v4(),
        )
        .unwrap();
        let mut snapshot = Snapshot {
            revision: "fresh".into(),
            entries: vec![uu_core::service_admin::connections::Entry {
                id: "new-owner-entry".into(),
                name: "Reimported group".into(),
                address: address.replace("group=", "%67roup="),
                ..Default::default()
            }],
            restart_required: true,
            imported: None,
        };
        assert!(confirm_owner_absent(&snapshot, &address).is_err());
        snapshot.entries[0].address = uu_core::internet::enrollment::group_url(
            "wss://203.0.113.10:9443/v1/signal",
            &uuid::Uuid::new_v4(),
        )
        .unwrap();
        assert!(confirm_owner_absent(&snapshot, &address).is_ok());
    }
    #[test]
    fn routes_block_both_stores_and_success_keeps_other_groups_and_admitted_credentials() {
        use uu_core::{
            device_book::Profile,
            internet::enrollment::{self, Enrollment, Publication},
        };
        let root =
            std::env::temp_dir().join(format!("eclipse-server-remove-{}", uuid::Uuid::new_v4()));
        let config = Config {
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            signaling_host: String::new(),
        };
        let store = Store::open(config.at("device-trust")).unwrap();
        let service = |name: &str| {
            let invitation = Enrollment::new(
                name.into(),
                enrollment::group_url("wss://203.0.113.10:9443/v1/signal", &uuid::Uuid::new_v4())
                    .unwrap(),
                zeroize::Zeroizing::new("ab".repeat(32)),
                None,
            )
            .unwrap();
            enrollment::import(
                &config.at("device-trust"),
                &invitation,
                Publication::PreserveOrDefault("own-host"),
                true,
            )
            .unwrap()
        };
        let first = service("First group");
        let other = service("Other group");
        let profile = Profile::new(
            "Saved device",
            Destination::Vps {
                service: first.id.clone(),
                device: "remote".into(),
            },
        );
        store
            .edit(|book| {
                book.devices.push(profile.clone());
                Ok(())
            })
            .unwrap();
        let mut called = false;
        let failure = remove(&config, &first.address, || {
            called = true;
            Ok(None)
        })
        .err()
        .unwrap();
        assert!(!called && failure.owner_snapshot.is_none());
        assert!(failure.message.contains("仍用于设备连接"));
        store
            .edit(|book| {
                book.devices.clear();
                Ok(())
            })
            .unwrap();
        let snapshot = Snapshot {
            revision: "owner-confirmation".into(),
            entries: vec![],
            restart_required: true,
            imported: None,
        };
        let receipt = remove(&config, &first.address, || Ok(Some(snapshot)))
            .unwrap_or_else(|failure| panic!("{}", failure.message))
            .unwrap();
        assert_eq!(receipt.revision, "owner-confirmation");
        assert_eq!(store.read().unwrap().services, vec![other]);
        assert!(
            first.token_file.is_file(),
            "existing Launch snapshots retain their credential version"
        );
        let raced = service("Concurrent group");
        let raced_profile = Profile::new(
            "New reference",
            Destination::Vps {
                service: raced.id.clone(),
                device: "remote".into(),
            },
        );
        let failure = remove(&config, &raced.address, || {
            store.edit(|book| {
                book.devices.push(raced_profile);
                Ok(())
            })?;
            Ok(Some(Snapshot {
                revision: "confirmed-before-new-reference".into(),
                entries: vec![],
                restart_required: true,
                imported: None,
            }))
        })
        .err()
        .unwrap();
        assert_eq!(
            failure.owner_snapshot.unwrap().revision,
            "confirmed-before-new-reference"
        );
        assert!(store.read().unwrap().service(&raced.id).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
