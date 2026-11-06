//! Evidence binding for an interrupted first installation after external removal.
//! This authorizes no device or desktop rollback, and never adopts live targets.
use super::{Error, Result};
use serde_json::Value;
use uuid::Uuid;

pub(super) struct InterruptedInstall {
    pub directory: String,
    pub minimum: u32,
    pub backup_sha256: String,
}
impl InterruptedInstall {
    pub fn from_record(record: &Value) -> Result<Self> {
        let fail=||Error::Platform("interrupted transaction is not a bound first installation; local recovery evidence is still required".into());
        if record["schema"]!="eclipse-capacity-transaction/v1"
            || !matches!(record["phase"].as_str(),Some("installation_submitted"|"installation_remove_submitted"|"installation_removed"|"external_uninstall_verified"))
            || !record["config"].is_null() || !record["inf"].is_null()
            || !record["count_before"].is_null()
        {return Err(fail());}
        let token=record["owner_token"].as_str().ok_or_else(fail)?;
        if Uuid::parse_str(token).is_err(){return Err(fail());}
        let instance=record["instance"].as_str().ok_or_else(fail)?;
        if instance.is_empty() || instance.len()>512 || instance.contains('\0'){return Err(fail());}
        let directory=record["installation_directory"].as_str().ok_or_else(fail)?;
        if directory.is_empty() || directory.contains('\0'){return Err(fail());}
        let minimum=u32::try_from(record["count_requested"].as_u64().ok_or_else(fail)?).map_err(|_|fail())?;
        if minimum==0{return Err(fail());}
        let backup_sha256=record["config_before_sha256"].as_str().ok_or_else(fail)?;
        if backup_sha256.len()!=64 || !backup_sha256.bytes().all(|b|b.is_ascii_hexdigit()) {return Err(fail());}
        if record["driver_version"]!=super::driver_package().driver_version{return Err(fail());}
        Ok(Self{directory:directory.into(),minimum,backup_sha256:backup_sha256.into()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record()->Value {
        serde_json::json!({"schema":"eclipse-capacity-transaction/v1","phase":"installation_submitted",
            "owner_token":Uuid::new_v4().to_string(),"instance":"generated-device-instance",
            "installation_directory":"generated-owned-directory","count_requested":16,
            "config":null,"inf":null,"count_before":null,"config_before_sha256":"a".repeat(64),
            "driver_version":super::super::driver_package().driver_version})
    }
    #[test]
    fn only_bound_first_install_records_can_enter_external_removal_recovery() {
        assert!(InterruptedInstall::from_record(&record()).is_ok());
        for (key,value) in [("phase",Value::from("pool_save_submitted")),("config",Value::from("existing.xml")),
            ("inf",Value::from("oem1.inf")),("count_before",Value::from(1)),("instance",Value::Null),
            ("owner_token",Value::from("invalid")),("count_requested",Value::from(0)),
            ("config_before_sha256",Value::from("unbound")),("driver_version",Value::from("unknown"))] {
            let mut changed=record();changed[key]=value;
            assert!(InterruptedInstall::from_record(&changed).is_err(),"{key}");
        }
    }
    #[test]
    fn removal_submission_and_completed_node_removal_keep_the_original_evidence_binding() {
        for phase in ["installation_remove_submitted", "installation_removed"] {
            let mut bound = record(); bound["phase"] = phase.into();
            assert!(InterruptedInstall::from_record(&bound).is_ok());
            bound["config_before_sha256"] = Value::Null;
            assert!(InterruptedInstall::from_record(&bound).is_err());
        }
    }
}
