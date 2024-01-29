use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    str::FromStr,
    time::Duration,
};

use domain::base::Message;
use expire_collections::ExpiringMap;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::{net::UdpSocket, select, sync::mpsc, time::interval, try_join};

async fn handle_query() {
    let socket = UdpSocket::bind(("0.0.0.0", 53)).await.unwrap();
    let (sock_tx, mut sock_rx) = mpsc::channel::<(Message<Vec<u8>>, SocketAddr)>(1_000);
    let (mesh_tx, mut mesh_rx) = mpsc::channel::<(Message<Vec<u8>>, SocketAddr)>(1_000);

    //Mesh Loop
    let mesh = tokio::spawn(async move {
        let mcastaddr = SocketAddrV4::from_str("224.0.0.250:5350").unwrap();
        let addr = SocketAddr::from(mcastaddr);
        let mesh_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        mesh_sock.set_reuse_address(true).unwrap();
        mesh_sock
            .join_multicast_v4(mcastaddr.ip(), &Ipv4Addr::UNSPECIFIED)
            .unwrap();
        mesh_sock.set_nonblocking(true).unwrap();
        mesh_sock.bind(&SockAddr::from(addr)).unwrap();
        let amesh_sock = UdpSocket::from_std(mesh_sock.into()).unwrap();
        let mut expire: ExpiringMap<u16, SocketAddr> = ExpiringMap::new(Duration::from_secs(30));
        let mut inter = interval(Duration::from_secs(10));
        loop {
            let mut buf = [0; 512];
            select! {
                Ok((len,_)) = amesh_sock.recv_from(&mut buf) => {
                    let msg = Message::from_octets(buf[..len].to_vec()).unwrap();
                    if expire.contains(&msg.header().id()) && msg.header().qr() {
                        println!("Recevied from mesh");
                        let retaddr = *expire.get(&msg.header().id()).unwrap();
                        sock_tx.send((msg, retaddr)).await.unwrap();
                    }
                }
                Some((data, sock)) = mesh_rx.recv() => {
                    if !sock.ip().is_multicast() && !expire.contains(&data.header().id()) && !data.header().qr() {
                        println!("Sending to mesh");
                        expire.insert(data.header().id(), sock);
                        amesh_sock.send_to(data.as_slice(), mcastaddr).await.unwrap();
                    }
                }
                _ = inter.tick() => {
                    expire.remove_expired_entries();
                }
            }
        }
    });
    //Socket loop
    let soc = tokio::spawn(async move {
        let mut buf = [0; 512];
        loop {
            select! {
                Ok((len, addr)) = socket.recv_from(&mut buf) => {
                    mesh_tx.send((Message::from_octets(buf[..len].to_vec()).unwrap(), addr)).await.unwrap();
                }
                Some((data, addr)) = sock_rx.recv() => {
                    socket.send_to(data.as_slice(), addr).await.unwrap();
                }
            }
        }
    });
    let _ = try_join!(soc, mesh);
}

#[tokio::main]
async fn main() {
    handle_query().await;
}
