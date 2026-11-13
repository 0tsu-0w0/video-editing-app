//! forge-core: EffectSpec の検証、HLSL の前置コード生成、.anm2 の生成と再検証。
//! UI にも GPU にも依存しない。

pub mod au2fx;
pub mod emit;
pub mod hlsl;
pub mod prompt;
pub mod spec;
pub mod verify;

pub use spec::{EffectSpec, Issue};

#[cfg(test)]
mod tests;
