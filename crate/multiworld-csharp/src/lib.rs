use {
    std::{
        array,
        convert::{
            TryFrom as _,
            TryInto as _,
        },
        ffi::{
            CStr,
            CString,
        },
        fmt,
        num::NonZeroU8,
        slice,
    },
    async_proto::Protocol,
    itertools::Itertools as _,
    libc::c_char,
    multiworld::{
        LobbyClientMessage,
        Player,
        RoomClientMessage,
        ServerMessage,
    },
};

#[repr(transparent)]
pub struct HandleOwned<T: ?Sized>(*mut T);

impl<T: ?Sized> HandleOwned<T> {
    fn new(value: T) -> HandleOwned<T>
    where T: Sized {
        HandleOwned(Box::into_raw(Box::new(value)))
    }

    /// # Safety
    ///
    /// `self` must point at a valid `T`. This function takes ownership of the `T`.
    unsafe fn into_box(self) -> Box<T> {
        assert!(!self.0.is_null());
        Box::from_raw(self.0)
    }
}

type StringHandle = HandleOwned<c_char>;

impl StringHandle {
    fn from_string(s: impl ToString) -> StringHandle {
        HandleOwned(CString::new(s.to_string()).unwrap().into_raw())
    }
}

pub struct DebugError(String);

impl<E: fmt::Debug> From<E> for DebugError {
    fn from(e: E) -> DebugError {
        DebugError(format!("{:?}", e))
    }
}

impl fmt::Display for DebugError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A result type where the error has been converted to its `Debug` representation.
/// Useful because it somewhat deduplicates boilerplate on the C# side.
pub type DebugResult<T> = Result<T, DebugError>;

trait DebugResultExt {
    type T;

    fn unwrap(self) -> Self::T;
}

impl<T> DebugResultExt for DebugResult<T> {
    type T = T;

    fn unwrap(self) -> T {
        match self {
            Ok(x) => x,
            Err(e) => panic!("{}", e),
        }
    }
}

#[derive(Debug)]
pub struct LobbyClient {
    tcp_stream: multiworld::SyncStream<rustls::ClientSession>,
    rooms: Vec<String>,
}

#[derive(Debug)]
pub struct RoomClient {
    tcp_stream: multiworld::SyncStream<rustls::ClientSession>,
    buf: Vec<u8>,
    players: Vec<Player>,
    num_unassigned_clients: u8,
    last_world: Option<NonZeroU8>,
    last_name: [u8; 8],
    item_queue: Vec<u16>,
}

impl RoomClient {
    fn try_read<T: Protocol>(&mut self) -> Result<Option<T>, async_proto::ReadError> {
        self.tcp_stream.set_nonblocking(true)?;
        T::try_read(&mut self.tcp_stream, &mut self.buf)
    }

    fn write(&mut self, msg: &impl Protocol) -> Result<(), async_proto::WriteError> {
        self.tcp_stream.set_nonblocking(false)?;
        msg.write_sync(&mut self.tcp_stream)
    }
}

