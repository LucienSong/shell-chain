mod engine;
mod error;
mod finality;
mod fork_choice;
mod poa;
pub mod slashing;
pub mod validator;
pub mod wpoa;

pub use engine::{ConsensusEngine, EngineType};
pub use error::ConsensusError;
pub use finality::{Attestation, FinalityState};
pub use fork_choice::{BlockScore, ForkChoice};
pub use poa::{PoaConfig, PoaEngine};
pub use slashing::{
    detect_double_sign, detect_offline, SlashEvidence, SlashRecord, SlashType, SlashingConfig,
};
pub use validator::{ValidatorInfo, ValidatorSet, ValidatorSetConfig, ValidatorStatus};
pub use wpoa::{WPoaConfig, WPoaEngine};
