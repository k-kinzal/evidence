use std::collections::{BTreeSet, HashMap};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cas_fs::{Hasher, ObjectId};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::record::{self, MAX_RECORD, Record};
use super::store::{self, Key, Records, Store};
use crate::format::DaemonConfig;
use crate::layout::{DIR, Layout, LoadedConfig};
use crate::{Error, Result};

pub(super) const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
const PORT: u16 = 7467;
const LEAF: usize = 64;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(super) enum Peer {
    Local(Store),
    Tcp(Client),
}

impl Peer {
    pub(super) fn open(root: &Path, url: &str) -> Result<Self> {
        if let Some(address) = url.strip_prefix("tcp://") {
            return Ok(Self::Tcp(Client::connect(address)?));
        }
        if url.contains("://") {
            return Err(Error::Usage(format!("unsupported peer URL `{url}`")));
        }
        let dir = root.join(url);
        if !dir.join(DIR).is_dir() {
            return Err(Error::NoProject(dir));
        }
        Ok(Self::Local(Store::new(&dir)))
    }

    pub(super) fn get(&mut self, key: Key) -> Result<Option<Vec<u8>>> {
        match self {
            Self::Local(store) => store.object_store().read(key),
            Self::Tcp(client) => {
                send(&mut client.stream, &Request::Get(key))?;
                match reply(&mut client.stream)? {
                    Reply::Found => receive_bytes(&mut client.stream, MAX_PAYLOAD).map(Some),
                    Reply::Missing => Ok(None),
                    _ => Err(protocol()),
                }
            }
        }
    }

    pub(super) fn fetch(
        &mut self,
        line: &str,
        targets: Option<&BTreeSet<String>>,
        known: &Records,
    ) -> Result<Records> {
        match self {
            Self::Local(store) => Ok(subset(&store.records()?, line, targets)),
            Self::Tcp(client) => client.fetch(line, targets, known),
        }
    }

    pub(super) fn offer(&mut self, id: ObjectId, record: &Record, from: &Store) -> Result<Offer> {
        match self {
            Self::Local(store) => {
                let mut objects = 0;
                let complete = store::transfer(&record.lock, &store.object_store(), |key| {
                    let value = from.object_store().read(key)?;
                    objects += usize::from(value.is_some());
                    Ok(value)
                })?
                .is_some();
                let added = record::put(&store.record_path(id), &record.encode()?)?;
                Ok(Offer {
                    added,
                    objects,
                    complete,
                })
            }
            Self::Tcp(client) => client.offer(id, record, from),
        }
    }
}

