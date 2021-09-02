use {
    std::{
        collections::{
            HashMap,
            hash_map,
        },
        convert::{
            Infallible as Never,
            TryFrom as _,
        },
        net::Ipv6Addr,
        sync::Arc,
    },
    async_proto::Protocol as _,
    chrono::prelude::*,
    derive_more::From,
    tokio::{
        io,
        net::{
            TcpListener,
            tcp::{
                OwnedReadHalf,
                OwnedWriteHalf,
            },
        },
        sync::{
            Mutex,
            RwLock,
        },
    },
    multiworld::{
        LobbyClientMessage,
        Player,
        Room,
        RoomClientMessage,
        ServerMessage,
    },
};

#[derive(From, Debug)]
enum SessionError {
    Read(async_proto::ReadError),
    #[from(ignore)]
    VersionMismatch(u8),
    Write(async_proto::WriteError),
}

async fn client_session(rooms: Arc<RwLock<HashMap<String, Arc<RwLock<Room>>>>>, socket: TcpStream) -> Result<(), SessionError> {
    macro_rules! error {
        ($($msg:tt)*) => {{
            let msg = format!($($msg)*);
            ServerMessage::Error(msg).write(&mut *writer.lock().await).await?;
            return Ok(())
        }};
    }

    let socket_id = multiworld::socket_id(&socket);
    multiworld::VERSION.write(&mut *writer.lock().await).await?;
    let client_version = u8::read(&mut reader).await?;
    if client_version != multiworld::VERSION { return Err::<(), _>(SessionError::VersionMismatch(client_version)) }
    {
        // finish handshake by sending room list (treated as a single packet)
        let mut writer = writer.lock().await;
        let rooms = rooms.read().await;
        u64::try_from(rooms.len()).expect("too many rooms").write(&mut *writer).await?;
        for room_name in rooms.keys() {
            room_name.write(&mut *writer).await?;
        }
    }
    //TODO keep room list up to date
    let (reader, writer, room) = loop {
        match LobbyClientMessage::read(&mut reader).await? {
            LobbyClientMessage::JoinRoom { name, password } => if let Some(room) = rooms.read().await.get(&name) {
                let (reader, writer) = socket.into_split();
                let writer = Arc::new(Mutex::new(writer));
                if room.read().await.password != password { error!("wrong password for room {:?}", name) }
                if room.read().await.clients.len() >= u8::MAX.into() { error!("room {:?} is full", name) }
                {
                    let mut room = room.write().await;
                    room.add_client(socket_id, Arc::clone(&writer)).await;
                    let mut players = Vec::<Player>::default();
                    let mut num_unassigned_clients = 0;
                    for &(player, _) in room.clients.values() {
                        if let Some(player) = player {
                            players.insert(players.binary_search_by_key(&player.world, |p| p.world).expect_err("duplicate world number"), player);
                        } else {
                            num_unassigned_clients += 1;
                        }
                    }
                    ServerMessage::EnterRoom { players, num_unassigned_clients }.write(&mut *writer.lock().await).await?;
                }
                break (reader, writer, Arc::clone(room))
            } else {
                error!("there is no room named {:?}", name)
            },
            LobbyClientMessage::CreateRoom { name, password } => {
                let (reader, writer) = socket.into_split();
                let writer = Arc::new(Mutex::new(writer));
                //TODO disallow creating new rooms if preparing for reboot? (or at least warn)
                if name.is_empty() { error!("room name must not be empty") }
                if name.chars().count() >= 64 { error!("room name too long (maximum 64 characters)") }
                if name.contains('\0') { error!("room name must not contain null characters") }
                if password.chars().count() >= 64 { error!("room password too long (maximum 64 characters)") }
                if password.contains('\0') { error!("room password must not contain null characters") }
                match rooms.write().await.entry(name) {
                    hash_map::Entry::Occupied(_) => error!("a room with this name already exists"),
                    hash_map::Entry::Vacant(entry) => {
                        let mut clients = HashMap::default();
                        clients.insert(socket_id, (None, Arc::clone(&writer)));
                        let room = Arc::new(RwLock::new(Room {
                            password, clients,
                            base_queue: Vec::default(),
                            player_queues: HashMap::default(),
                        }));
                        entry.insert(Arc::clone(&room));
                        //TODO automatically delete rooms after 7 days of inactivity (reduce to 24 hours after backup system is implemented, to reduce room list clutter)
                        ServerMessage::EnterRoom {
                            players: Vec::default(),
                            num_unassigned_clients: 1,
                        }.write(&mut *writer.lock().await).await?;
                        break (reader, writer, room)
                    }
                }
            }
            LobbyClientMessage::Encrypt => {
                unimplemented!() //TODO enable TLS
            }
        }
    };
    loop {
        match RoomClientMessage::read(&mut reader).await? {
            RoomClientMessage::PlayerId(id) => if !room.write().await.load_player(socket_id, id).await {
                error!("world {} is already taken", id)
            },
            RoomClientMessage::ResetPlayerId => room.write().await.unload_player(socket_id).await,
            RoomClientMessage::PlayerName(name) => if !room.write().await.set_player_name(socket_id, name).await {
                error!("please claim a world before setting your player name")
            },
            RoomClientMessage::SendItem { key, kind, target_world } => if !room.write().await.queue_item(socket_id, key, kind, target_world).await {
                error!("please claim a world before sending items")
            },
        }
    }
}

#[wheel::main]
async fn main() -> io::Result<Never> {
    let rooms = Arc::new(RwLock::new(HashMap::default()));
    let listener = TcpListener::bind((Ipv6Addr::UNSPECIFIED, multiworld::PORT)).await?;
    loop {
        let (socket, _) = listener.accept().await?;
        let rooms = Arc::clone(&rooms);
        tokio::spawn(async move {
            if let Err(e) = client_session(Arc::clone(&rooms), socket).await {
                eprintln!("{} error in client session: {:?}", Utc::now().format("%Y-%m-%d %H:%M:%S"), e);
            }
            for room in rooms.write().await.values_mut() {
                if room.read().await.has_client(socket_id) {
                    room.write().await.remove_client(socket_id).await;
                }
            }
        });
    }
}
