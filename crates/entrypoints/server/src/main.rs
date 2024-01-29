use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    str::FromStr,
    time::Duration,
};

use clap::Parser;
use docker_api::{
    opts::{ContainerFilter, ContainerListOpts},
    Docker,
};
use domain::{
    base::{
        iana::{Class, Rcode},
        Message, MessageBuilder, ToDname, Ttl,
    },
    rdata::A,
};
use expire_collections::ExpiringMap;
use lazy_static::lazy_static;
use rand::{seq::SliceRandom, thread_rng};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::{
    net::UdpSocket,
    select,
    sync::mpsc,
    time::{interval, timeout},
    try_join,
};

lazy_static! {
    static ref DOCKER: Docker = Docker::unix("/tmp/docker.sock");
}

pub enum Ret {
    Resp(Message<Vec<u8>>),
    SendToMesh(Message<Vec<u8>>, SocketAddr),
    NoOp,
}

async fn process_query(msg: Message<Vec<u8>>, tip: Args, addr: SocketAddr) -> Ret {
    let net = default_net::get_default_interface().unwrap();
    let ip = net.ipv4.first().unwrap().addr;
    let mut ans = MessageBuilder::new_vec()
        .start_answer(&msg, Rcode::NXDomain)
        .unwrap();
    ans.header_mut().set_ra(true);
    let domain = tip.domain.clone().unwrap_or("local".into());
    let host = DOCKER.info().await.unwrap().name.unwrap();
    for ques in msg.question() {
        let quest = ques.unwrap();
        let name = quest.qname();
        if name.to_string().ends_with(&domain) {
            if format!("{}.{}", host, domain).to_lowercase() == name.to_string().to_lowercase() {
                ans.push((
                    name.to_dname::<Vec<u8>>().unwrap(),
                    Class::In,
                    Ttl::SECOND,
                    A::from_str(&ip.to_string()).unwrap(),
                ))
                .unwrap();
                ans.header_mut().set_rcode(Rcode::NoError);
                return Ret::Resp(ans.into_message());
            }
            for cont in DOCKER
                .containers()
                .list(
                    &ContainerListOpts::builder()
                        .filter([ContainerFilter::Label("caddy*".into(), name.to_string())])
                        .build(),
                )
                .await
                .unwrap()
            {
                if cont.labels.clone().unwrap().iter().any(|x| {
                    x.0.contains("caddy") && !x.0.contains(".") && x.1.eq(name.to_string().as_str())
                }) {
                    ans.push((
                        name.to_dname::<Vec<u8>>().unwrap(),
                        Class::In,
                        Ttl::SECOND,
                        A::from_str(&ip.to_string()).unwrap(),
                    ))
                    .unwrap();
                    ans.header_mut().set_rcode(Rcode::NoError);
                    return Ret::Resp(ans.into_message());
                }
            }
            if !addr.ip().is_multicast() {
                return Ret::SendToMesh(msg, addr);
            }
        } else {
            let sock = UdpSocket::bind("0.0.0.0:0").await.unwrap();
            let conts = DOCKER
                .containers()
                .list(
                    &ContainerListOpts::builder()
                        .filter([ContainerFilter::LabelKey("DNS_SERVER".into())])
                        .build(),
                )
                .await
                .unwrap();
            if conts.len() > 0 {
                let cont = conts.choose(&mut thread_rng()).unwrap();
                let nets = cont.network_settings.clone().unwrap().networks.unwrap();
                let ip = nets
                    .values()
                    .find(|x| x.ip_address.is_some())
                    .unwrap()
                    .ip_address
                    .clone()
                    .unwrap();
                sock.send_to(msg.as_slice(), format!("{ip}:{}", 53))
                    .await
                    .unwrap();
                let mut tmp = [0; 512];
                match timeout(Duration::from_millis(500), sock.recv_from(&mut tmp)).await {
                    Err(_) => continue,
                    Ok(t) => match t {
                        Err(_) => continue,
                        Ok((siz, _)) => {
                            return Ret::Resp(Message::from_octets(tmp[..siz].to_vec()).unwrap())
                        }
                    },
                }
            }
        }
    }
    if !addr.ip().is_multicast() {
        return Ret::SendToMesh(msg, addr);
    } else {
        return Ret::NoOp;
    }
}

async fn handle_query(tip: &Args) {
    let socket = UdpSocket::bind(("0.0.0.0", 53)).await.unwrap();
    let (sock_tx, mut sock_rx) = mpsc::channel::<(Message<Vec<u8>>, SocketAddr)>(1_000);
    let (proc_tx, mut proc_rx) = mpsc::channel::<(Message<Vec<u8>>, SocketAddr)>(1_000);
    let (mesh_tx, mut mesh_rx) = mpsc::channel::<(Message<Vec<u8>>, SocketAddr)>(1_000);

    //Mesh Loop
    let ptx = proc_tx.clone();
    let stx = sock_tx.clone();
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
                        println!("Recieved from mesh!");
                        let retaddr = *expire.get(&msg.header().id()).unwrap();
                        stx.send((msg, retaddr)).await.unwrap();
                    } else if !expire.contains(&msg.header().id()){
                        ptx.send((msg, addr)).await.unwrap();
                    }
                }
                Some((data, sock)) = mesh_rx.recv() => {
                    if !sock.ip().is_multicast() && !expire.contains(&data.header().id()) && data.header().qr() {
                        println!("Sending to mesh!");
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

    //Processor loop
    let id = tip.clone();
    let proc = tokio::spawn(async move {
        loop {
            let id = id.clone();
            let mtx = mesh_tx.clone();
            let stx = sock_tx.clone();
            select! {
                Some((msg, sa)) = proc_rx.recv() => {
                    tokio::spawn(async move {
                    let res = process_query(msg, id, sa).await;
                    match res {
                        Ret::Resp(msg) => {
                            if sa.ip().is_multicast() {
                                mtx.send((msg,sa)).await.unwrap();
                            } else {
                                stx.send((msg,sa)).await.unwrap();
                            }
                        }
                        Ret::SendToMesh(msg, sa) => {
                            mtx.send((msg, sa)).await.unwrap();
                        }
                        Ret::NoOp => {
                        }
                    }});
                }
            }
        }
    });
    //Socket loop
    let ptx = proc_tx.clone();
    let soc = tokio::spawn(async move {
        let mut buf = [0; 512];
        loop {
            select! {
                Ok((len, addr)) = socket.recv_from(&mut buf) => {
                    ptx.send((Message::from_octets(buf[..len].to_vec()).unwrap(), addr)).await.unwrap();
                }
                Some((data,addr)) = sock_rx.recv() => {
                    socket.send_to(data.as_slice(), addr).await.unwrap();
                }
            }
        }
    });
    let _ = try_join!(soc, proc, mesh);
}

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// local domain
    #[arg(short, long)]
    domain: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    handle_query(&args).await;
}
