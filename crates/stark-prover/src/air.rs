//! AIR (Algebraic Intermediate Representation) for the signature batch
//! commitment circuit.
//!
//! See [`crate`] module docs for the full circuit description.

use winterfell::{
    math::{fields::f128::BaseElement, FieldElement, ToElements},
    Air, AirContext, Assertion, EvaluationFrame, ProofOptions, TraceInfo,
    TransitionConstraintDegree,
};

// ── Public inputs ────────────────────────────────────────────────────────────

/// Public inputs for the signature batch commitment proof.
///
/// - `batch_root`: the final accumulator value after hashing all entries.
/// - `n_sigs`: number of signatures included in the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigBatchPublicInputs {
    /// Final accumulator value — committed to in the block header.
    pub batch_root: BaseElement,
    /// Number of signatures in the batch (padded trace length may be larger).
    pub n_sigs: usize,
}

impl ToElements<BaseElement> for SigBatchPublicInputs {
    fn to_elements(&self) -> Vec<BaseElement> {
        vec![
            self.batch_root,
            BaseElement::new(self.n_sigs as u128),
        ]
    }
}

// ── AIR ──────────────────────────────────────────────────────────────────────

/// Trace column indices.
pub const COL_ACC: usize = 0;
pub const COL_ENTRY: usize = 1;

/// Number of trace columns.
pub const TRACE_WIDTH: usize = 2;

/// AIR for the hash-chain accumulator circuit.
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

        // Single transition constraint of degree 3:
        //   acc[t+1] = acc[t]^3 + entry[t]
        let degrees = vec![TransitionConstraintDegree::new(3)];

        // Two boundary assertions: acc at step 0 and at the last step.
        let num_assertions = 2;

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
        let cur_acc = frame.current()[COL_ACC];
        let cur_entry = frame.current()[COL_ENTRY];
        let next_acc = frame.next()[COL_ACC];

        // acc[t+1] - (acc[t]^3 + entry[t]) = 0
        result[0] = next_acc - (cur_acc.exp(3u32.into()) + cur_entry);
    }

    fn get_assertions(&self) -> Vec<Assertion<Self::BaseField>> {
        let last_step = self.trace_length() - 1;
        vec![
            // Accumulator must start at zero.
            Assertion::single(COL_ACC, 0, BaseElement::ZERO),
            // Accumulator must end at the claimed batch_root.
            Assertion::single(COL_ACC, last_step, self.pub_inputs.batch_root),
        ]
    }

    fn context(&self) -> &AirContext<Self::BaseField> {
        &self.context
    }
}
