Source provenance
=================

The DRDA/DDM client, SQL implementation, original protocol tests, and
CCSID 37 conversion table were extracted from ykwyuta/Zeus,
commit bc18da0cf2c3e429d4458062c96f959a87b2f376:

- crates/zeus-drda/src/{client,ddm,sql}.rs
- crates/zeus-drda/tests/db2.rs
- crates/zeus-cobol/src/ebcdic.rs

Upstream: https://github.com/ykwyuta/Zeus

This standalone distribution is licensed under the MIT License; see LICENSE.
The standalone adaptation removes all Zeus workspace and runtime dependencies,
uses fixed CCSID 37 for DDM control strings, and provides independent packaging,
documentation, examples, configuration, and tests.
