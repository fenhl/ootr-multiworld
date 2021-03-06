#![deny(rust_2018_idioms, unused, unused_crate_dependencies, unused_import_braces, unused_lifetimes, unused_qualifications, warnings)]
#![forbid(unsafe_code)]

use {
    std::{
        collections::{
            BTreeSet,
            HashMap,
            HashSet,
        },
        mem,
        net::{
            Ipv4Addr,
            Ipv6Addr,
        },
        num::NonZeroU8,
        sync::Arc,
    },
    async_proto::Protocol,
    async_recursion::async_recursion,
    chrono::prelude::*,
    itertools::Itertools as _,
    tokio::{
        net::{
            TcpStream,
            tcp::OwnedWriteHalf,
        },
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

#[derive(Debug, Clone, Copy)]
pub struct Item {
    pub source: NonZeroU8,
    pub key: u32,
    pub kind: u16,
}

#[derive(Debug)]
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
                    self.player_queues.entry(target_world).or_insert_with(|| self.base_queue.clone()).push(Item { source, key, kind });
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

#[derive(Debug, Clone, Protocol)]
pub enum ServerMessage {
    /// An error has occurred. Contains a human-readable error message.
    Error(String),
    /// A new room has been created.
    NewRoom(String),
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

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)] Read(#[from] async_proto::ReadError),
    #[error(transparent)] Write(#[from] async_proto::WriteError),
    #[error("protocol version mismatch: server is version {0} but we're version {}", VERSION)]
    VersionMismatch(u8),
}

pub async fn handshake(tcp_stream: &mut TcpStream) -> Result<BTreeSet<String>, ClientError> {
    VERSION.write(tcp_stream).await?;
    let server_version = u8::read(tcp_stream).await?;
    if server_version != VERSION { return Err(ClientError::VersionMismatch(server_version)) }
    Ok(BTreeSet::read(tcp_stream).await?)
}

pub fn handshake_sync(tcp_stream: &mut std::net::TcpStream) -> Result<BTreeSet<String>, ClientError> {
    VERSION.write_sync(tcp_stream)?;
    let server_version = u8::read_sync(tcp_stream)?;
    if server_version != VERSION { return Err(ClientError::VersionMismatch(server_version)) }
    Ok(BTreeSet::read_sync(tcp_stream)?)
}

fn render_filename(name: [u8; 8]) -> String {
    let filename_encoding = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', 'A', 'B', 'C', 'D', 'E',
        'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U',
        'V', 'W', 'X', 'Y', 'Z', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k',
        'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', ' ',
        '???', '?', '!', ':', '-', '(', ')', '???', '???', ',', '.', '/', '???', '???', '???', '???',
        '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???', '???',
    ];
    name.into_iter().map(|c| filename_encoding[usize::from(c)]).collect()
}

pub fn format_room_state(players: &[Player], num_unassigned_clients: u8, my_world: Option<NonZeroU8>) -> String {
    match (players.len(), num_unassigned_clients) {
        (0, 0) => unreachable!(), // the current client should always be in the room
        (0, unassigned) => format!("{unassigned} client{} with no world", if unassigned == 1 { "" } else { "s" }),
        (_, unassigned) => {
            let mut buf = players.iter()
                .map(|player| if player.name == Player::DEFAULT_NAME {
                    if my_world == Some(player.world) {
                        format!("{}. [create save file 1 to set name]", player.world)
                    } else {
                        format!("{}. [unnamed]", player.world)
                    }
                } else {
                    format!("{}. {}", player.world, render_filename(player.name))
                })
                .join("\r\n");
            if unassigned > 0 {
                buf.push_str(&format!("\r\n???and {unassigned} client{} with no world", if unassigned == 1 { "" } else { "s" }));
            }
            buf
        }
    }
}
