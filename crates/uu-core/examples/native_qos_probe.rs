//! Isolated ordinary-user API capability check. Never touches installed apps.
#[cfg(windows)]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::{
        net::{Ipv4Addr, SocketAddrV4},
        sync::Arc,
    };
    use uu_core::native_qos::Guard;
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 3,
        "usage: native_qos_probe <concrete-local-ipv4> <owned-destination-ipv4:port>"
    );
    let local: Ipv4Addr = args[1].parse()?;
    let destination: SocketAddrV4 = args[2].parse()?;
    anyhow::ensure!(
        !local.is_unspecified() && !local.is_multicast(),
        "concrete interface required"
    );
    let a = Arc::new(tokio::net::UdpSocket::bind((local, 0)).await?);
    let b = Arc::new(tokio::net::UdpSocket::bind((local, 0)).await?);
    let (wa, wb) = (Arc::downgrade(&a), Arc::downgrade(&b));
    tokio::task::spawn_blocking(move||->anyhow::Result<()>{
        let mut first=Guard::acquire(a,destination)?;let mut second=Guard::acquire(b,destination)?;
        let before=second.query()?;first.close();first.close();let after=second.query()?;
        anyhow::ensure!(before==after,"independent second flow changed after closing first");
        anyhow::ensure!(before.conformant_dscp>0 || before.conformant_l2>0,"PacketPriority query returned only best-effort markings; candidate configuration unconfirmed");
        second.close();drop(first);drop(second);
        anyhow::ensure!(wa.upgrade().is_none()&&wb.upgrade().is_none(),"owned sockets retained");
        println!("{}",serde_json::json!({"event":"native_qos_capability","passed":true,"two_handles":true,"query_after_other_closed":after,"sockets_released":true}));Ok(())
    }).await??;
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Windows qWAVE capability probe only");
    std::process::exit(2);
}