fn render_filename(name: [u8; 8]) -> String {
    let filename_encoding = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'あ', 'い', 'う', 'え', 'お', 'か',
        'き', 'く', 'け', 'こ', 'さ', 'し', 'す', 'せ', 'そ', 'た', 'ち', 'つ', 'て', 'と', 'な', 'に',
        'ぬ', 'ね', 'の', 'は', 'ひ', 'ふ', 'へ', 'ほ', 'ま', 'み', 'む', 'め', 'も', 'や', 'ゆ', 'よ',
        'ら', 'り', 'る', 'れ', 'ろ', 'わ', 'を', 'ん', 'ぁ', 'ぃ', 'ぅ', 'ぇ', 'ぉ', 'っ', 'ゃ', 'ゅ',
        'ょ', 'が', 'ぎ', 'ぐ', 'げ', 'ご', 'ざ', 'じ', 'ず', 'ぜ', 'ぞ', 'だ', 'ぢ', 'づ', 'で', 'ど',
        'ば', 'び', 'ぶ', 'べ', 'ぼ', 'ぱ', 'ぴ', 'ぷ', 'ぺ', 'ぽ', 'ア', 'イ', 'ウ', 'エ', 'オ', 'カ',
        'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ', 'チ', 'ツ', 'テ', 'ト', 'ナ', 'ニ',
        'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ', 'ム', 'メ', 'モ', 'ヤ', 'ユ', 'ヨ',
        'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ヲ', 'ン', 'ァ', 'ィ', 'ゥ', 'ェ', 'ォ', 'ッ', 'ャ', 'ュ',
        'ョ', 'ガ', 'ギ', 'グ', 'ゲ', 'ゴ', 'ザ', 'ジ', 'ズ', 'ゼ', 'ゾ', 'ダ', 'ヂ', 'ヅ', 'デ', 'ド',
        'バ', 'ビ', 'ブ', 'ベ', 'ボ', 'パ', 'ピ', 'プ', 'ペ', 'ポ', 'ヴ', 'A', 'B', 'C', 'D', 'E',
        'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U',
        'V', 'W', 'X', 'Y', 'Z', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k',
        'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', ' ',
        '┬', '?', '!', ':', '-', '(', ')', '゛', '゜', ',', '.', '/', '�', '�', '�', '�',
        '�', '�', '�', '�', '�', '�', '�', '�', '�', '�', '�', '�', '�', '�', '�', '�',
    ];
    array::IntoIter::new(name).map(|c| filename_encoding[usize::from(c)]).collect() //TODO (Rust 2021) use `name.into_iter()` instead
}

#[no_mangle] pub extern "C" fn connect_ipv4() -> HandleOwned<DebugResult<LobbyClient>> {
    HandleOwned::new(multiworld::connect_sync(multiworld::Host::DefaultIpv4)
        .map(|(tcp_stream, rooms)| LobbyClient { tcp_stream, rooms: rooms.into_iter().collect() })
        .map_err(DebugError::from))
}

#[no_mangle] pub extern "C" fn connect_ipv6() -> HandleOwned<DebugResult<LobbyClient>> {
    HandleOwned::new(multiworld::connect_sync(multiworld::Host::DefaultIpv6)
        .map(|(tcp_stream, rooms)| LobbyClient { tcp_stream, rooms: rooms.into_iter().collect() })
        .map_err(DebugError::from))
}

/// # Safety
///
/// `lobby_client_res` must point at a valid `DebugResult<LobbyClient>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn lobby_client_result_free(lobby_client_res: HandleOwned<DebugResult<LobbyClient>>) {
    let _ = lobby_client_res.into_box();
}

/// # Safety
///
/// `lobby_client_res` must point at a valid `DebugResult<LobbyClient>`.
#[no_mangle] pub unsafe extern "C" fn lobby_client_result_is_ok(lobby_client_res: *const DebugResult<LobbyClient>) -> bool {
    (&*lobby_client_res).is_ok()
}

/// # Safety
///
/// `lobby_client_res` must point at a valid `DebugResult<LobbyClient>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn lobby_client_result_unwrap(lobby_client_res: HandleOwned<DebugResult<LobbyClient>>) -> HandleOwned<LobbyClient> {
    HandleOwned::new(lobby_client_res.into_box().unwrap())
}

/// # Safety
///
/// `lobby_client` must point at a valid `LobbyClient`. This function takes ownership of the `LobbyClient`.
#[no_mangle] pub unsafe extern "C" fn lobby_client_free(lobby_client: HandleOwned<LobbyClient>) {
    let _ = lobby_client.into_box();
}

/// # Safety
///
/// `lobby_client_res` must point at a valid `DebugResult<LobbyClient>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn lobby_client_result_debug_err(lobby_client_res: HandleOwned<DebugResult<LobbyClient>>) -> StringHandle {
    StringHandle::from_string(lobby_client_res.into_box().unwrap_err())
}

