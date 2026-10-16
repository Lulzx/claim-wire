# 15. Testing and conformance

**Status:** draft · **Paper:** —

## Scope
How open-magnetar is tested: the layers, the test-vector formats under `reference/vectors/`, how Rust crates consume
them, RISC-V conformance, the negative-test catalogue, differential testing, fuzzing, and CI. Security accounting
is in 13; the calculator `reference/openmagnetar/params.py` is tested by `reference/tests/test_params.py`.

## 15.1 Resolved questions

1. **How far does the reference model go?** **[OM-choice]** The arithmetization (emulator, virtual-instruction
   expansion, witness, both R1CS relations, cross-step constraints) in full; every sub-protocol (transcript, sum-check,
   PCS, Mosaic, Spartan, Twist/Shout/one-hot, routers) as a real prover and verifier at toy sizes; the Ligerito PCS
   at `n <= 12` only. There is **no** full-size end-to-end prover in Python; the end-to-end protocol (11) is a small
   composed model (`protocol.py`, owned by the author of 11) on toy parameters. Performance is the Rust crates' job.
2. **Python and dependencies.** **[OM-choice]** Pure Python 3.12+, stdlib only (`unittest`, `hashlib`, `json`, `random`),
   for auditability (C8). Suite: `cd <repo> && python3 -m unittest discover -s reference/tests -t reference`
   (<!-- cw:inventory.python_tests -->192<!-- /cw --> tests, about one minute).
