//! Test that #[lez_program(instruction = "path")] accepts an external Instruction type.
//!
//! This tests the contract: programs can bring their own Instruction enum
//! and the framework will use it instead of generating one.

use lez_framework_core::error::LezError;
use lez_framework_core::types::LezOutput;

/// Simulates what a program with external Instruction would look like after expansion.
mod simulated_external_instruction {
    use super::*;

    // This would come from multisig_core or similar external crate
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, borsh::BorshSerialize, borsh::BorshDeserialize)]
    pub enum MyInstruction {
        DoSomething { value: u64 },
        DoSomethingElse,
    }

    // The macro would generate: `use my_crate::Instruction as Instruction;`
    #[allow(unused)]
    use MyInstruction as Instruction;

    // Verify the alias works for deserialization (what the generated main() does)
    #[test]
    fn test_external_instruction_deserializes() {
        let instr = Instruction::DoSomething { value: 42 };
        let bytes = borsh::to_vec(&instr).unwrap();
        let decoded: Instruction = borsh::from_slice(&bytes).unwrap();
        match decoded {
            Instruction::DoSomething { value } => assert_eq!(value, 42),
            _ => panic!("Wrong variant"),
        }
    }

    // Verify handler can return LezResult using the external instruction
    fn handle_do_something(value: u64) -> Result<LezOutput, LezError> {
        if value == 0 {
            return Err(LezError::custom(1, "value cannot be zero"));
        }
        Ok(LezOutput::states_only(vec![]))
    }

    #[test]
    fn test_handler_with_external_instruction() {
        assert!(handle_do_something(42).is_ok());
        let err = handle_do_something(0).unwrap_err();
        assert_eq!(err.error_code(), 6001);
    }
}