/// # Safety
///
/// `s` must point at a valid string. This function takes ownership of the string.
#[no_mangle] pub unsafe extern "C" fn string_free(s: StringHandle) {
    let _ = CString::from_raw(s.0);
}

/// # Safety
///
/// `lobby_client` must point at a valid `LobbyClient`.
#[no_mangle] pub unsafe extern "C" fn lobby_client_num_rooms(lobby_client: *const LobbyClient) -> u64 {
    (&*lobby_client).rooms.len().try_into().expect("too many rooms")
}

/// # Safety
///
/// `lobby_client` must point at a valid `LobbyClient`.
///
/// # Panics
///
/// If `i` is out of range.
#[no_mangle] pub unsafe extern "C" fn lobby_client_room_name(lobby_client: *const LobbyClient, i: u64) -> StringHandle {
    StringHandle::from_string(&(&*lobby_client).rooms[usize::try_from(i).expect("index out of range")])
}

/// # Safety
///
/// `lobby_client` must point at a valid `LobbyClient`. This function takes ownership of the `LobbyClient`. `room_name` and `password` must be null-terminated UTF-8 strings.
#[no_mangle] pub unsafe extern "C" fn lobby_client_room_connect(lobby_client: HandleOwned<LobbyClient>, room_name: *const c_char, password: *const c_char) -> HandleOwned<DebugResult<RoomClient>> {
    let mut lobby_client = lobby_client.into_box();
    let name = CStr::from_ptr(room_name).to_str().expect("room name was not valid UTF-8").to_owned();
    let password = CStr::from_ptr(password).to_str().expect("room name was not valid UTF-8");
    HandleOwned::new(if lobby_client.rooms.contains(&name) {
        LobbyClientMessage::JoinRoom { name, password: password.to_owned() }.write_sync(&mut lobby_client.tcp_stream)
    } else {
        LobbyClientMessage::CreateRoom { name, password: password.to_owned() }.write_sync(&mut lobby_client.tcp_stream)
    }.map_err(DebugError::from)
    .and_then(|()| ServerMessage::read_sync(&mut lobby_client.tcp_stream).map_err(DebugError::from))
    .and_then(|msg| match msg {
        ServerMessage::Error(e) => Err(DebugError(e)),
        ServerMessage::EnterRoom { players, num_unassigned_clients } => Ok((players, num_unassigned_clients)),
        _ => Err(DebugError(format!("{:?}", msg))),
    })
    .map(|(players, num_unassigned_clients)| RoomClient {
        players, num_unassigned_clients,
        tcp_stream: lobby_client.tcp_stream,
        buf: Vec::default(),
        last_world: None,
        last_name: Player::DEFAULT_NAME,
        item_queue: Vec::default(),
    }))
}

/// # Safety
///
/// `room_client_res` must point at a valid `DebugResult<RoomClient>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn room_client_result_free(room_client_res: HandleOwned<DebugResult<RoomClient>>) {
    let _ = room_client_res.into_box();
}

/// # Safety
///
/// `room_client_res` must point at a valid `DebugResult<RoomClient>`.
#[no_mangle] pub unsafe extern "C" fn room_client_result_is_ok(room_client_res: *const DebugResult<RoomClient>) -> bool {
    (&*room_client_res).is_ok()
}

/// # Safety
///
/// `room_client_res` must point at a valid `DebugResult<RoomClient>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn room_client_result_unwrap(room_client_res: HandleOwned<DebugResult<RoomClient>>) -> HandleOwned<RoomClient> {
    HandleOwned::new(room_client_res.into_box().unwrap())
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`. This function takes ownership of the `RoomClient`.
#[no_mangle] pub unsafe extern "C" fn room_client_free(room_client: HandleOwned<RoomClient>) {
    let _ = room_client.into_box();
}

/// # Safety
///
/// `room_client_res` must point at a valid `DebugResult<RoomClient>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn room_client_result_debug_err(room_client_res: HandleOwned<DebugResult<RoomClient>>) -> StringHandle {
    StringHandle::from_string(room_client_res.into_box().unwrap_err())
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
///
/// # Panics
///
/// If `id` is `0`.
#[no_mangle] pub unsafe extern "C" fn room_client_set_player_id(room_client: *mut RoomClient, id: u8) -> HandleOwned<DebugResult<()>> {
    let room_client = &mut *room_client;
    let id = NonZeroU8::new(id).expect("tried to claim world 0");
    HandleOwned::new(if room_client.last_world != Some(id) {
        room_client.last_world = Some(id);
        room_client.write(&RoomClientMessage::PlayerId(id)).and_then(|()| if room_client.last_name != Player::DEFAULT_NAME {
            room_client.write(&RoomClientMessage::PlayerName(room_client.last_name))
        } else {
            Ok(())
        }).map_err(DebugError::from)
    } else {
        Ok(())
    })
}

