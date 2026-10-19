//! Live pointer protocol check. Run only against an explicitly supplied test
//! device with a mouse-event window open on that device.
use std::time::Duration;
use uu_core::controller::{spawn, ControllerEvent};
fn main() -> anyhow::Result<()> {
    let mut args=std::env::args().skip(1);
    let device=args.next().ok_or_else(||anyhow::anyhow!("usage: pointer_check DEVICE BUTTON_MASK..."))?;
    anyhow::ensure!(device=="aeawvmepembcpca5","this diagnostic is restricted to KVM-WIN11");
    let buttons:Vec<u32>=args.map(|a|a.parse()).collect::<Result<_,_>>()?;
    anyhow::ensure!(!buttons.is_empty(),"supply button masks explicitly");
    let handle=spawn(uu_core::Config::from_env()?,device,"36.159.243.209".into())?;
    let mut frames=handle.frames;
    std::thread::spawn(move||{while frames.blocking_recv().is_some(){}});
    let deadline=std::time::Instant::now()+Duration::from_secs(40);
    loop {
        anyhow::ensure!(std::time::Instant::now()<deadline,"connection timed out");
        match handle.events.recv_timeout(Duration::from_millis(500)) {
            Ok(ControllerEvent::Status(s)) if s=="已请求目标画质/码率"=>break,
            Ok(ControllerEvent::Error(e))=>anyhow::bail!("{e}"),
            _=>{}
        }
    }
    let send=|value:serde_json::Value|{let _=handle.input.send(value.to_string().into_bytes());};
    send(serde_json::json!({"action":"mouse_move_absolute","abs_x":0.5,"abs_y":0.5,"screen_id":1}));
    std::thread::sleep(Duration::from_millis(600));
    for button in buttons {
        println!("probe down button={button}");
        send(serde_json::json!({"action":"mouse_press","button":button,"screen_id":1}));
        std::thread::sleep(Duration::from_millis(600));
        send(serde_json::json!({"action":"mouse_move_absolute","abs_x":0.52,"abs_y":0.52,"screen_id":1}));
        std::thread::sleep(Duration::from_millis(200));
        send(serde_json::json!({"action":"mouse_release","button":button,"screen_id":1}));
        println!("probe up button={button}");
        std::thread::sleep(Duration::from_millis(800));
    }
    let _=handle.close.send(());
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}
