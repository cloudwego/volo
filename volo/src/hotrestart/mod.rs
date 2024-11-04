use std::{
    collections::HashMap,
    error::Error,
    fmt::Display,
    io::{IoSlice, IoSliceMut},
    os::fd::{AsRawFd, RawFd},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, LazyLock, Mutex as StdMutex, OnceLock,
    },
    time::Duration,
};

use nix::{
    cmsg_space,
    sys::{
        signal,
        socket::{
            recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags, RecvMsg, UnixAddr,
        },
    },
    unistd::getpid,
};
use tokio::{
    io::{self, Interest},
    net::UnixDatagram,
    sync::Mutex,
};

const HOT_RESTART_PARENT_ADDR: &str = "volo_hot_restart_parent.sock";
const HOT_RESTART_CHILD_ADDR: &str = "volo_hot_restart_child.sock";

pub static DEFAULT_HOT_RESTART: LazyLock<HotRestart> = LazyLock::new(HotRestart::new);

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
enum HotRestartState {
    Uninitalized = 0,
    ParentInitialized = 1,
    ChildInitialized = 2,
}

#[derive(Debug)]
pub struct HotRestartError {
    pub message: String,
}

impl Display for HotRestartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "hot_restart_err_msg: {}", self.message)
    }
}

impl Error for HotRestartError {}

#[repr(u8)]
enum HotRestartMsgType {
    PassFdRequest = 1,
    PassFdResponse = 2,
    TerminateParentRequest = 3,
}

impl From<HotRestartMsgType> for u8 {
    fn from(value: HotRestartMsgType) -> Self {
        value as u8
    }
}

impl TryFrom<u8> for HotRestartMsgType {
    type Error = HotRestartError;

    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(HotRestartMsgType::PassFdRequest),
            2 => Ok(HotRestartMsgType::PassFdResponse),
            3 => Ok(HotRestartMsgType::TerminateParentRequest),
            _ => Err(HotRestartError {
                message: String::from("unknown msg_type"),
            }),
        }
    }
}

// simple self message
enum HotRestartMessage {
    TerminateParentRequest,
    PassFdRequest(String),
    PassFdResponse(RawFd),
}

pub struct HotRestart {
    state: Arc<Mutex<HotRestartState>>,
    listener_fds: Arc<StdMutex<HashMap<String, RawFd>>>,
    dup_listener_num: AtomicI32,
    listener_num: AtomicI32,
    parent_sock_path: OnceLock<PathBuf>,
    child_sock_path: OnceLock<PathBuf>,
    domain_sock: Arc<Mutex<Option<UnixDatagram>>>,
}

impl Default for HotRestart {
    fn default() -> Self {
        Self::new()
    }
}