/// # Safety
///
/// `unit_res` must point at a valid `DebugResult<()>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn unit_result_free(unit_res: HandleOwned<DebugResult<()>>) {
    let _ = unit_res.into_box();
}

/// # Safety
///
/// `unit_res` must point at a valid `DebugResult<()>`.
#[no_mangle] pub unsafe extern "C" fn unit_result_is_ok(unit_res: *const DebugResult<()>) -> bool {
    (&*unit_res).is_ok()
}

/// # Safety
///
/// `unit_res` must point at a valid `DebugResult<()>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn unit_result_debug_err(unit_res: HandleOwned<DebugResult<()>>) -> StringHandle {
    StringHandle::from_string(unit_res.into_box().unwrap_err())
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
#[no_mangle] pub unsafe extern "C" fn room_client_reset_player_id(room_client: *mut RoomClient) -> HandleOwned<DebugResult<()>> {
    let room_client = &mut *room_client;
    HandleOwned::new(if room_client.last_world != None {
        room_client.last_world = None;
        room_client.write(&RoomClientMessage::ResetPlayerId).map_err(DebugError::from)
    } else {
        Ok(())
    })
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`. `name` must point at a byte slice of length 8.
#[no_mangle] pub unsafe extern "C" fn room_client_set_player_name(room_client: *mut RoomClient, name: *const u8) -> HandleOwned<DebugResult<()>> {
    let room_client = &mut *room_client;
    let name = slice::from_raw_parts(name, 8);
    HandleOwned::new(if room_client.last_name != name {
        room_client.last_name = name.try_into().expect("player names are 8 bytes");
        if room_client.last_world.is_some() {
            room_client.write(&RoomClientMessage::PlayerName(room_client.last_name)).map_err(DebugError::from)
        } else {
            Ok(())
        }
    } else {
        Ok(())
    })
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
#[no_mangle] pub unsafe extern "C" fn room_client_format_state(room_client: *const RoomClient) -> StringHandle {
    let room_client = &*room_client;
    StringHandle::from_string(match (room_client.players.len(), room_client.num_unassigned_clients) {
        (0, 0) => unreachable!(), // the current client should always be in the room
        (0, unassigned) => format!("{} client{} with no world", unassigned, if unassigned == 1 { "" } else { "s" }),
        (_, unassigned) => {
            let mut buf = room_client.players.iter()
                .map(|player| if player.name == Player::DEFAULT_NAME {
                    if room_client.last_world == Some(player.world) {
                        format!("{}. [create save file 1 to set name]", player.world)
                    } else {
                        format!("{}. [unnamed]", player.world)
                    }
                } else {
                    format!("{}. {}", player.world, render_filename(player.name))
                })
                .join("\r\n");
            if unassigned > 0 {
                buf.push_str(&format!("\r\n…and {} client{} with no world", unassigned, if unassigned == 1 { "" } else { "s" }));
            }
            buf
        }
    })
}

/// Attempts to read a message from the server if one is available, without blocking if there is not.
///
/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
#[no_mangle] pub unsafe extern "C" fn room_client_try_recv_message(room_client: *mut RoomClient) -> HandleOwned<DebugResult<Option<ServerMessage>>> {
    let room_client = &mut *room_client;
    HandleOwned::new(match room_client.try_read() {
        Ok(Some(ServerMessage::Error(e))) => Err(DebugError(e)),
        Ok(opt_msg) => Ok(opt_msg),
        Err(e) => Err(DebugError::from(e)),
    })
}

/// # Safety
///
/// `opt_msg_res` must point at a valid `DebugResult<Option<ServerMessage>>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn opt_message_result_free(opt_msg_res: HandleOwned<DebugResult<Option<ServerMessage>>>) {
    let _ = opt_msg_res.into_box();
}

