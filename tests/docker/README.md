# Disposable Db2 integration environment

Requires Docker with enough memory for Db2. These commands affect only the
`rust-db2-driver-validation` Compose project. The fixture password is public and
for local tests only. The ports bind to loopback.

```powershell
docker compose -f tests/docker/compose.yml up -d
docker compose -f tests/docker/compose.yml logs -f db2
```

Wait for `Setup has completed`, then confirm database readiness:

```powershell
docker compose -f tests/docker/compose.yml exec -T db2 su - db2inst1 -c 'db2 connect to DRIVERDB'
```

Enable native Db2 TLS (this restarts only this disposable instance):

```powershell
docker compose -f tests/docker/compose.yml cp tests/docker/enable-tls.sh db2:/tmp/enable-tls.sh
docker compose -f tests/docker/compose.yml exec -T db2 bash /tmp/enable-tls.sh
docker compose -f tests/docker/compose.yml cp db2:/database/driver-tls/server.pem tests/docker/server.pem
```

The generated self-signed certificate has `localhost` and `127.0.0.1` SANs.
It uses a 2048-bit RSA key and SHA-256. Only the public certificate is copied
to the host. The private key/keystore stay inside the container.
The setup script installs `libicu` inside this disposable container because
the stock image omits the ICU runtime used by GSKit's certificate tool.

Run the real-server suite once over TCP and once over TLS:

```powershell
$env:DB2_URL = 'db2://db2inst1:DriverTest_2026!@127.0.0.1:35000/DRIVERDB'
Remove-Item Env:DB2_TLS_CA -ErrorAction SilentlyContinue
cargo test --test db2 -- --ignored --nocapture
$env:DB2_URL = 'db2://db2inst1:DriverTest_2026!@127.0.0.1:35001/DRIVERDB'
$env:DB2_TLS_CA = (Resolve-Path tests/docker/server.pem).Path
cargo test --test db2 -- --ignored --nocapture
```

Optionally set `DB2_TLS_VERSION` to `1.2` or `1.3` to restrict the test client
to that protocol version. Leave it unset to use rustls defaults.

The suite creates and removes its own tables. Tests failing mid-run may leave
test tables; remove the disposable container and volumes after validation:

```powershell
docker compose -f tests/docker/compose.yml down -v
Remove-Item -LiteralPath tests/docker/server.pem -ErrorAction SilentlyContinue
Remove-Item Env:DB2_URL, Env:DB2_TLS_CA -ErrorAction SilentlyContinue
```

Server setup follows IBM's instructions for
[creating a keystore](https://www.ibm.com/docs/en/db2/12.1.x?topic=certificate-creating-keystore-global-security-kit-gskit),
[certificate SANs](https://www.ibm.com/docs/en/db2/12.1.x?topic=instances-representing-servers-in-certificate), and
[enabling TLS](https://www.ibm.com/docs/en/db2/12.1.x?topic=ctcsc-configure-tls-db2-server).

## Verification on 2026-10-26

Windows client with Db2 LUW Community Edition 12.1.5.0 in this fixture:

- All three integration tests passed over TCP and native TLS.
- TLS 1.2 and TLS 1.3 were each verified with the client's version restricted.
- The runtime-dependency-free build (`--no-default-features`) passed the same
  integration suite over TCP.
- Bound values included Unicode and SQL punctuation, NULL, empty text/binary,
  BIGINT limits, DECIMAL(31,8), DOUBLE, and 85 input parameters.
- The cancellation test confirmed the worker's lock wait through Db2's
  monitoring function, then required the client to return a cancellation error
  within two seconds, before releasing the blocking transaction.

This verification also found and fixed CCSID 500 punctuation in authentication,
continuation descriptors for results with more than 84 columns, and a Windows
TLS read that did not immediately unblock on socket shutdown alone.
