# Public-vector validation

## Recorded run

On 2026-10-16, the Rust CLI executed the C SHA-256 guest, generated real
Ligerito proofs, and verified them against three known SHA-256 answers.
All three outputs matched, all three proofs verified, and all six negative
checks below were rejected.

The run used the local working tree based on commit `9872a7e`, including
uncommitted implementation changes. These results do not describe that
commit alone. The host CLI and guest ELFs were freshly built for the run.

Configuration:

- Release Rust CLI, `provable-120` profile, Ligerito PCS, SHA-256 transcript
  hashing, default `pcs-r = 2`; no mock PCS.
- RV64I C guests compiled with LLVM clang and lld on macOS.
- RAM: `A = 16`; input and output regions: `I = O = 14`.
- Exact byte inputs, without trailing newlines.

## Known answers and proof results

The `abc` and 56-byte SHA-256 vectors appear in the test driver of
[RFC 6234, section 8.5](https://www.rfc-editor.org/rfc/rfc6234#section-8.5).
The empty-message digest was also checked. Every expected digest below was
independently checked with Python's `hashlib.sha256` before comparison with
the guest output.
<!-- cw:table validation.public_vectors -->
| Case | Input bytes | Padded virtual cycles | Proof bytes | Output matches | Verification |
|---|---:|---:|---:|---|---|
| Empty message | 0 | 8,192 | 211,390 | Yes | Accepted |
| `abc` | 3 | 8,192 | 210,814 | Yes | Accepted |
| Standard 56-byte message | 56 | 16,384 | 245,214 | Yes | Accepted |

Exact inputs and expected hexadecimal digests:

```text
Input: empty byte string
SHA-256: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

Input: abc
Input hex: 616263
SHA-256: ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad

Input: abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq
SHA-256: 248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1
```

For each case, the serialized public instance contained the original input,
the expected 32-byte digest, and exit code zero. Execution through `om run`
also produced the expected digest.
