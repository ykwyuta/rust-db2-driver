use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rust_db2_driver::{
    Connection, Params, Value,
    ddm::{self, Dss, Object, cp},
};

trait Io: Read + Write {}
impl<T: Read + Write> Io for T {}

fn chain(io: &mut impl Read) -> Vec<Dss> {
    let mut result = Vec::new();
    loop {
        let d = ddm::read_dss(io).unwrap();
        let more = d.chained();
        result.push(d);
        if !more {
            return result;
        }
    }
}

fn reply(io: &mut impl Write, corr: u16, obj: Object) {
    io.write_all(&ddm::dss(ddm::RPYDSS, corr, &obj)).unwrap();
    io.flush().unwrap();
}

fn handshake(io: &mut Box<dyn Io>) {
    let r = chain(io);
    assert_eq!(r[0].object.cp, cp::EXCSAT);
    assert_eq!(r[1].object.cp, cp::ACCSEC);
    reply(io, r[1].correlator, Object::new(cp::ACCSECRD));
    let r = chain(io);
    assert_eq!(r[0].object.cp, cp::SECCHK);
    assert_eq!(r[1].object.cp, cp::ACCRDB);
    reply(io, r[1].correlator, Object::new(cp::ACCRDBRM));
}

fn listener() -> (TcpListener, Params) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let params = Params::from_url(&format!(
        "db2://user:secret@127.0.0.1:{}/TEST",
        listener.local_addr().unwrap().port()
    ))
    .unwrap();
    (listener, params)
}

fn accept(listener: TcpListener) -> TcpStream {
    let s = listener.accept().unwrap().0;
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    s
}

fn blocked_server(mut io: Box<dyn Io>, ready: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
    handshake(&mut io);
    let request = chain(&mut io);
    assert_eq!(request[0].object.cp, cp::EXCSQLIMM);
    io.write_all(&[0, 20, 0xd0]).unwrap(); // Incomplete DSS header.
    io.flush().unwrap();
    ready.send(()).unwrap();
    // Wait for cancellation to close the transport, without a fixed sleep.
    let mut b = [0];
    match io.read(&mut b) {
        Ok(0) => (),
        Err(e) => assert_ne!(e.kind(), std::io::ErrorKind::TimedOut),
        other => panic!("expected transport shutdown: {other:?}"),
    }
    // Keep the peer open until the client returns, so the test also catches
    // Windows reads that are only released when the server closes its socket.
    release.recv_timeout(Duration::from_secs(5)).unwrap();
}

