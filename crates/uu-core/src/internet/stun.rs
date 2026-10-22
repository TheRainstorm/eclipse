//! Bounded UDP STUN binding service. No TURN allocations or media forwarding.
use anyhow::Result;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, sync::watch};
use webrtc::stun::{
    message::{Message, BINDING_REQUEST, BINDING_SUCCESS},
    xoraddr::XorMappedAddress,
};

pub async fn serve(bind: SocketAddr, mut stop: watch::Receiver<bool>) -> Result<()> {
    let socket = UdpSocket::bind(bind).await?;
    tracing::info!(address=%socket.local_addr()?,"STUN binding listening; TURN disabled");
    let mut packet = [0u8; 512];
    let mut rates: HashMap<IpAddr, (Instant, u32)> = HashMap::new();
    let mut swept = Instant::now();
    loop {
        tokio::select! {
            _=stop.changed()=>if *stop.borrow()||stop.has_changed().is_err(){break;},
            received=socket.recv_from(&mut packet)=>{
                let (length,remote)=received?;
                if length<20||length>256||packet[4..8]!=[0x21,0x12,0xa4,0x42] {continue;}
                if swept.elapsed()>Duration::from_secs(30){rates.retain(|_,(at,_)|at.elapsed()<Duration::from_secs(2));swept=Instant::now();}
                if !rates.contains_key(&remote.ip())&&rates.len()>=4096{continue;}
                let entry=rates.entry(remote.ip()).or_insert((Instant::now(),0));
                if entry.0.elapsed()>=Duration::from_secs(1){*entry=(Instant::now(),0);}
                if entry.1>=60 {continue;}entry.1+=1;
                let mut request=Message::new();
                if request.write(&packet[..length]).is_err()||request.typ!=BINDING_REQUEST {continue;}
                let mut response=Message::new();
                response.build(&[Box::new(request.transaction_id),Box::new(BINDING_SUCCESS),
                    Box::new(XorMappedAddress{ip:remote.ip(),port:remote.port()})])?;
                let _=socket.send_to(&response.raw,remote).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::stun::{agent::TransactionId, message::Getter};
    #[tokio::test]
    async fn binding_preserves_transaction_and_reports_the_observed_udp_endpoint() {
        let reserve = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reserve.local_addr().unwrap();
        drop(reserve);
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(serve(address, rx));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut request = Message::new();
        request
            .build(&[Box::<TransactionId>::default(), Box::new(BINDING_REQUEST)])
            .unwrap();
        let mut bytes = [0u8; 512];
        let (len, _) = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                client.send_to(&request.raw, address).await.unwrap();
                if let Ok(result) =
                    tokio::time::timeout(Duration::from_millis(100), client.recv_from(&mut bytes))
                        .await
                {
                    break result.unwrap();
                }
            }
        })
        .await
        .unwrap();
        let mut response = Message::new();
        response.write(&bytes[..len]).unwrap();
        assert_eq!(response.typ, BINDING_SUCCESS);
        assert_eq!(response.transaction_id, request.transaction_id);
        let mut mapped = XorMappedAddress::default();
        mapped.get_from(&response).unwrap();
        assert_eq!(
            SocketAddr::new(mapped.ip, mapped.port),
            client.local_addr().unwrap()
        );
        stop.send_replace(true);
        task.await.unwrap().unwrap();
    }
}