pub(super) fn subset(records: &Records, line: &str, targets: Option<&BTreeSet<String>>) -> Records {
    let line_records = records.iter().filter(|(_, r)| r.branch == line);
    let ids = targets.map(|ids| {
        let mut selected = ids.clone();
        selected.extend(
            records
                .values()
                .filter(|record| {
                    ids.iter().any(|target| {
                        record.path == *target
                            || record.path.strip_suffix(".evid") == Some(target.as_str())
                            || Path::new(&record.path).starts_with(target)
                    })
                })
                .map(|record| record.id.clone()),
        );
        store::dependencies(records, selected)
    });
    let selected = line_records
        .filter(|(_, r)| ids.as_ref().is_none_or(|ids| ids.contains(&r.id)))
        .map(|(id, _)| *id);
    store::ancestors(records, selected)
        .into_iter()
        .map(|id| (id, records[&id].clone()))
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
enum Request {
    Scope {
        branch: String,
        targets: Option<BTreeSet<String>>,
    },
    Range(String),
    Record(ObjectId),
    Get(Key),
    Offer(ObjectId),
}

#[derive(Debug, Serialize, Deserialize)]
enum Reply {
    Ready,
    Range {
        count: usize,
        fingerprint: ObjectId,
        ids: Option<Vec<ObjectId>>,
    },
    Found,
    Missing,
    NeedRecord,
    Need(Key),
    Accepted(Offer),
    Error(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Offer {
    pub(super) added: bool,
    pub(super) objects: usize,
    pub(super) complete: bool,
}

#[derive(Debug)]
pub(super) struct Client {
    stream: TcpStream,
}

impl Client {
    fn connect(address: &str) -> Result<Self> {
        let address = if !address.contains(':') || address.ends_with(']') {
            format!("{address}:{PORT}")
        } else {
            address.to_owned()
        };
        let mut last = None;
        for addr in address.to_socket_addrs().map_err(network)? {
            match TcpStream::connect_timeout(&addr, TIMEOUT) {
                Ok(stream) => {
                    configure(&stream)?;
                    if !matches!(reply(&mut &stream)?, Reply::Ready) {
                        return Err(protocol());
                    }
                    return Ok(Self { stream });
                }
                Err(error) => last = Some(error),
            }
        }
        Err(network(last.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, address)
        })))
    }

    fn fetch(
        &mut self,
        branch: &str,
        targets: Option<&BTreeSet<String>>,
        known: &Records,
    ) -> Result<Records> {
        send(
            &mut self.stream,
            &Request::Scope {
                branch: branch.to_owned(),
                targets: targets.cloned(),
            },
        )?;
        if !matches!(reply(&mut self.stream)?, Reply::Ready) {
            return Err(protocol());
        }
        let mut prefixes = vec![String::new()];
        let mut result = Records::new();
        let mut different = BTreeSet::new();
        let known = subset(known, branch, targets);
        let local: Vec<_> = known.keys().map(|id| (id.to_hex(), *id)).collect();
        while let Some(prefix) = prefixes.pop() {
            let local_ids = range(&local, &prefix);
            let local_hash = fingerprint(local_ids);
            send(&mut self.stream, &Request::Range(prefix.clone()))?;
            let Reply::Range {
                count,
                fingerprint: remote_hash,
                ids,
            } = reply(&mut self.stream)?
            else {
                return Err(protocol());
            };
            if count == local_ids.len() && remote_hash == local_hash {
                for (_, id) in local_ids {
                    result.insert(*id, known[id].clone());
                }
            } else if let Some(ids) = ids {
                if ids.len() != count
                    || count > LEAF
                    || ids.windows(2).any(|pair| pair[0] >= pair[1])
                    || ids.iter().any(|id| !id.to_hex().starts_with(&prefix))
                {
                    return Err(protocol());
                }
                different.extend(ids);
            } else {
                if prefix.len() >= 64 || count <= LEAF {
                    return Err(protocol());
                }
                prefixes.extend("0123456789abcdef".chars().map(|n| format!("{prefix}{n}")));
            }
        }
        for id in different {
            let record = if let Some(record) = known.get(&id) {
                record.clone()
            } else {
                send(&mut self.stream, &Request::Record(id))?;
                if !matches!(reply(&mut self.stream)?, Reply::Found) {
                    return Err(protocol());
                }
                Record::decode(id, &receive_bytes(&mut self.stream, MAX_RECORD)?)?
            };
            result.insert(id, record);
        }
        store::validate_graph(&result)?;
        Ok(result)
    }

    fn offer(&mut self, id: ObjectId, record: &Record, from: &Store) -> Result<Offer> {
        send(&mut self.stream, &Request::Offer(id))?;
        let objects = from.object_store();
        loop {
            match reply(&mut self.stream)? {
                Reply::NeedRecord => send_bytes(&mut self.stream, &record.encode()?)?,
                Reply::Need(key) => {
                    let bytes = objects.read(key)?;
                    send(&mut self.stream, &bytes.is_some())?;
                    if let Some(bytes) = bytes {
                        send_bytes(&mut self.stream, &bytes)?;
                    }
                }
                Reply::Accepted(result) => return Ok(result),
                _ => return Err(protocol()),
            }
        }
    }
}

fn range<'a>(entries: &'a [(String, ObjectId)], prefix: &str) -> &'a [(String, ObjectId)] {
    let start = entries.partition_point(|(hex, _)| hex.as_str() < prefix);
    let end = entries[start..].partition_point(|(hex, _)| hex.starts_with(prefix));
    &entries[start..start + end]
}

fn fingerprint(entries: &[(String, ObjectId)]) -> ObjectId {
    let mut hasher = Hasher::new();
    hasher.update(b"evidence-range/v1");
    for (_, id) in entries {
        hasher.update(id.as_bytes());
    }
    hasher.finish()
}

/// Serves one initialized project over the TCP peer protocol until interrupted.
pub fn serve(dir: &Path, listen: &str) -> Result<()> {
    let root = Layout::discover(dir)?;
    let policy = Policy::new(LoadedConfig::load(Some(&root))?.config.daemon)?;
    let listener = TcpListener::bind(listen).map_err(network)?;
    let server = Arc::new(Server {
        store: Store::new(&root),
        policy,
        usage: Mutex::new(HashMap::new()),
    });
    let slots = Arc::new(());
    for conn in listener.incoming() {
        let stream = conn.map_err(network)?;
        if Arc::strong_count(&slots) > 32 {
            continue;
        }
        let address = stream.peer_addr().map_err(network)?;
        let server = Arc::clone(&server);
        let slot = Arc::clone(&slots);
        std::thread::spawn(move || {
            let mut stream = stream;
            if let Err(error) = server.handle(&mut stream, address) {
                let _ = send(&mut stream, &Reply::Error(error.to_string()));
                tracing::debug!(%address, %error, "peer session ended");
            }
            drop(slot);
        });
    }
    Ok(())
}