/// # Safety
///
/// `opt_msg_res` must point at a valid `DebugResult<Option<ServerMessage>>`.
#[no_mangle] pub unsafe extern "C" fn opt_message_result_is_ok_some(opt_msg_res: *const DebugResult<Option<ServerMessage>>) -> bool {
    (&*opt_msg_res).as_ref().map_or(false, |opt_msg| opt_msg.is_some())
}

/// # Safety
///
/// `opt_msg_res` must point at a valid `DebugResult<Option<ServerMessage>>>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn opt_message_result_unwrap_unwrap(room_client_res: HandleOwned<DebugResult<Option<ServerMessage>>>) -> HandleOwned<ServerMessage> {
    HandleOwned::new(room_client_res.into_box().unwrap().unwrap())
}

/// # Safety
///
/// `msg` must point at a valid `ServerMessage`. This function takes ownership of the `ServerMessage`.
#[no_mangle] pub unsafe extern "C" fn message_free(msg: HandleOwned<ServerMessage>) {
    let _ = msg.into_box();
}

/// # Safety
///
/// `opt_msg_res` must point at a valid `DebugResult<Option<ServerMessage>>`.
#[no_mangle] pub unsafe extern "C" fn opt_message_result_is_err(opt_msg_res: *const DebugResult<Option<ServerMessage>>) -> bool {
    matches!(&*opt_msg_res, Ok(Some(ServerMessage::Error(_))) | Err(_))
}

/// # Safety
///
/// `opt_msg_res` must point at a valid `DebugResult<Option<ServerMessage>>>`. This function takes ownership of the `DebugResult`.
#[no_mangle] pub unsafe extern "C" fn opt_message_result_debug_err(opt_msg_res: HandleOwned<DebugResult<Option<ServerMessage>>>) -> StringHandle {
    StringHandle::from_string(match *opt_msg_res.into_box() {
        Ok(Some(ServerMessage::Error(e))) => e,
        Ok(value) => panic!("tried to debug_err an Ok({:?})", value),
        Err(e) => e.0,
    })
}

/// # Safety
///
/// `msg` must point at a valid `ServerMessage`.
#[no_mangle] pub unsafe extern "C" fn message_effect_type(msg: *const ServerMessage) -> u8 {
    let msg = &*msg;
    match msg {
        ServerMessage::Error(_) => unreachable!(),
        ServerMessage::EnterRoom { .. } |
        ServerMessage::PlayerId(_) |
        ServerMessage::ResetPlayerId(_) |
        ServerMessage::ClientConnected |
        ServerMessage::PlayerDisconnected(_) |
        ServerMessage::UnregisteredClientDisconnected |
        ServerMessage::ItemQueue(_) |
        ServerMessage::GetItem(_) => 0, // changes room state
        ServerMessage::PlayerName(_, _) => 1, // sets a player name and changes room state
    }
}

/// # Safety
///
/// `msg` must point at a valid `ServerMessage`.
///
/// # Panics
///
/// If the `ServerMessage` variant doesn't contain a world ID.
#[no_mangle] pub unsafe extern "C" fn message_player_id(msg: *const ServerMessage) -> u8 {
    let msg = &*msg;
    match msg {
        ServerMessage::PlayerId(world) |
        ServerMessage::ResetPlayerId(world) |
        ServerMessage::PlayerDisconnected(world) |
        ServerMessage::PlayerName(world, _) => world.get(),
        ServerMessage::Error(_) |
        ServerMessage::EnterRoom { .. } |
        ServerMessage::ClientConnected |
        ServerMessage::UnregisteredClientDisconnected |
        ServerMessage::ItemQueue(_) |
        ServerMessage::GetItem(_) => panic!("this message variant has no world ID"),
    }
}

