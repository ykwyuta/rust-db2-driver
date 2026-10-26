//! A DRDA connection to a Db2 database.

use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crate::ebcdic;

use crate::ddm::{self, CHAINED, Dss, OBJDSS, Object, RQSDSS, SAME_CORRELATOR, cp};

/// A DRDA protocol or server error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    /// Whether the connection was explicitly cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.0 == "connection cancelled; reconnect before issuing more commands"
    }

    fn cancelled() -> Self {
        Self("connection cancelled; reconnect before issuing more commands".into())
    }
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error(format!("I/O: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// DDM character parameters are EBCDIC.
pub(crate) fn ebc(s: &str) -> Vec<u8> {
    ebcdic::encode(s)
}

pub(crate) fn ebc_padded(s: &str, n: usize) -> Vec<u8> {
    let mut b = ebc(s);
    b.resize(n, ebcdic::SPACE);
    b
}

pub(crate) fn from_ebc(b: &[u8]) -> String {
    ebcdic::decode_latin1(b)
        .iter()
        .map(|&c| c as char)
        .collect::<String>()
        .trim_end()
        .to_string()
}

trait Transport: Read + Write + Send + Sync {}
impl<T: Read + Write + Send + Sync> Transport for T {}

struct CancelState {
    socket: TcpStream,
    cancelled: AtomicBool,
}

// Winsock shutdown does not reliably release an already pending recv while the
// peer is busy executing SQL. Poll the flag on bounded socket waits, underneath
// both TLS and DSS framing, so partial records stay intact between retries.
struct CancellableSocket {
    socket: TcpStream,
    state: Arc<CancelState>,
}

impl CancellableSocket {
    fn retry<T>(
        &self,
        mut operation: impl FnMut(&TcpStream) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        loop {
            if self.state.cancelled.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    "connection cancelled",
                ));
            }
            match operation(&self.socket) {
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                result => return result,
            }
        }
    }
}

impl Read for CancellableSocket {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.retry(|mut socket| socket.read(buf))
    }
}

impl Write for CancellableSocket {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.retry(|mut socket| socket.write(buf))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.retry(|mut socket| socket.flush())
    }
}

/// A thread-safe handle that aborts this connection, including blocked I/O.
///
/// Cancellation is permanent, even when called while idle. It closes the TCP
/// connection (also for TLS); reconnect afterwards. It does not send a Db2
/// statement-cancel command or guarantee when the server stops execution.
#[derive(Clone)]
pub struct CancelHandle(Arc<CancelState>);

