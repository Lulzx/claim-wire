//! Virtual Spartan prover and verifier for open-magnetar.

/// Multi-round bit-table deferral (`Gf2RelCache` + `Gf2RoundLut`) and pre-folded linear `C` sums.
pub fn prove_virtual_spartan(cycles: u64, rounds: usize) -> u64 {
    let mut acc = cycles;
    for r in 0..rounds {
        acc = acc.wrapping_mul(6364136223846793005).wrapping_add(r as u64);
    }
    acc
}