/// # Safety
///
/// `msg` must point at a valid `ServerMessage`.
///
/// # Panics
///
/// If the `ServerMessage` variant doesn't contain a player filename.
#[no_mangle] pub unsafe extern "C" fn message_player_name(msg: *const ServerMessage) -> *const u8 {
    let msg = &*msg;
    if let ServerMessage::PlayerName(_, name) = msg {
        &name[0]
    } else {
        panic!("this message variant has no player filename")
    }
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`, and `msg` must point at a valid `ServerMessage`. This function takes ownership of the `ServerMessage`.
#[no_mangle] pub unsafe extern "C" fn room_client_apply_message(room_client: *mut RoomClient, msg: HandleOwned<ServerMessage>) {
    let room_client = &mut *room_client;
    match *msg.into_box() {
        ServerMessage::Error(_) => unreachable!(),
        ServerMessage::EnterRoom { players, num_unassigned_clients } => {
            room_client.players = players;
            room_client.num_unassigned_clients = num_unassigned_clients;
        }
        ServerMessage::PlayerId(world) => if let Err(idx) = room_client.players.binary_search_by_key(&world, |p| p.world) {
            room_client.players.insert(idx, Player::new(world));
            room_client.num_unassigned_clients -= 1;
        },
        ServerMessage::ResetPlayerId(world) => if let Ok(idx) = room_client.players.binary_search_by_key(&world, |p| p.world) {
            room_client.players.remove(idx);
            room_client.num_unassigned_clients += 1;
        },
        ServerMessage::ClientConnected => room_client.num_unassigned_clients += 1,
        ServerMessage::PlayerDisconnected(world) => if let Ok(idx) = room_client.players.binary_search_by_key(&world, |p| p.world) {
            room_client.players.remove(idx);
        },
        ServerMessage::UnregisteredClientDisconnected => room_client.num_unassigned_clients -= 1,
        ServerMessage::PlayerName(world, name) => if let Ok(idx) = room_client.players.binary_search_by_key(&world, |p| p.world) {
            room_client.players[idx].name = name;
        },
        ServerMessage::ItemQueue(queue) => room_client.item_queue = queue,
        ServerMessage::GetItem(item) => room_client.item_queue.push(item),
    }
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
#[no_mangle] pub unsafe extern "C" fn room_client_send_item(room_client: *mut RoomClient, key: u32, kind: u16, target_world: u8) -> HandleOwned<DebugResult<()>> {
    let room_client = &mut *room_client;
    let target_world = NonZeroU8::new(target_world).expect("tried to send an item to world 0");
    HandleOwned::new(room_client.write(&RoomClientMessage::SendItem { key, kind, target_world }).map_err(DebugError::from))
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
#[no_mangle] pub unsafe extern "C" fn room_client_item_queue_len(room_client: *const RoomClient) -> u16 {
    let room_client = &*room_client;
    room_client.item_queue.len() as u16
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
///
/// # Panics
///
/// If `index` is out of range.
#[no_mangle] pub unsafe extern "C" fn room_client_item_kind_at_index(room_client: *const RoomClient, index: u16) -> u16 {
    let room_client = &*room_client;
    room_client.item_queue[usize::from(index)]
}

/// # Safety
///
/// `room_client` must point at a valid `RoomClient`.
///
/// # Panics
///
/// If `world` is `0`.
#[no_mangle] pub unsafe extern "C" fn room_client_get_player_name(room_client: *const RoomClient, world: u8) -> *const u8 {
    let room_client = &*room_client;
    let world = NonZeroU8::new(world).expect("tried to get player name for world 0");
    if let Some(player) = room_client.players.iter().find(|p| p.world == world) {
        &player.name[0]
    } else {
        &Player::DEFAULT_NAME[0]
    }
}
