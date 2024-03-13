# SPARQL Transactional Test
A tool for testing the correctness of a triplestore's transaction implementation.

⚠️ This tool is in prototype stage. ⚠️

## Stresstest
The `stress` subcommand of this tool simply stresses
a triplestore with a given number of read workers.

### Example
```shell
# start up triplestore here

# stress given triplestore with 32 readers using the queries from queries.txt for 30 seconds
cargo run --release -- stress -t 30 -r 32 -q queries.txt http://localhost:9080/sparql
```


## Verification
The `verify` subcommand uses precalculated, known-correct results to check the
transaction implementation of the triplestore. It does this by stressing the triplestore using
a number of readers while simultaneously performing concurrent updates. After each update the tool verifies that
the update was applied correctly.

To explicitly test durability use `verify [..] durability` with `--kill-script`, `--start-script` and `--restart-script`.
In this mode the test will periodically kill and restart the server to ensure transactional durability.

For best test coverage it is advisable to run both `verify` twice, once with and once without the
previously named options.

To generate the known-correct data, use `generate_update_operations.sh`.
Or use one of the pre-generated, swdf-based sets provided in `rdf_small/` and `rdf_large/`.

### Example
```shell
# start up triplestore here

# 4  write workers using pre-generated data from rdf_large/
# 24 read workers using the queries from queries.txt to stress the triplestore
cargo run --release -- verify -w 4 -Q rdf_large -r 24 -q queries.txt \
    http://localhost:9080/sparql http://localhost:9080/update


# verify durability will start the triplestore itself, it does not need to be running before issuing the following command

# 4  write workers using pre-generated data from rdf_large/
# 24 read workers using the queries from queries.txt to stress the triplestore
# using given scripts to perform server lifecycle management
cargo run --release -- verify -w 4 -Q rdf_large -r 24 -q queries.txt \
    http://localhost:9080/sparql http://localhost:9080/update
    durability --start-script examples/start.sh --kill-script examples/kill.sh --restart-script examples/restart.sh
```