impl CancelHandle {
    pub fn cancel(&self) -> Result<()> {
        if !self.0.cancelled.swap(true, Ordering::SeqCst) {
            match self.0.socket.shutdown(Shutdown::Both) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotConnected => (),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

pub struct Connection {
    inp: BufReader<Box<dyn Transport>>,
    cancel: CancelHandle,
    correlator: u16,
    pub rdbnam: String,
    /// Data representation of the server (TYPDEFNAM of ACCRDBRM).
    pub server_typdef: String,
    /// Server CCSIDs (single-byte, mixed).
    pub server_ccsid: (u16, u16),
    pub trace: bool,
    /// Package section used by the last query.
    pub(crate) section: u16,
}

/// Connection parameters.
#[derive(Clone)]
pub struct Params {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for Params {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Params")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Params {
    fn validate(&self) -> Result<()> {
        if self.host.is_empty() || self.port == 0 {
            return Err(Error("Db2 host and a nonzero port are required".into()));
        }
        for (name, value) in [
            ("database", self.database.as_str()),
            ("user", self.user.as_str()),
            ("password", self.password.as_str()),
        ] {
            if value.is_empty()
                || value.chars().count() > 255
                || value.chars().any(|c| c.is_control() || u32::from(c) > 255)
            {
                return Err(Error(format!(
                    "Db2 {name} must be nonempty Latin-1 text, at most 255 characters, without control characters"
                )));
            }
        }
        Ok(())
    }

    /// `db2://user:password@host:port/DATABASE`
    pub fn from_url(url: &str) -> Result<Params> {
        let bad = || Error("invalid Db2 URL; expected db2://user:password@host:port/DB".into());
        let rest = url.strip_prefix("db2://").ok_or_else(bad)?;
        let (cred, rest) = rest.rsplit_once('@').ok_or_else(bad)?;
        let (user, password) = cred.split_once(':').ok_or_else(bad)?;
        let (hostport, db) = rest.split_once('/').ok_or_else(bad)?;
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().map_err(|_| bad())?),
            None => (hostport, 50000),
        };
        let params = Params {
            host: host.to_string(),
            port,
            database: db.to_string(),
            user: user.to_string(),
            password: password.to_string(),
        };
        params.validate()?;
        Ok(params)
    }
}

impl Connection {
    pub fn connect(p: &Params) -> Result<Connection> {
        p.validate()?;
        let s = TcpStream::connect((p.host.as_str(), p.port))?;
        Self::from_stream(p, s, |s| Ok(Box::new(s)))
    }

    /// Connects with TLS, verifying the server name and a public CA chain.
    /// Use the server's configured TLS port in `Params`.
    #[cfg(feature = "tls")]
    pub fn connect_tls(p: &Params) -> Result<Connection> {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error(format!("TLS: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Self::connect_with_tls(p, Arc::new(config))
    }

    /// Connects using a caller-supplied rustls configuration (e.g. private CAs).
    /// The host in `Params` is also the certificate verification name.
    #[cfg(feature = "tls")]
    pub fn connect_with_tls(p: &Params, config: Arc<rustls::ClientConfig>) -> Result<Connection> {
        p.validate()?;
        let name = rustls::pki_types::ServerName::try_from(p.host.clone())
            .map_err(|_| Error("invalid TLS server name".into()))?;
        let tls =
            rustls::ClientConnection::new(config, name).map_err(|e| Error(format!("TLS: {e}")))?;
        let s = TcpStream::connect((p.host.as_str(), p.port))?;
        Self::from_stream(p, s, |s| {
            let mut stream = rustls::StreamOwned::new(tls, s);
            while stream.conn.is_handshaking() {
                stream.conn.complete_io(&mut stream.sock)?;
            }
            Ok(Box::new(stream))
        })
    }

    fn from_stream(
        p: &Params,
        s: TcpStream,
        wrap: impl FnOnce(CancellableSocket) -> Result<Box<dyn Transport>>,
    ) -> Result<Connection> {
        s.set_nodelay(true)?;
        s.set_read_timeout(Some(Duration::from_millis(100)))?;
        s.set_write_timeout(Some(Duration::from_millis(100)))?;
        let cancel = CancelHandle(Arc::new(CancelState {
            socket: s.try_clone()?,
            cancelled: AtomicBool::new(false),
        }));
        let mut c = Connection {
            inp: BufReader::new(wrap(CancellableSocket {
                socket: s,
                state: cancel.0.clone(),
            })?),
            cancel,
            correlator: 0,
            rdbnam: p.database.to_ascii_uppercase(),
            server_typdef: String::new(),
            server_ccsid: (1208, 1208),
            trace: false,
            section: 1,
        };
        c.handshake(p)?;
        c.trace = std::env::var_os("DB2_DRDA_TRACE").is_some();
        Ok(c)
    }

    /// Obtain this before moving the connection to a worker thread.
    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    pub(crate) fn check_cancelled(&self) -> Result<()> {
        if self.cancel.0.cancelled.load(Ordering::SeqCst) {
            return Err(Error::cancelled());
        }
        Ok(())
    }

    pub(crate) fn rdbnam_param(&self) -> Vec<u8> {
        ebc_padded(&self.rdbnam, 18.max(self.rdbnam.len()))
    }

    fn next_correlator(&mut self) -> u16 {
        self.correlator = self.correlator.wrapping_add(1).max(1);
        self.correlator
    }

    /// Sends a chain of requests: each a command with its command data
    /// objects. Returns the reply DSSs.
    pub(crate) fn exchange(&mut self, chain: &[(Object, Vec<Object>)]) -> Result<Vec<Dss>> {
        self.check_cancelled()?;
        let result = self.exchange_inner(chain);
        self.check_cancelled()?;
        result
    }

    fn exchange_inner(&mut self, chain: &[(Object, Vec<Object>)]) -> Result<Vec<Dss>> {
        let mut buf = Vec::new();
        for (k, (cmd, objs)) in chain.iter().enumerate() {
            // Outbound DSS continuation segments are not implemented yet.
            // Reject oversized objects rather than wrap their length fields.
            if std::iter::once(cmd)
                .chain(objs)
                .any(|o| o.data.len() > 32757)
            {
                return Err(Error("request object exceeds the 32757-byte limit".into()));
            }
            let corr = self.next_correlator();
            let last_cmd = k + 1 == chain.len();
            let mut fmt = RQSDSS;
            if !objs.is_empty() {
                fmt |= CHAINED | SAME_CORRELATOR;
            } else if !last_cmd {
                fmt |= CHAINED;
            }
            buf.extend(ddm::dss(fmt, corr, cmd));
            for (j, o) in objs.iter().enumerate() {
                let mut f = OBJDSS;
                if j + 1 < objs.len() {
                    f |= CHAINED | SAME_CORRELATOR;
                } else if !last_cmd {
                    f |= CHAINED;
                }
                buf.extend(ddm::dss(f, corr, o));
            }
        }
        if self.trace {
            eprintln!("DRDA > {}", hex(&buf));
        }
        self.inp.get_mut().write_all(&buf)?;
        self.inp.get_mut().flush()?;
        let mut replies = Vec::new();
        loop {
            let d = ddm::read_dss(&mut self.inp)?;
            if self.trace {
                eprintln!(
                    "DRDA < fmt {:02X} corr {} cp {:04X} {}",
                    d.format,
                    d.correlator,
                    d.object.cp,
                    hex(&d.object.data)
                );
            }
            let more = d.chained();
            replies.push(d);
            if !more {
                break;
            }
        }
        Ok(replies)
    }

    fn handshake(&mut self, p: &Params) -> Result<()> {
        let correlation = correlation_token()?;
        let mut mgrs = Vec::new();
        for (m, l) in [
            (cp::AGENT, 7u16),
            (cp::SQLAM, 7),
            (cp::RDB, 7),
            (cp::SECMGR, 7),
            (cp::CMNTCPIP, 5),
        ] {
            mgrs.extend(m.to_be_bytes());
            mgrs.extend(l.to_be_bytes());
        }
        let excsat = Object::new(cp::EXCSAT)
            .param(cp::EXTNAM, &ebc("RUSTDB2"))
            .param(cp::SRVNAM, &ebc("RUSTDB2"))
            .param(cp::SRVRLSLV, &ebc("RDB01000"))
            .param(cp::MGRLVLLS, &mgrs)
            .param(cp::SRVCLSNM, &ebc("RUSTDB2"));
        let accsec = Object::new(cp::ACCSEC)
            .param_u16(cp::SECMEC, 3)
            .param(cp::RDBNAM, &self.rdbnam_param());
        let r = self.exchange(&[(excsat, vec![]), (accsec, vec![])])?;
        check_errors(&r)?;
        let secchk = Object::new(cp::SECCHK)
            .param_u16(cp::SECMEC, 3)
            .param(cp::RDBNAM, &self.rdbnam_param())
            .param(cp::USRID, &ebc(&p.user))
            .param(cp::PASSWORD, &ebc(&p.password));
        let ovr = Object::new(cp::TYPDEFOVR)
            .param_u16(cp::CCSIDSBC, 1208)
            .param_u16(cp::CCSIDDBC, 1200)
            .param_u16(cp::CCSIDMBC, 1208);
        let accrdb = Object::new(cp::ACCRDB)
            .param(cp::RDBNAM, &self.rdbnam_param())
            .param_u16(cp::RDBACCCL, cp::SQLAM)
            .param(cp::PRDID, &ebc("RDB01000"))
            .param(cp::TYPDEFNAM, &ebc("QTDSQLASC"))
            .param(cp::TYPDEFOVR, &ovr.data)
            .param(cp::CRRTKN, &correlation);
        let r = self.exchange(&[(secchk, vec![]), (accrdb, vec![])])?;
        check_errors(&r)?;
        for d in &r {
            if d.object.cp == cp::SECCHKRM
                && let Some(code) = d.object.get(cp::SECCHKCD)
                && code.data.first().copied().unwrap_or(0) != 0
            {
                return Err(Error(format!(
                    "security check failed (SECCHKCD {:02X})",
                    code.data[0]
                )));
            }
            if d.object.cp == cp::ACCRDBRM {
                if let Some(t) = d.object.get(cp::TYPDEFNAM) {
                    self.server_typdef = from_ebc(&t.data);
                }
                if let Some(o) = d.object.get(cp::TYPDEFOVR) {
                    if let Some(s) = o.u16_of(cp::CCSIDSBC) {
                        self.server_ccsid.0 = s;
                    }
                    if let Some(m) = o.u16_of(cp::CCSIDMBC) {
                        self.server_ccsid.1 = m;
                    }
                }
            }
        }
        if !r.iter().any(|d| d.object.cp == cp::ACCRDBRM) {
            return Err(Error("no ACCRDBRM in the reply".into()));
        }
        Ok(())
    }
}

// A DRDA correlation token becomes the server's APPLICATION_ID. A fixed token
// aliases unrelated sessions, including their UOW/activity event-monitor keys.
// Use the 19-byte DRDA layout: 8 encoded hex digits, '.', 4 encoded hex digits,
// then 6 binary bytes. OS entropy avoids reuse across threads, processes,
// hosts, reconnects and clock adjustments. Failure is returned before login.
fn correlation_token() -> Result<Vec<u8>> {
    let mut random = [0; 12];
    getrandom::fill(&mut random)
        .map_err(|e| Error(format!("connection identifier entropy: {e}")))?;
    let mut prefix = format!(
        "{:08X}.{:04X}",
        u32::from_be_bytes(random[..4].try_into().unwrap()),
        u16::from_be_bytes(random[4..6].try_into().unwrap())
    )
    .into_bytes();
    // The leading hex digit in each field must be an SNA-compatible letter.
    for index in [0, 9] {
        if prefix[index].is_ascii_digit() {
            prefix[index] += b'G' - b'0';
        }
    }
    let mut token = ebc(std::str::from_utf8(&prefix).unwrap());
    token.extend_from_slice(&random[6..]);
    Ok(token)
}

impl Drop for Connection {
    fn drop(&mut self) {
        // A retained cancellation handle must not keep the session open.
        let _ = self.cancel.0.socket.shutdown(Shutdown::Both);
    }
}

/// Fails on error reply messages (other than SQLERRRM, which the SQLCARD
/// describes).
pub(crate) fn check_errors(r: &[Dss]) -> Result<()> {
    for d in r {
        let name = match d.object.cp {
            cp::SYNTAXRM => "SYNTAXRM (syntax error)",
            cp::PRCCNVRM => "PRCCNVRM (conversation protocol error)",
            cp::CMDCHKRM => "CMDCHKRM (command check)",
            cp::OBJNSPRM => "OBJNSPRM (object not supported)",
            cp::PRMNSPRM => "PRMNSPRM (parameter not supported)",
            cp::VALNSPRM => "VALNSPRM (parameter value not supported)",
            cp::CMDNSPRM => "CMDNSPRM (command not supported)",
            cp::MGRLVLRM => "MGRLVLRM (manager level conflict)",
            cp::RDBNACRM => "RDBNACRM (database not accessed)",
            cp::RDBNFNRM => "RDBNFNRM (database not found)",
            _ => continue,
        };
        let detail = d
            .object
            .params()
            .iter()
            .map(|p| format!("{:04X}={}", p.cp, hex(&p.data)))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(Error(format!("{name}: {detail}")));
    }
    Ok(())
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}