#[derive(Debug)]
struct Server {
    store: Store,
    policy: Policy,
    usage: Mutex<HashMap<IpAddr, u64>>,
}

impl Server {
    fn handle(&self, stream: &mut TcpStream, address: SocketAddr) -> Result<()> {
        configure(stream)?;
        send(stream, &Reply::Ready)?;
        let mut records = None;
        let mut scoped = Vec::new();
        let mut budget = self.policy.push_bytes;
        loop {
            let Some(bytes) = frame(stream, MAX_PAYLOAD)? else {
                return Ok(());
            };
            let request: Request = serde_json::from_slice(&bytes).map_err(record::invalid_json)?;
            if records.is_none() {
                records = Some(self.store.records()?);
            }
            let records = records.as_mut().ok_or_else(protocol)?;
            match request {
                Request::Scope { branch, targets } => {
                    if !record::valid_name(&branch) {
                        return Err(protocol());
                    }
                    scoped = subset(records, &branch, targets.as_ref())
                        .into_keys()
                        .map(|id| (id.to_hex(), id))
                        .collect();
                    send(stream, &Reply::Ready)?;
                }
                Request::Range(prefix) => {
                    if prefix.len() > 64 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(protocol());
                    }
                    let entries = range(&scoped, &prefix);
                    send(
                        stream,
                        &Reply::Range {
                            count: entries.len(),
                            fingerprint: fingerprint(entries),
                            ids: (entries.len() <= LEAF)
                                .then(|| entries.iter().map(|(_, id)| *id).collect()),
                        },
                    )?;
                }
                Request::Record(id) => {
                    let hex = id.to_hex();
                    if scoped.binary_search_by(|(key, _)| key.cmp(&hex)).is_ok()
                        && let Some(record) = records.get(&id)
                    {
                        send(stream, &Reply::Found)?;
                        send_bytes(stream, &record.encode()?)?;
                    } else {
                        send(stream, &Reply::Missing)?;
                    }
                }
                Request::Get(key) => {
                    if let Some(bytes) = self.store.object_store().read(key)? {
                        send(stream, &Reply::Found)?;
                        send_bytes(stream, &bytes)?;
                    } else {
                        send(stream, &Reply::Missing)?;
                    }
                }
                Request::Offer(id) => {
                    let ip = address.ip();
                    if !self.policy.receive {
                        return Err(Error::Invalid("this daemon is read-only".into()));
                    }
                    let mut objects = 0;
                    let mut charge = |size| self.charge(ip, &mut budget, size);
                    let present = self.store.record(id)?;
                    let record = if let Some(record) = present.clone() {
                        record
                    } else {
                        send(stream, &Reply::NeedRecord)?;
                        let bytes = receive_bytes(stream, MAX_RECORD)?;
                        charge(bytes.len())?;
                        Record::decode(id, &bytes)?
                    };
                    if !self.policy.allows(&record.branch, ip) {
                        if present.is_some() {
                            let complete =
                                store::transfer(&record.lock, &self.store.object_store(), |_| {
                                    Ok(None)
                                })?
                                .is_some();
                            send(
                                stream,
                                &Reply::Accepted(Offer {
                                    added: false,
                                    objects: 0,
                                    complete,
                                }),
                            )?;
                            continue;
                        }
                        return Err(Error::Invalid(format!(
                            "push to branch {} from {ip} is denied",
                            record.branch
                        )));
                    }
                    self.store
                        .load_with_parents(record.parents.iter().copied(), records)?;
                    store::validate_edges(records, id, &record)?;
                    let complete =
                        store::transfer(&record.lock, &self.store.object_store(), |key| {
                            send(stream, &Reply::Need(key))?;
                            if !receive::<bool>(stream)? {
                                return Ok(None);
                            }
                            let bytes = receive_bytes(stream, MAX_PAYLOAD)?;
                            charge(bytes.len())?;
                            objects += 1;
                            Ok(Some(bytes))
                        })?
                        .is_some();
                    let added = record::put(&self.store.record_path(id), &record.encode()?)?;
                    records.insert(id, record);
                    send(
                        stream,
                        &Reply::Accepted(Offer {
                            added,
                            objects,
                            complete,
                        }),
                    )?;
                }
            }
        }
    }

    fn charge(&self, ip: IpAddr, budget: &mut u64, size: usize) -> Result<()> {
        let size = size as u64;
        let limit = || Error::Invalid("daemon push size limit exceeded".into());
        let next_budget = budget.checked_sub(size).ok_or_else(limit)?;
        let mut usage = self.usage.lock().map_err(|_| protocol())?;
        let totals = self.store.root.join("local/daemon");
        let path = totals.join(ip.to_string().replace(':', "_"));
        let prev = if let Some(prev) = usage.get(&ip) {
            *prev
        } else {
            record::read(&path, 32)?.map_or(Ok(0), |bytes| {
                std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(protocol)
            })?
        };
        let next = prev
            .checked_add(size)
            .filter(|n| *n <= self.policy.peer_bytes)
            .ok_or_else(limit)?;
        std::fs::create_dir_all(&totals).map_err(|error| Error::io(&totals, error))?;
        crate::format::document::atomic_write(&path, next.to_string().as_bytes())?;
        usage.insert(ip, next);
        *budget = next_budget;
        Ok(())
    }
}

