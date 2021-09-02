use {
    std::{
        collections::{
            BTreeSet,
            HashMap,
            HashSet,
        },
        io,
        mem,
        net::{
            Ipv4Addr,
            Ipv6Addr,
            SocketAddr,
            ToSocketAddrs,
        },
        num::NonZeroU8,
        sync::Arc,
        time::Duration,
    },
    async_proto::Protocol,
    async_recursion::async_recursion,
    chrono::prelude::*,
    derivative::Derivative,
    derive_more::From,
    tokio::{
        net::tcp::OwnedWriteHalf,
        sync::Mutex,
    },
};
#[cfg(unix)] use std::os::unix::io::AsRawFd;
#[cfg(windows)] use std::os::windows::io::AsRawSocket;

pub const ADDRESS_V4: Ipv4Addr = Ipv4Addr::new(37, 252, 122, 84);
pub const ADDRESS_V6: Ipv6Addr = Ipv6Addr::new(0x2a02, 0x2770, 0x8, 0, 0x21a, 0x4aff, 0xfee1, 0xf281);
pub const PORT: u16 = 24809;
pub const VERSION: u8 = 1;

const TRIFORCE_PIECE: u16 = 0xca;

#[cfg(unix)] pub type SocketId = std::os::unix::io::RawFd;
#[cfg(windows)] pub type SocketId = std::os::windows::io::RawSocket;

#[cfg(unix)] pub fn socket_id<T: AsRawFd>(socket: &T) -> SocketId { socket.as_raw_fd() }
#[cfg(windows)] pub fn socket_id<T: AsRawSocket>(socket: &T) -> SocketId { socket.as_raw_socket() }

#[derive(Debug, Clone, Copy, Protocol)]
pub struct Player {
    pub world: NonZeroU8,
    pub name: [u8; 8],
}

impl Player {
    pub const DEFAULT_NAME: [u8; 8] = [0xdf; 8];

