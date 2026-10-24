//! Bounded native capability probe using the exact production WLAN guard.
use std::{path::PathBuf, time::Duration};
use uu_core::wlan_streaming::Guard;
fn observe(observer: &Guard) -> serde_json::Value {
    serde_json::to_value(observer.snapshot()).unwrap()
}
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "snapshot".into());
    let path = args.next().map(PathBuf::from);
    let mut observer = Guard::acquire("probe-observer", false);
    let baseline = observe(&observer);
    let mut steps = vec![serde_json::json!({"step":"baseline","value":baseline})];
    match mode.as_str() {
        "snapshot" => {}
        "lifecycle" => {
            let mut first = Guard::acquire("probe-first", true);
            std::thread::sleep(Duration::from_millis(250));
            steps.push(serde_json::json!({"step":"first_requested","value":observe(&observer),"request":first.report()}));
            let mut second = Guard::acquire("probe-second", true);
            std::thread::sleep(Duration::from_millis(250));
            steps.push(serde_json::json!({"step":"second_requested","value":observe(&observer),"request":second.report()}));
            let first_close = first.close();
            std::thread::sleep(Duration::from_millis(250));
            steps.push(serde_json::json!({"step":"first_closed","close":first_close,"value":observe(&observer)}));
            let second_close = second.close();
            std::thread::sleep(Duration::from_millis(250));
            steps.push(serde_json::json!({"step":"both_closed","close":second_close,"value":observe(&observer)}));
        }
        "hold" => {
            let seconds = args.next().unwrap_or_else(|| "30".into()).parse::<u64>()?;
            anyhow::ensure!((1..=120).contains(&seconds), "hold must be1..120sec");
            let mut guard = Guard::acquire("probe-owned-hold", true);
            steps.push(serde_json::json!({"step":"holding","value":observe(&observer),"request":guard.report(),"pid":std::process::id()}));
            if let Some(path) = &path {
                std::fs::write(path, serde_json::to_vec_pretty(&steps)?)?;
            }
            std::thread::sleep(Duration::from_secs(seconds));
            let closed = guard.close();
            steps.push(
                serde_json::json!({"step":"hold_closed","close":closed,"value":observe(&observer)}),
            );
        }
        _ => anyhow::bail!("snapshot|lifecycle|hold [report-file] [hold-seconds]"),
    }
    let closed = observer.close();
    let report = serde_json::json!({"steps":steps,"observer_close":closed,"mode":mode});
    if let Some(path) = path {
        let temporary = path.with_extension("partial");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&report)?)?;
        std::fs::rename(temporary, path)?;
    }
    println!("{}", report);
    Ok(())
}