#[derive(Debug)]
struct Policy {
    receive: bool,
    write: std::collections::BTreeMap<String, Vec<Network>>,
    push_bytes: u64,
    peer_bytes: u64,
}

#[derive(Debug)]
struct Network {
    address: IpAddr,
    prefix: u32,
}

impl Policy {
    fn new(config: DaemonConfig) -> Result<Self> {
        let mut write = std::collections::BTreeMap::new();
        for (branch, values) in config.write {
            if branch != "*" && !record::valid_name(&branch) {
                return Err(Error::Config(format!("invalid daemon branch `{branch}`")));
            }
            let mut rules = Vec::new();
            for value in values {
                let (ip, prefix) = value
                    .split_once('/')
                    .map_or((value.as_str(), None), |(ip, n)| (ip, Some(n)));
                let bad = || Error::Config(format!("invalid address `{value}` in daemon.write"));
                let address: IpAddr = ip.parse().map_err(|_| bad())?;
                let bits = if address.is_ipv4() { 32 } else { 128 };
                let prefix = prefix.map_or(Ok(bits), str::parse).map_err(|_| bad())?;
                if prefix > bits {
                    return Err(bad());
                }
                rules.push(Network { address, prefix });
            }
            write.insert(branch, rules);
        }
        Ok(Self {
            receive: config.receive,
            write,
            push_bytes: config.push_bytes.unwrap_or(256 * 1024 * 1024),
            peer_bytes: config.peer_bytes.unwrap_or(4 * 1024 * 1024 * 1024),
        })
    }

    fn allows(&self, branch: &str, ip: IpAddr) -> bool {
        self.write
            .get(branch)
            .or_else(|| self.write.get("*"))
            .is_some_and(|rules| {
                rules.iter().any(|rule| {
                    let (a, b, bits) = match (rule.address, ip) {
                        (IpAddr::V4(a), IpAddr::V4(b)) => {
                            (u128::from(u32::from(a)), u128::from(u32::from(b)), 32)
                        }
                        (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a), u128::from(b), 128),
                        _ => return false,
                    };
                    rule.prefix == 0 || (a ^ b) >> (bits - rule.prefix) == 0
                })
            })
    }
}

fn configure(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(TIMEOUT)).map_err(network)?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(network)?;
    stream.set_nodelay(true).map_err(network)
}

fn send(stream: &mut impl Write, message: &impl Serialize) -> Result<()> {
    send_bytes(
        stream,
        &serde_json::to_vec(message).map_err(record::invalid_json)?,
    )
}

fn send_bytes(stream: &mut impl Write, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PAYLOAD {
        return Err(protocol());
    }
    let size = u32::try_from(bytes.len()).map_err(|_| protocol())?;
    stream.write_all(&size.to_be_bytes()).map_err(network)?;
    stream.write_all(bytes).map_err(network)
}

fn frame(stream: &mut impl Read, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut size = [0; 4];
    if stream.read(&mut size[..1]).map_err(network)? == 0 {
        return Ok(None);
    }
    stream.read_exact(&mut size[1..]).map_err(network)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > limit {
        return Err(protocol());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).map_err(network)?;
    Ok(Some(bytes))
}

fn receive_bytes(stream: &mut impl Read, limit: usize) -> Result<Vec<u8>> {
    frame(stream, limit)?.ok_or_else(protocol)
}

fn receive<T: DeserializeOwned>(stream: &mut impl Read) -> Result<T> {
    serde_json::from_slice(&receive_bytes(stream, MAX_PAYLOAD)?).map_err(record::invalid_json)
}

fn reply(stream: &mut impl Read) -> Result<Reply> {
    match receive(stream)? {
        Reply::Error(message) => Err(Error::Invalid(format!("peer: {message}"))),
        reply => Ok(reply),
    }
}

fn protocol() -> Error {
    Error::Invalid("invalid peer protocol message".into())
}

fn network(error: io::Error) -> Error {
    Error::io(PathBuf::from("TCP peer"), error)
}