fn cancel_blocked(mut c: Connection, ready: mpsc::Receiver<()>, release: mpsc::Sender<()>) {
    let cancel = c.cancel_handle();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let err = c.execute("VALUES 1").unwrap_err();
        assert!(err.is_cancelled(), "{err}");
        assert!(c.rollback().unwrap_err().is_cancelled());
        done_tx.send(()).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    cancel.cancel().unwrap();
    cancel.cancel().unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("cancellation did not unblock I/O");
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[test]
fn cancels_blocked_tcp_and_prevents_reuse() {
    let (listener, p) = listener();
    let (tx, rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || blocked_server(Box::new(accept(listener)), tx, release_rx));
    cancel_blocked(Connection::connect(&p).unwrap(), rx, release_tx);
    server.join().unwrap();
}

#[test]
fn fragmented_reply_survives_socket_poll_timeouts() {
    let (listener, p) = listener();
    let server = thread::spawn(move || {
        let mut io: Box<dyn Io> = Box::new(accept(listener));
        handshake(&mut io);
        let r = chain(&mut io);
        let response = ddm::dss(
            ddm::RPYDSS,
            r[0].correlator,
            &Object {
                cp: cp::SQLCARD,
                data: vec![0xff],
            },
        );
        io.write_all(&response[..3]).unwrap();
        io.flush().unwrap();
        // Intentionally exceed the client's 100 ms polling interval. Partial
        // DSS data must survive the timeout rather than restart its decoding.
        thread::sleep(Duration::from_millis(300));
        io.write_all(&response[3..]).unwrap();
        io.flush().unwrap();
    });
    let mut c = Connection::connect(&p).unwrap();
    assert_eq!(c.execute("VALUES 1").unwrap().sqlcode, 0);
    server.join().unwrap();
}

#[test]
fn sends_bound_values_separately_and_preserves_prepare_errors() {
    let (listener, p) = listener();
    let server = thread::spawn(move || {
        let mut io: Box<dyn Io> = Box::new(accept(listener));
        handshake(&mut io);
        for command in [cp::EXCSQLSTT, cp::OPNQRY] {
            let r = chain(&mut io);
            assert_eq!(r[0].object.cp, cp::PRPSQLSTT);
            assert_eq!(r[1].object.cp, cp::SQLSTT);
            assert!(!String::from_utf8_lossy(&r[1].object.data).contains("untrusted"));
            reply(
                &mut io,
                r[0].correlator,
                Object {
                    cp: cp::SQLCARD,
                    data: vec![0xff],
                },
            );
            let r = chain(&mut io);
            assert_eq!(r[0].object.cp, command);
            assert_eq!(r[1].object.cp, cp::SQLDTA);
            let values = r[1].object.params();
            assert_eq!(values[0].cp, cp::FDODSC);
            assert_eq!(values[1].cp, cp::FDODTA);
            assert!(values[1].data.windows(9).any(|b| b == b"untrusted"));
            reply(
                &mut io,
                r[0].correlator,
                Object {
                    cp: cp::SQLCARD,
                    data: vec![0xff],
                },
            );
        }
        let r = chain(&mut io);
        let mut error = vec![0];
        error.extend((-104i32).to_be_bytes());
        error.extend(b"42601TESTPROC");
        error.push(0xff);
        reply(
            &mut io,
            r[0].correlator,
            Object {
                cp: cp::SQLCARD,
                data: error,
            },
        );
        let mut b = [0];
        assert_eq!(io.read(&mut b).unwrap(), 0); // No execute after prepare error.
    });
    let mut c = Connection::connect(&p).unwrap();
    let values = [Value::Str("untrusted' ? --".into())];
    assert_eq!(
        c.execute_params("INSERT INTO T VALUES (?)", &values)
            .unwrap()
            .sqlcode,
        0
    );
    assert_eq!(
        c.query_params("VALUES CAST(? AS VARCHAR(40))", &values)
            .unwrap()
            .sqlca
            .sqlcode,
        0
    );
    assert_eq!(
        c.execute_params("INVALID ?", &values).unwrap().sqlcode,
        -104
    );
    drop(c);
    server.join().unwrap();
}

#[test]
fn retained_handle_does_not_keep_connection_alive() {
    let (listener, p) = listener();
    let server = thread::spawn(move || {
        let mut io: Box<dyn Io> = Box::new(accept(listener));
        handshake(&mut io);
        let mut b = [0];
        assert_eq!(io.read(&mut b).unwrap(), 0);
    });
    let c = Connection::connect(&p).unwrap();
    let handle = c.cancel_handle();
    drop(c);
    server.join().unwrap();
    drop(handle);
}

#[cfg(feature = "tls")]
mod tls {
    use super::*;
    use rust_db2_driver::rustls::{
        self, ClientConfig, ServerConfig, pki_types::PrivatePkcs8KeyDer,
    };
    use std::sync::Arc;

    fn configs(name: &str) -> (Arc<ServerConfig>, Arc<ClientConfig>) {
        let cert = rcgen::generate_simple_self_signed(vec![name.into()]).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server = ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
            )
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.cert.der().clone()).unwrap();
        let client = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        (Arc::new(server), Arc::new(client))
    }

    #[test]
    fn verifies_private_ca_and_cancels_blocked_tls() {
        let (server_config, client_config) = configs("127.0.0.1");
        let (listener, p) = listener();
        let (tx, rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let io = rustls::StreamOwned::new(
                rustls::ServerConnection::new(server_config).unwrap(),
                accept(listener),
            );
            blocked_server(Box::new(io), tx, release_rx);
        });
        cancel_blocked(
            Connection::connect_with_tls(&p, client_config).unwrap(),
            rx,
            release_tx,
        );
        server.join().unwrap();
    }

    #[test]
    fn rejects_wrong_hostname_and_untrusted_cert_before_drda() {
        for wrong_name in [false, true] {
            let (server_config, client_config) = configs(if wrong_name {
                "wrong.invalid"
            } else {
                "127.0.0.1"
            });
            let (listener, p) = listener();
            let server = thread::spawn(move || {
                let mut io = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server_config).unwrap(),
                    accept(listener),
                );
                let mut b = [0];
                assert!(io.read(&mut b).is_err()); // TLS alert, no DRDA credentials.
            });
            let result = if wrong_name {
                Connection::connect_with_tls(&p, client_config)
            } else {
                Connection::connect_tls(&p)
            };
            assert!(result.is_err());
            server.join().unwrap();
        }
    }
}
