#!/bin/bash
# Run as root inside the disposable validation container after Db2 is ready.
set -euo pipefail
# The stock 12.1.5 image omits the ICU runtime required by gsk9certutil.
rpm -q libicu >/dev/null 2>&1 || yum install -y libicu
install -d -m 700 -o db2inst1 -g db2iadm1 /database/driver-tls
su - db2inst1 <<'DB2'
set -eu
export LD_LIBRARY_PATH=/opt/ibm/db2/V12.1/lib64/gskit_db2:/opt/ibm/db2/V12.1/lib64:${LD_LIBRARY_PATH:-}
gsk=/opt/ibm/db2/V12.1/gskit/bin/gsk9certutil_64
cd /database/driver-tls
if [ ! -f server.p12 ]; then
  "$gsk" -keydb -create -db server.p12 -pw DriverKeystore_2026 -stash
  "$gsk" -cert -create -db server.p12 -stashed -label driver-test -dn 'CN=localhost' -san_dnsname localhost -san_ipaddr 127.0.0.1 -size 2048 -sigalg SHA256_WITH_RSA -expire 365
  "$gsk" -cert -extract -db server.p12 -stashed -label driver-test -target server.pem -format ascii
fi
db2 update dbm cfg using SSL_SVR_KEYDB /database/driver-tls/server.p12 SSL_SVR_STASH /database/driver-tls/server.sth SSL_SVR_LABEL driver-test SSL_SVCENAME 50001 SSL_VERSIONS TLSV12,TLSV13 AUTHENTICATION SERVER
db2set DB2COMM=TCPIP,SSL
db2stop force
db2start
DB2
