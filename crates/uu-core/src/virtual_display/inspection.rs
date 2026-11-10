//! Management read model. A snapshot holds the manager's ownership lock and
//! queries the backend once; reading never prepares or recovers a device.
use super::*;
use policy::SavedDisplay;

/// One observation of live ownership, saved definitions and backend targets.
/// External Windows changes may still happen after the backend query returns.
#[derive(Clone, Debug)]
pub struct Inspection {
    pub status: Status,
    pub displays: Vec<Display>,
    pub slots: Vec<Slot>,
}

impl Manager {
    /// Use this for a management response instead of combining separate reads.
    pub fn inspect(&self) -> Result<Inspection> {
        let mut entries = self.lock()?;
        let inventory = self.backend.inventory()?;
        let records = self.saved_records()?;
        refresh_entries(&mut entries, &inventory);
        let slots = self.slots_from_inventory(&entries, &inventory)?;
        let available = inventory
            .targets
            .iter()
            .zip(&slots)
            .filter(|(target, slot)| target.available && !slot.active && !slot.leased)
            .count();
        Ok(Inspection {
            status: self.status_from_inventory(&entries, &records, &inventory, available)?,
            displays: display_list(&entries, &records),
            slots,
        })
    }

    pub fn status(&self) -> Result<Status> {
        let entries = self.lock()?;
        let inventory = self.backend.inventory()?;
        let mut available = 0;
        for target in &inventory.targets {
            if target.available
                && !target.active
                && !entries.values().any(|entry| entry.target == target.key)
                && !self.backend.is_claimed(&target.key)?
            {
                available += 1;
            }
        }
        self.status_from_inventory(&entries, &self.saved_records()?, &inventory, available)
    }

    pub fn list(&self) -> Result<Vec<Display>> {
        let mut entries = self.lock()?;
        let inventory = self.backend.inventory()?;
        refresh_entries(&mut entries, &inventory);
        Ok(display_list(&entries, &self.saved_records()?))
    }

    pub fn slots(&self) -> Result<Vec<Slot>> {
        let entries = self.lock()?;
        self.slots_from_inventory(&entries, &self.backend.inventory()?)
    }

    fn slots_from_inventory(
        &self,
        entries: &BTreeMap<LeaseId, Entry>,
        inventory: &Inventory,
    ) -> Result<Vec<Slot>> {
        inventory
            .targets
            .iter()
            .map(|target| {
                let leased = entries.values().any(|entry| entry.target == target.key)
                    || self.backend.is_claimed(&target.key)?;
                Ok(Slot {
                    id: target.key.slot_id(),
                    name: target.name.clone(),
                    source: target.source.clone(),
                    active: target.active,
                    leased,
                    actual: target.actual,
                })
            })
            .collect()
    }

    pub(super) fn allocated_count_from_records(
        entries: &BTreeMap<LeaseId, Entry>,
        records: &[SavedDisplay],
    ) -> usize {
        entries.len()
            + records
                .iter()
                .filter(|saved| saved.enabled && !entries.contains_key(&LeaseId(saved.id)))
                .count()
    }

    fn status_from_inventory(
        &self,
        entries: &BTreeMap<LeaseId, Entry>,
        records: &[SavedDisplay],
        inventory: &Inventory,
        available_slots: usize,
    ) -> Result<Status> {
        let explanation = if self.backend.on_demand() {
            uu_i18n::text("加号只添加配置；启用时连接到 Windows，禁用时移除设备并保留配置。临时屏在最后一个串流断开后自动禁用，最多保存 16 条配置。")
        } else if !inventory.supported {
            "Virtual displays are only supported on Windows."
        } else if !inventory.driver_present {
            "Creating a virtual display can prepare the pinned signed driver through an authorized management service. A foreground application without that service requires local administrator approval."
        } else if inventory.targets.is_empty() {
            "The driver has no unused connected target. Preparation reserves an inactive pool while the host is idle; existing streams and displays are preserved."
        } else if available_slots == 0 {
            "No unused virtual display slot is available. Additional capacity can be prepared when the host is idle. Active streams are never interrupted to reload a driver, and the initial reservation is not a session limit."
        } else {
            "Unused virtual display slots are available. Removing a lease disconnects only that target from the Windows desktop."
        };
        Ok(Status {
            supported: inventory.supported,
            driver_present: inventory.driver_present,
            available_slots,
            leased: entries.len(),
            allocated: Self::allocated_count_from_records(entries, records),
            keep_after_stream: self.keep_after_stream()?,
            limit: self.limit.load(Ordering::Acquire),
            supports_lifetimes: true,
            supports_configuration: true,
            supports_catalog: true,
            on_demand: self.backend.on_demand(),
            explanation: explanation.into(),
            install_url: INSTALL_URL.into(),
            preparation: if self.backend.preparation_observation()?.is_some() {
                Some(
                    self.preparation
                        .inspect(&CapacityProvider {
                            manager: self,
                            leases: entries.len() as u32,
                            initial: None,
                            package: None,
                        })
                        .map_err(|failure| Error::Platform(failure.message))?,
                )
            } else {
                None
            },
        })
    }
}

fn refresh_entries(entries: &mut BTreeMap<LeaseId, Entry>, inventory: &Inventory) {
    for entry in entries.values_mut() {
        let target = inventory
            .targets
            .iter()
            .find(|target| target.key == entry.target);
        entry.display.active = target.is_some_and(|target| target.active);
        entry.display.actual = target.and_then(|target| target.actual);
        if let Some(target) = target {
            entry.display.source.clone_from(&target.source);
        }
    }
}

fn display_list(entries: &BTreeMap<LeaseId, Entry>, records: &[SavedDisplay]) -> Vec<Display> {
    let mut displays: Vec<_> = records
        .iter()
        .map(|saved| {
            entries
                .get(&LeaseId(saved.id))
                .map(|entry| entry.display.clone())
                .unwrap_or_else(|| saved.disconnected())
        })
        .collect();
    displays.extend(
        entries
            .values()
            .filter(|entry| !records.iter().any(|saved| saved.id == entry.display.id.0))
            .map(|entry| entry.display.clone()),
    );
    displays
}
