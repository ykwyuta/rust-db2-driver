Source provenance
=================

The DRDA/DDM client, SQL implementation, original protocol tests, and
CCSID 37 conversion table were extracted from ykwyuta/Zeus,
commit bc18da0cf2c3e429d4458062c96f959a87b2f376:

- crates/zeus-drda/src/{client,ddm,sql}.rs
- crates/zeus-drda/tests/db2.rs
- crates/zeus-cobol/src/ebcdic.rs

Upstream: https://github.com/ykwyuta/Zeus

The upstream GPL-3.0-or-later license is retained; see LICENSE.
The standalone adaptation removes all Zeus workspace and runtime dependencies,
uses fixed CCSID 37 for DDM control strings, and provides independent packaging,
documentation, examples, configuration, and tests.
