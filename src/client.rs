// NOTE: since most of the outer code is sync w/ threads, and most of the code in here is async,
// we've adopted a weird model in here where we instantiate the tokio runtime on each use, and tear
// it down afterwards. It's not the most efficient pattern I am well aware but it does work.
//
// -erikh
//
use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use http::{HeaderMap, HeaderValue};
use tokio::sync::mpsc;
use zerotier_central_api::types::Network as CentralNetwork;
use zerotier_central_api::{types::Member, Client, ResponseValue};
use zerotier_one_api::types::Network;

use crate::app::NetworkFlag;

// address of Central
const CENTRAL_BASEURL: &str = "https://my.zerotier.com/api/v1";

// this provides the production configuration for talking to central through the openapi libraries.
pub fn central_client(token: String) -> Result<zerotier_central_api::Client, anyhow::Error> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("bearer {}", token))?,
    );

    Ok(zerotier_central_api::Client::new_with_client(
        &std::env::var("ZEROTIER_CENTRAL_INSTANCE").unwrap_or(CENTRAL_BASEURL.to_string()),
        reqwest::Client::builder()
            .https_only(true)
            .default_headers(headers)
            .build()?,
    ))
}

// determine the path of the authtoken.secret
pub fn authtoken_path(arg: Option<&Path>) -> &Path {
    if let Some(arg) = arg {
        return arg;
    }

    if cfg!(target_os = "linux") {
        Path::new("/var/lib/zerotier-one/authtoken.secret")
    } else if cfg!(target_os = "windows") {
        Path::new("C:/ProgramData/ZeroTier/One/authtoken.secret")
    } else if cfg!(target_os = "macos") {
        Path::new("/Library/Application Support/ZeroTier/One/authtoken.secret")
    } else {
        panic!("authtoken.secret not found; please provide the -s option to provide a custom path")
    }
}

pub fn local_client_from_file(
    authtoken_path: &Path,
) -> Result<zerotier_one_api::Client, anyhow::Error> {
    let authtoken = std::fs::read_to_string(authtoken_path)?;
    local_client(authtoken)
}

fn local_client(authtoken: String) -> Result<zerotier_one_api::Client, anyhow::Error> {
    let mut headers = HeaderMap::new();
    headers.insert("X-ZT1-Auth", HeaderValue::from_str(&authtoken)?);

    Ok(zerotier_one_api::Client::new_with_client(
        "http://127.0.0.1:9993",
        reqwest::Client::builder()
            .default_headers(headers)
            .build()?,
    ))
}

pub async fn get_networks(s: mpsc::UnboundedSender<Vec<Network>>) -> Result<(), anyhow::Error> {
    let client = local_client_from_file(authtoken_path(None))?;
    let networks = client.get_networks().await?;

    s.send(networks.to_vec())?;
    Ok(())
}

pub fn leave_network(network_id: String) -> Result<ResponseValue<()>, zerotier_one_api::Error> {
    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let (s, mut r) = mpsc::unbounded_channel();

    t.spawn(async move {
        let client = local_client_from_file(authtoken_path(None)).unwrap();
        s.send(client.delete_network(&network_id).await).unwrap()
    });

    let res: Result<ResponseValue<()>, zerotier_one_api::Error>;

    loop {
        if let Ok(r) = r.try_recv() {
            res = r;
            break;
        }
    }

    t.shutdown_background();
    res
}

pub fn join_network(network_id: String) -> Result<ResponseValue<Network>, zerotier_one_api::Error> {
    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (s, mut r) = mpsc::unbounded_channel();

    t.spawn(async move {
        let client = local_client_from_file(authtoken_path(None)).unwrap();
        s.send(
            client
                .update_network(
                    &network_id,
                    &Network {
                        allow_default: None,
                        allow_dns: None,
                        allow_global: None,
                        allow_managed: None,
                        assigned_addresses: Vec::new(),
                        bridge: None,
                        broadcast_enabled: None,
                        dns: None,
                        id: None,
                        mac: None,
                        mtu: None,
                        multicast_subscriptions: Vec::new(),
                        name: None,
                        netconf_revision: None,
                        port_device_name: None,
                        port_error: None,
                        routes: Vec::new(),
                        status: None,
                        type_: None,
                    },
                )
                .await,
        )
    });

    let res: Result<ResponseValue<Network>, zerotier_one_api::Error>;

    loop {
        if let Ok(r) = r.try_recv() {
            res = r;
            break;
        }
    }

    t.shutdown_background();
    res
}

pub fn sync_get_networks() -> Result<Vec<Network>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    t.spawn(crate::client::get_networks(s));

    let networks: Vec<Network>;

    let timeout = Instant::now();

    'outer: loop {
        match r.try_recv() {
            Ok(n) => {
                networks = n;
                break 'outer;
            }

            Err(_) => std::thread::sleep(Duration::new(0, 10)),
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }

    t.shutdown_background();
    Ok(networks)
}

