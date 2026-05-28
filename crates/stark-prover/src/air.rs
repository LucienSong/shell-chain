//! AIR (Algebraic Intermediate Representation) for the signature batch
//! commitment circuit.
//!
//! See [`crate`] module docs for the full circuit description.
//!
//! # Circuit summary
//!
//! Each signature is mapped to a 256-bit BLAKE3 leaf:
//! `leaf_i = BLAKE3(msg_hash_i ‖ pk_hash_i)`
//!
//! The leaf is split into two f128 field elements:
//! `leaf_lo_i = u128::from_le_bytes(leaf_i[0..16])`
//! `leaf_hi_i = u128::from_le_bytes(leaf_i[16..32])`
//!
//! Two parallel degree-3 accumulators produce the 256-bit batch root:
//! `acc_lo[t+1] = acc_lo[t]^3 + leaf_lo[t]`
//! `acc_hi[t+1] = acc_hi[t]^3 + leaf_hi[t]`
//!
//! The batch root is `acc_lo_final ‖ acc_hi_final` (32 LE bytes).

use winterfell::{
    math::{fields::f128::BaseElement, FieldElement, ToElements},
    Air, AirContext, Assertion, EvaluationFrame, ProofOptions, TraceInfo,
    TransitionConstraintDegree,
};

// ── Public inputs ────────────────────────────────────────────────────────────

/// Public inputs for the signature batch commitment proof.
///
/// - `batch_root_lo` / `batch_root_hi`: the two halves of the 256-bit root.
/// - `n_sigs`: number of signatures included in the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigBatchPublicInputs {
    /// Low 128 bits of the final batch root (acc_lo after all entries).
    pub batch_root_lo: BaseElement,
    /// High 128 bits of the final batch root (acc_hi after all entries).
    pub batch_root_hi: BaseElement,
    /// Number of signatures in the batch (padded trace length may be larger).
    pub n_sigs: usize,
}

impl ToElements<BaseElement> for SigBatchPublicInputs {
    fn to_elements(&self) -> Vec<BaseElement> {
        vec![
            self.batch_root_lo,
            self.batch_root_hi,
            BaseElement::new(self.n_sigs as u128),
        ]
    }
}

// ── AIR ──────────────────────────────────────────────────────────────────────

/// Trace column indices.
pub const COL_ACC_LO: usize = 0;
pub const COL_ACC_HI: usize = 1;
pub const COL_LEAF_LO: usize = 2;
pub const COL_LEAF_HI: usize = 3;

/// Number of trace columns.
pub const TRACE_WIDTH: usize = 4;

/// AIR for the dual hash-chain accumulator circuit.
///
/// Two parallel degree-3 transitions:
/// - `acc_lo[t+1] = acc_lo[t]^3 + leaf_lo[t]`
/// - `acc_hi[t+1] = acc_hi[t]^3 + leaf_hi[t]`
pub struct SigBatchAir {
    context: AirContext<BaseElement>,
    pub_inputs: SigBatchPublicInputs,
}

impl Air for SigBatchAir {
    type BaseField = BaseElement;
    type PublicInputs = SigBatchPublicInputs;

    fn new(trace_info: TraceInfo, pub_inputs: SigBatchPublicInputs, options: ProofOptions) -> Self {
        assert_eq!(
            TRACE_WIDTH,
            trace_info.width(),
            "SigBatchAir requires exactly {TRACE_WIDTH} trace columns"
        );

        // Two parallel transition constraints of degree 3:
        //   acc_lo[t+1] = acc_lo[t]^3 + leaf_lo[t]
        //   acc_hi[t+1] = acc_hi[t]^3 + leaf_hi[t]
        let degrees = vec![
            TransitionConstraintDegree::new(3),
            TransitionConstraintDegree::new(3),
        ];

        // Four boundary assertions: both accumulators at step 0 and last step.
        let num_assertions = 4;

        SigBatchAir {
            context: AirContext::new(trace_info, degrees, num_assertions, options),
            pub_inputs,
        }
    }

    fn evaluate_transition<E: FieldElement + From<Self::BaseField>>(
        &self,
        frame: &EvaluationFrame<E>,
        _periodic_values: &[E],
        result: &mut [E],
    ) {
        let cur_acc_lo = frame.current()[COL_ACC_LO];
        let cur_acc_hi = frame.current()[COL_ACC_HI];
        let cur_leaf_lo = frame.current()[COL_LEAF_LO];
        let cur_leaf_hi = frame.current()[COL_LEAF_HI];
        let next_acc_lo = frame.next()[COL_ACC_LO];
        let next_acc_hi = frame.next()[COL_ACC_HI];

        // acc_lo[t+1] - (acc_lo[t]^3 + leaf_lo[t]) = 0
        result[0] = next_acc_lo - (cur_acc_lo.exp(3u32.into()) + cur_leaf_lo);
        // acc_hi[t+1] - (acc_hi[t]^3 + leaf_hi[t]) = 0
        result[1] = next_acc_hi - (cur_acc_hi.exp(3u32.into()) + cur_leaf_hi);
    }

    fn get_assertions(&self) -> Vec<Assertion<Self::BaseField>> {
        let last_step = self.trace_length() - 1;
        vec![
            // Both accumulators must start at zero.
            Assertion::single(COL_ACC_LO, 0, BaseElement::ZERO),
            Assertion::single(COL_ACC_HI, 0, BaseElement::ZERO),
            // Both accumulators must end at the claimed batch root halves.
            Assertion::single(COL_ACC_LO, last_step, self.pub_inputs.batch_root_lo),
            Assertion::single(COL_ACC_HI, last_step, self.pub_inputs.batch_root_hi),
        ]
    }

    fn context(&self) -> &AirContext<Self::BaseField> {
        &self.context
    }
}
