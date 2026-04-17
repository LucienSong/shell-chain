mod engine;
mod error;
mod finality;
mod fork_choice;
mod poa;
pub mod challenge;
pub mod peer_scoring;
pub mod prover_registry;
pub mod rate_limiter;
pub mod slashing;
pub mod validator;
pub mod window;
pub mod wpoa;

pub use challenge::{ChallengeReason, ChallengeResponse, ProofChallenge};
pub use engine::{ConsensusEngine, EngineType};
pub use error::ConsensusError;
pub use finality::{Attestation, FinalityState};
pub use fork_choice::{BlockScore, ForkChoice};
pub use poa::{PoaConfig, PoaEngine};
pub use peer_scoring::{PeerEvent, PeerScorer, PeerScoringConfig, PeerId as ScoringPeerId};
pub use prover_registry::{ProverRecord, ProverRegistry, ProverRegistryConfig, RegistryError};
pub use rate_limiter::{ProofRateLimiter, RateLimiterConfig};
pub use slashing::{
    detect_double_sign, detect_offline, EquivocationProof, SlashEvidence, SlashRecord, SlashType,
    SlashingConfig,
};
pub use validator::{ValidatorInfo, ValidatorSet, ValidatorSetConfig, ValidatorStatus};
pub use window::{ProofWindowManager, WindowConfig, WindowError, WindowState};
pub use wpoa::{WPoaConfig, WPoaEngine};