    pub fn new(world: NonZeroU8) -> Self {
        Self {
            world,
            name: Self::DEFAULT_NAME,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Item {
    pub source: NonZeroU8,
    pub key: u32,
    pub kind: u16,
}

pub struct Room {
    pub password: String,
    pub clients: HashMap<SocketId, (Option<Player>, Arc<Mutex<OwnedWriteHalf>>)>,
    pub base_queue: Vec<Item>,
    pub player_queues: HashMap<NonZeroU8, Vec<Item>>,
}

impl Room {
    async fn write(&mut self, client_id: SocketId, msg: &ServerMessage) {
        if let Some((_, writer)) = self.clients.get(&client_id) {
            let mut writer = writer.lock().await;
            if let Err(e) = msg.write(&mut *writer).await {
                eprintln!("{} error sending message: {:?}", Utc::now().format("%Y-%m-%d %H:%M:%S"), e);
                drop(writer);
                self.remove_client(client_id).await;
            }
        }
    }

    async fn write_all(&mut self, msg: &ServerMessage) {
        let mut notified = HashSet::new();
        while let Some((&client_id, (_, writer))) = self.clients.iter().find(|&(client_id, _)| !notified.contains(client_id)) {
            let mut writer = writer.lock().await;
            if let Err(e) = msg.write(&mut *writer).await {
                eprintln!("{} error sending message: {:?}", Utc::now().format("%Y-%m-%d %H:%M:%S"), e);
                drop(writer);
                self.remove_client(client_id).await;
            }
            notified.insert(client_id);
        }
    }

    pub async fn add_client(&mut self, client_id: SocketId, writer: Arc<Mutex<OwnedWriteHalf>>) {
        // the client doesn't need to be told that it has connected, so notify everyone *before* adding it
        self.write_all(&ServerMessage::ClientConnected).await;
        self.clients.insert(client_id, (None, writer));
    }

    pub fn has_client(&self, client_id: SocketId) -> bool {
        self.clients.contains_key(&client_id)
    }

    #[async_recursion]
    pub async fn remove_client(&mut self, client_id: SocketId) {
        if let Some((player, _)) = self.clients.remove(&client_id) {
            let msg = if let Some(Player { world, .. }) = player {
                ServerMessage::PlayerDisconnected(world)
            } else {
                ServerMessage::UnregisteredClientDisconnected
            };
            self.write_all(&msg).await;
        }
    }

    /// Moves a player from unloaded (no world assigned) to the given `world`.
    pub async fn load_player(&mut self, client_id: SocketId, world: NonZeroU8) -> bool {
        if self.clients.iter().any(|(&iter_client_id, (iter_player, _))| iter_player.as_ref().map_or(false, |p| p.world == world) && iter_client_id != client_id) {
            return false
        }
        let prev_player = &mut self.clients.get_mut(&client_id).expect("no such client").0;
        if let Some(player) = prev_player {
            let prev_world = mem::replace(&mut player.world, world);
            if prev_world == world { return true }
            self.write_all(&ServerMessage::ResetPlayerId(prev_world)).await;
        } else {
            *prev_player = Some(Player::new(world));
        }
        self.write_all(&ServerMessage::PlayerId(world)).await;
        let queue = self.player_queues.get(&world).unwrap_or(&self.base_queue).iter().map(|item| item.kind).collect::<Vec<_>>();
        if !queue.is_empty() {
            self.write(client_id, &ServerMessage::ItemQueue(queue)).await;
        }
        true
    }

    pub async fn unload_player(&mut self, client_id: SocketId) {
        if let Some(prev_player) = self.clients.get_mut(&client_id).expect("no such client").0.take() {
            self.write_all(&ServerMessage::ResetPlayerId(prev_player.world)).await;
        }
    }

    pub async fn set_player_name(&mut self, client_id: SocketId, name: [u8; 8]) -> bool {
        if let Some(ref mut player) = self.clients.get_mut(&client_id).expect("no such client").0 {
            let world = player.world;
            player.name = name;
            drop(player);
            self.write_all(&ServerMessage::PlayerName(world, name)).await;
            true
        } else {
            false
        }
    }

    pub async fn queue_item(&mut self, source_client: SocketId, key: u32, kind: u16, target_world: NonZeroU8) -> bool {
        if let Some(source) = self.clients.get(&source_client).expect("no such client").0.map(|source_player| source_player.world) {
            if kind == TRIFORCE_PIECE {
                if !self.base_queue.iter().any(|item| item.source == source && item.key == key) {
                    let item = Item { source, key, kind };
                    self.base_queue.push(item);
                    for queue in self.player_queues.values_mut() {
                        queue.push(item);
                    }
                    let msg = ServerMessage::GetItem(kind);
                    let player_clients = self.clients.iter()
                        .filter_map(|(&target_client, (p, _))| if p.map_or(false, |p| p.world != source) { Some(target_client) } else { None })
                        .collect::<Vec<_>>();
                    for target_client in player_clients {
                        self.write(target_client, &msg).await;
                    }
                }
            } else {
                if !self.player_queues.get(&target_world).map_or(false, |queue| queue.iter().any(|item| item.source == source && item.key == key)) {
                    let base_queue = &self.base_queue; //TODO (Rust 2021) remove this line
                    self.player_queues.entry(target_world).or_insert_with(|| base_queue.clone()).push(Item { source, key, kind });
                    if let Some((&target_client, _)) = self.clients.iter().find(|(_, (p, _))| p.map_or(false, |p| p.world == target_world)) {
                        self.write(target_client, &ServerMessage::GetItem(kind)).await;
                    }
                }
            }
            true
        } else {
            false
        }
    }
}

#[derive(Protocol)]
pub enum LobbyClientMessage {
    JoinRoom {
        name: String,
        password: String,
    },
    CreateRoom {
        name: String,
        password: String,
    },
    Encrypt,
}

#[derive(Protocol)]
pub enum RoomClientMessage {
    /// Claims a world.
    PlayerId(NonZeroU8),
    /// Unloads the previously claimed world.
    ResetPlayerId,
    /// Player names are encoded in the NTSC charset, with trailing spaces (`0xdf`).
    PlayerName([u8; 8]),
    SendItem {
        key: u32,
        kind: u16,
        target_world: NonZeroU8,
    },
}

#[derive(Debug, Protocol)]
pub enum ServerMessage {
    /// An error has occurred. Contains a human-readable error message.
    Error(String),
    /// You have created or joined a room.
    EnterRoom {
        players: Vec<Player>,
        num_unassigned_clients: u8,
    },
    /// A previously unassigned world has been taken by a client.
    PlayerId(NonZeroU8),
    /// A previously assigned world has been unassigned.
    ResetPlayerId(NonZeroU8),
    /// A new (unassigned) client has connected to the room.
    ClientConnected,
    /// A client with a world has disconnected from the room.
    PlayerDisconnected(NonZeroU8),
    /// A client without a world has disconnected from the room.
    UnregisteredClientDisconnected,
    /// A player has changed their name.
    ///
    /// Player names are encoded in the NTSC charset, with trailing spaces (`0xdf`).
    PlayerName(NonZeroU8, [u8; 8]),
    /// Your list of received items has changed.
    ItemQueue(Vec<u16>),
    /// You have received a new item, add it to the end of your item queue.
    GetItem(u16),
}

#[derive(Debug, From)]
pub enum ClientError {
    Io(io::Error),
    Read(async_proto::ReadError),
    VersionMismatch(u8),
    Write(async_proto::WriteError),
}

pub enum Host {
    DefaultIpv6,
    DefaultIpv4,
    Custom(Vec<SocketAddr>),
}

pub trait IntoHost {
    fn into_host(self) -> Result<Host, ClientError>;
}

impl IntoHost for Host {
    fn into_host(self) -> Result<Host, ClientError> {
        Ok(self)
    }
}

impl<'a, T: ToSocketAddrs> IntoHost for &'a T {
    fn into_host(self) -> Result<Host, ClientError> {
        Ok(Host::Custom(self.to_socket_addrs()?.collect()))
    }
}

impl ToSocketAddrs for Host {
    type Iter = Box<dyn Iterator<Item = SocketAddr>>;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter> {
        match self {
            Host::DefaultIpv4 => Ok(Box::new((ADDRESS_V4, PORT).to_socket_addrs()?)),
            Host::DefaultIpv6 => Ok(Box::new((ADDRESS_V6, PORT).to_socket_addrs()?)),
            Host::Custom(addrs) => Ok(Box::new(addrs.clone().into_iter())),
        }
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub enum SyncStream<S: rustls::Session> {
    Encrypted(#[derivative(Debug = "ignore")] rustls::StreamOwned<S, std::net::TcpStream>),
    Unencrypted(std::net::TcpStream),
}

impl<S: rustls::Session> SyncStream<S> {
    pub fn set_nonblocking(&mut self, nonblocking: bool) -> io::Result<()> {
        match self {
            SyncStream::Encrypted(inner) => inner.sock.set_nonblocking(nonblocking),
            SyncStream::Unencrypted(inner) => inner.set_nonblocking(nonblocking),
        }
    }
}

impl<S: rustls::Session> io::Read for SyncStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            SyncStream::Encrypted(inner) => inner.read(buf),
            SyncStream::Unencrypted(inner) => inner.read(buf),
        }
    }
}

impl<S: rustls::Session> io::Write for SyncStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            SyncStream::Encrypted(inner) => inner.write(buf),
            SyncStream::Unencrypted(inner) => inner.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            SyncStream::Encrypted(inner) => inner.flush(),
            SyncStream::Unencrypted(inner) => inner.flush(),
        }
    }
}