impl HotRestart {
    pub fn new() -> Self {
        HotRestart {
            state: Arc::new(Mutex::new(HotRestartState::Uninitalized)),
            listener_fds: Arc::new(StdMutex::new(HashMap::new())),
            listener_num: AtomicI32::new(0),
            dup_listener_num: AtomicI32::new(0),
            parent_sock_path: OnceLock::new(),
            child_sock_path: OnceLock::new(),
            domain_sock: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn initialize(
        &self,
        sock_dir_path: &Path,
        server_listener_num: i32,
    ) -> io::Result<()> {
        let mut state = self.state.lock().await;
        if *state != HotRestartState::Uninitalized {
            return Ok(());
        }
        if !sock_dir_path.exists() {
            std::fs::create_dir_all(sock_dir_path)?;
        }
        self.listener_num
            .store(server_listener_num, Ordering::Relaxed);
        self.parent_sock_path
            .set(sock_dir_path.join(HOT_RESTART_PARENT_ADDR))
            .unwrap();
        self.child_sock_path
            .set(sock_dir_path.join(HOT_RESTART_CHILD_ADDR))
            .unwrap();
        if let Some(child_path) = self.child_sock_path.get() {
            if child_path.exists() {
                std::fs::remove_file(child_path.as_path()).unwrap();
            }
            if let Ok(child_sock) = UnixDatagram::bind(child_path.as_path()) {
                if let Ok(()) = child_sock.connect(self.parent_sock_path.get().unwrap().as_path()) {
                    // now child
                    tracing::info!(
                        "hot_restart child initialize, sock_dir_path: {:?}, server_listener_num: \
                         {}",
                        sock_dir_path,
                        server_listener_num
                    );
                    *state = HotRestartState::ChildInitialized;
                    let mut domain_sock = self.domain_sock.lock().await;
                    *domain_sock = Some(child_sock);
                    return Ok(());
                }
            }
        }

        // now parent
        tracing::info!(
            "hot_restart parent initialize, sock_dir_path: {:?}, server_listener_num: {}",
            sock_dir_path,
            server_listener_num
        );
        *state = HotRestartState::ParentInitialized;
        if let Some(path) = self.parent_sock_path.get() {
            if path.exists() {
                std::fs::remove_file(path.as_path()).unwrap();
            }
        }

        let domain_sock = UnixDatagram::bind(self.parent_sock_path.get().unwrap().as_path())?;
        let fds = self.listener_fds.clone();
        tokio::spawn(Self::parent_handle(
            domain_sock,
            self.child_sock_path.get().unwrap().clone(),
            fds,
        ));

        Ok(())
    }

    async fn parent_handle(
        parent_sock: UnixDatagram,
        child_sock_path: PathBuf,
        fds: Arc<StdMutex<HashMap<String, RawFd>>>,
    ) -> io::Result<()> {
        tracing::info!("hot_restart parent_handle");
        loop {
            parent_sock.readable().await?;
            match Self::recv_msg(&parent_sock) {
                Ok(HotRestartMessage::PassFdRequest(addr)) => {
                    if let Some(fd) = fds.lock().unwrap().get(&addr) {
                        tracing::info!("hot_restart parent passfd: {}, addr: {}", fd, addr);
                        Self::send_msg(
                            &parent_sock,
                            child_sock_path.as_path(),
                            HotRestartMsgType::PassFdResponse,
                            HotRestartMessage::PassFdResponse(*fd),
                        )?;
                    }
                }
                Ok(HotRestartMessage::TerminateParentRequest) => {
                    tracing::info!("hot_restart parent terminate");
                    parent_sock.shutdown(std::net::Shutdown::Both)?;
                    signal::kill(getpid(), signal::SIGTERM).unwrap();
                    break;
                }
                Ok(_) => {
                    // ignore unknown msg
                    continue;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    fn recv_msg(rx: &UnixDatagram) -> io::Result<HotRestartMessage> {
        let mut msg = vec![0; 1024];
        let mut iov = [IoSliceMut::new(&mut msg)];
        let mut cmsg_buffer = cmsg_space!([RawFd; 1]);
        let recv_msg: std::io::Result<RecvMsg<UnixAddr>> = rx.try_io(Interest::READABLE, || {
            recvmsg(
                rx.as_raw_fd(),
                &mut iov,
                Some(&mut cmsg_buffer),
                MsgFlags::empty(),
            )
            .map_err(Into::into)
        });

        match recv_msg {
            Ok(recv_msg) => {
                // 1 byte type + (4 bytes length + payload(...))*
                let msg = recv_msg.iovs().nth(0).unwrap();
                match HotRestartMsgType::try_from(msg[0]) {
                    Ok(msg_type) => match msg_type {
                        HotRestartMsgType::PassFdRequest => {
                            let length =
                                u32::from_ne_bytes((&msg[1..5]).try_into().expect("unreachable"))
                                    as usize;
                            let addr = unsafe {
                                String::from_utf8_unchecked(msg[5..(5 + length)].to_vec())
                            };
                            Ok(HotRestartMessage::PassFdRequest(addr))
                        }
                        HotRestartMsgType::PassFdResponse => {
                            let mut raw_fd = None;
                            for c in recv_msg.cmsgs()? {
                                if let ControlMessageOwned::ScmRights(mut fds) = c {
                                    raw_fd = fds.pop();
                                    break;
                                }
                            }
                            if let Some(fd) = raw_fd {
                                Ok(HotRestartMessage::PassFdResponse(fd))
                            } else {
                                Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "PassFdResponse without fd",
                                ))
                            }
                        }
                        HotRestartMsgType::TerminateParentRequest => {
                            Ok(HotRestartMessage::TerminateParentRequest)
                        }
                    },
                    Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e.message)),
                }
            }
            Err(e) => Err(e),
        }
    }

    fn send_msg(
        sock: &UnixDatagram,
        path: &Path,
        msg_type: HotRestartMsgType,
        body: HotRestartMessage,
    ) -> io::Result<()> {
        let peer_addr: UnixAddr = UnixAddr::new(path).unwrap();
        let mut sbuf = Vec::with_capacity(128);
        let mut cmsg: Vec<ControlMessage> = Vec::new();
        let mut fds = Vec::new();
        match msg_type {
            HotRestartMsgType::PassFdRequest => {
                sbuf.push(msg_type as u8);
                if let HotRestartMessage::PassFdRequest(addr) = body {
                    sbuf.extend((addr.len() as u32).to_ne_bytes());
                    sbuf.extend(addr.as_bytes());
                } else {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid data"));
                }
            }
            HotRestartMsgType::PassFdResponse => {
                sbuf.push(msg_type as u8);
                if let HotRestartMessage::PassFdResponse(fd) = body {
                    fds.push(fd);
                    cmsg.push(ControlMessage::ScmRights(&fds));
                } else {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid data"));
                }
            }
            HotRestartMsgType::TerminateParentRequest => {
                sbuf.push(msg_type as u8);
            }
        }
        sendmsg::<UnixAddr>(
            sock.as_raw_fd(),
            &[IoSlice::new(&sbuf)],
            &cmsg,
            MsgFlags::empty(),
            Some(&peer_addr),
        )?;

        Ok(())
    }

    pub fn register_listener_fd(&self, addr: String, raw_fd: RawFd) {
        tracing::info!("hot_restart register_listener_fd: {}, {}", addr, raw_fd);
        let mut listener_fds = self.listener_fds.lock().unwrap();
        listener_fds.insert(addr, raw_fd);
    }

    pub async fn dup_parent_listener_sock(&self, addr: String) -> io::Result<Option<RawFd>> {
        let mut state = self.state.lock().await;
        if *state != HotRestartState::ChildInitialized {
            tracing::info!(
                "hot_restart skip dup_parent_listener_sock: {}, as parent",
                addr
            );
            return Ok(None);
        }
        tracing::info!("hot_restart dup_parent_listener_sock: {}, as child", addr);
        // todo: retry?
        let child_guard = self.domain_sock.lock().await;
        let child_sock = child_guard.as_ref().unwrap();
        Self::send_msg(
            child_sock,
            self.parent_sock_path.get().unwrap().as_path(),
            HotRestartMsgType::PassFdRequest,
            HotRestartMessage::PassFdRequest(addr),
        )?;

        child_sock.readable().await?;

        match Self::recv_msg(child_sock) {
            Ok(HotRestartMessage::PassFdResponse(fd)) => {
                self.dup_listener_num.fetch_add(1, Ordering::AcqRel);
                tracing::info!("hot_restart dup_parent_listener_sock fd: {:?}", fd);
                if self.dup_listener_num.load(Ordering::Relaxed)
                    == self.listener_num.load(Ordering::Relaxed)
                {
                    tracing::info!("hot_restart send terminate_parent");
                    Self::send_msg(
                        child_sock,
                        self.parent_sock_path.get().unwrap().as_path(),
                        HotRestartMsgType::TerminateParentRequest,
                        HotRestartMessage::TerminateParentRequest,
                    )?;
                    // child -> parent
                    *state = HotRestartState::ParentInitialized;
                    child_sock.shutdown(std::net::Shutdown::Both)?;
                    drop(child_guard);
                    {
                        // reset domain_sock
                        let mut child_sock = self.domain_sock.lock().await;
                        *child_sock = None;
                    }
                    if let Some(path) = self.parent_sock_path.get() {
                        if path.exists() {
                            std::fs::remove_file(path.as_path()).unwrap();
                        }
                    }

                    let parent_sock_buf = self.parent_sock_path.get().unwrap().clone();
                    let child_sock_buf = self.child_sock_path.get().unwrap().clone();
                    let fds = self.listener_fds.clone();
                    tokio::spawn(async move {
                        let mut interval = tokio::time::interval(Duration::from_millis(5));

                        loop {
                            interval.tick().await;
                            let Ok(domain_sock) = UnixDatagram::bind(parent_sock_buf.as_path())
                            else {
                                continue;
                            };
                            tracing::info!("hot_restart child->parent");
                            Self::parent_handle(domain_sock, child_sock_buf.clone(), fds.clone())
                                .await?;
                            break;
                        }
                        Ok::<(), io::Error>(())
                    });
                }
                Ok(Some(fd))
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Not PassFdResponse",
            )),
            Err(e) => Err(e),
        }
    }
}
