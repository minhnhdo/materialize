WIP fuzzing. Not super useful yet.

Before first use:

``` sh
cd materialize
cargo install cargo-fuzz
cp -ar sqllogictest/test/ fuzz/corpus/fuzz_sqllogictest/
```

To fuzz stuff:

``` sh
cd materialize

cargo +nightly fuzz run fuzz_sqllogictest -- -workers=4
# or
cargo +nightly fuzz run fuzz_testdriver
```

Failing tests are added to `./fuzz/artifacts` and can be rerun with `cargo test -p sqllogictest`.