pub fn sync_get_members(client: Client, id: String) -> Result<Vec<Member>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    t.spawn(async move { s.send(client.get_network_member_list(&id).await).unwrap() });

    let members: Vec<Member>;

    let timeout = Instant::now();

    'outer: loop {
        match r.try_recv() {
            Ok(m) => match m {
                Ok(m) => {
                    members = m.to_vec();
                    break 'outer;
                }
                Err(e) => return Err(anyhow!(e)),
            },

            Err(_) => std::thread::sleep(Duration::new(0, 10)),
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }

    t.shutdown_background();
    Ok(members)
}

pub fn sync_update_member_name(
    client: Client,
    network_id: String,
    id: String,
    name: String,
) -> Result<ResponseValue<Member>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    t.spawn(async move {
        let mut member = client.get_network_member(&network_id, &id).await.unwrap();
        member.name = Some(name);
        s.send(
            client
                .update_network_member(&network_id, &id, &member)
                .await,
        )
        .unwrap();
    });

    let timeout = Instant::now();

    loop {
        if let Ok(res) = r.try_recv() {
            t.shutdown_background();
            return Ok(res?);
        } else {
            std::thread::sleep(Duration::new(0, 10));
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            t.shutdown_background();
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }
}

pub fn sync_member_auth(
    client: Client,
    network_id: String,
    id: String,
    auth: bool,
) -> Result<ResponseValue<Member>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    t.spawn(async move {
        let mut member = client.get_network_member(&network_id, &id).await.unwrap();
        member.config.as_mut().unwrap().authorized = Some(auth);
        s.send(
            client
                .update_network_member(&network_id, &id, &member)
                .await,
        )
        .unwrap();
    });

    let timeout = Instant::now();

    loop {
        if let Ok(res) = r.try_recv() {
            t.shutdown_background();
            return Ok(res?);
        } else {
            std::thread::sleep(Duration::new(0, 10));
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            t.shutdown_background();
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }
}

pub fn sync_deauthorize_member(
    client: Client,
    network_id: String,
    id: String,
) -> Result<ResponseValue<Member>, anyhow::Error> {
    sync_member_auth(client, network_id, id, false)
}

pub fn sync_authorize_member(
    client: Client,
    network_id: String,
    id: String,
) -> Result<ResponseValue<Member>, anyhow::Error> {
    sync_member_auth(client, network_id, id, true)
}

pub fn sync_delete_member(
    client: Client,
    network_id: String,
    id: String,
) -> Result<ResponseValue<()>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    t.spawn(async move {
        s.send(client.delete_network_member(&network_id, &id).await)
            .unwrap();
    });

    let timeout = Instant::now();

    loop {
        if let Ok(res) = r.try_recv() {
            t.shutdown_background();
            return Ok(res?);
        } else {
            std::thread::sleep(Duration::new(0, 10));
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            t.shutdown_background();
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }
}

macro_rules! true_or_none {
    ($code:expr) => {
        $code = $code.map_or(Some(false), |m| Some(!m));
    };
}

pub fn toggle_flag(id: String, flag: NetworkFlag) -> Result<ResponseValue<Network>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    t.spawn(async move {
        let local = local_client_from_file(authtoken_path(None)).unwrap();
        let mut network = match local.get_network(&id.clone()).await {
            Ok(network) => network,
            Err(e) => {
                s.send(Err(e)).unwrap();
                return;
            }
        };

        match flag {
            NetworkFlag::AllowDNS => {
                true_or_none!(network.allow_dns);
            }
            NetworkFlag::AllowGlobal => {
                true_or_none!(network.allow_global);
            }
            NetworkFlag::AllowManaged => {
                true_or_none!(network.allow_managed);
            }
            NetworkFlag::AllowDefault => {
                true_or_none!(network.allow_default);
            }
        }

        s.send(local.update_network(&id, &network).await).unwrap();
    });

    let timeout = Instant::now();

    loop {
        if let Ok(res) = r.try_recv() {
            t.shutdown_background();
            return Ok(res?);
        } else {
            std::thread::sleep(Duration::new(0, 10));
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            t.shutdown_background();
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }
}

pub fn sync_get_network(
    client: Client,
    network_id: String,
) -> Result<ResponseValue<CentralNetwork>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    t.spawn(async move { s.send(client.get_network_by_id(&network_id).await).unwrap() });

    let timeout = Instant::now();

    loop {
        if let Ok(res) = r.try_recv() {
            t.shutdown_background();
            return Ok(res?);
        } else {
            std::thread::sleep(Duration::new(0, 10));
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            t.shutdown_background();
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }
}

pub fn sync_apply_network_rules(
    client: Client,
    network_id: String,
    rules: String,
) -> Result<ResponseValue<CentralNetwork>, anyhow::Error> {
    let (s, mut r) = mpsc::unbounded_channel();

    let t = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    t.spawn(async move {
        let mut net = client.get_network_by_id(&network_id).await.unwrap();
        net.rules_source = Some(rules);
        let res = client.update_network(&network_id, &net).await;
        s.send(res).unwrap();
    });

    let timeout = Instant::now();

    loop {
        if let Ok(res) = r.try_recv() {
            t.shutdown_background();
            return Ok(res?);
        } else {
            std::thread::sleep(Duration::new(0, 10));
        }

        if timeout.elapsed() > Duration::new(3, 0) {
            t.shutdown_background();
            return Err(anyhow!("timeout reading from zerotier"));
        }
    }
}