pub fn connect_sync(host: impl IntoHost) -> Result<(SyncStream<rustls::ClientSession>, BTreeSet<String>), ClientError> {
    let host = host.into_host()?;
    let mut tcp_stream = std::net::TcpStream::connect(&host)?;
    tcp_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    tcp_stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    VERSION.write_sync(&mut tcp_stream)?;
    let server_version = u8::read_sync(&mut tcp_stream)?;
    if server_version != VERSION { return Err(ClientError::VersionMismatch(server_version)) }
    let mut tcp_stream = if matches!(host, Host::DefaultIpv4 | Host::DefaultIpv6) {
        LobbyClientMessage::Encrypt.write_sync(&mut tcp_stream)?;
        let mut config = rustls::ClientConfig::new();
        config.root_store.add_server_trust_anchors(&webpki_roots::TLS_SERVER_ROOTS);
        let config = Arc::new(config);
        let dns_name = webpki::DNSNameRef::try_from_ascii_str("fenhl.net").expect("invalid DNS name");
        let client = rustls::ClientSession::new(&config, dns_name);
        SyncStream::Encrypted(rustls::StreamOwned::new(client, tcp_stream))
    } else {
        SyncStream::Unencrypted(tcp_stream)
    };
    let rooms = BTreeSet::read_sync(&mut tcp_stream)?;
    Ok((tcp_stream, rooms))
}
