//! GC-independent GNU bytecode decoding and instruction-boundary metadata.
//!
//! Decoding is structural, not a proof of stack/constant/type correctness. Bad
//! instructions remain addressable error entries so an already-installed Lisp
//! condition handler can catch a decoder error at execution time.
use super::opcode::OpCode;
use std::{collections::HashMap, fmt, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DecodeError {
    UnknownOpcode { offset: usize, byte: u8 },
    Truncated { offset: usize },
    NotInstruction { offset: usize },
    InvalidTarget { offset: usize },
    MissingOperand { offset: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOpcode { offset, byte } => {
                write!(f, "Invalid Bytecode: opcode {byte} at offset {offset}")
            }
            Self::Truncated { offset } => {
                write!(f, "Truncated bytecode instruction at offset {offset}")
            }
            Self::NotInstruction { offset } => {
                write!(f, "Bytecode offset {offset} is not an instruction boundary")
            }
            Self::InvalidTarget { offset } => {
                write!(f, "Bytecode jump target {offset} is out of range")
            }
            Self::MissingOperand { offset } => {
                write!(f, "Bytecode instruction at {offset} has no operand")
            }
        }
    }
}
impl std::error::Error for DecodeError {}

/// Owned scalar metadata, never pointers into the moving Lisp heap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Instruction {
    pub opcode: OpCode,
    pub offset: usize,
    pub next: usize,
    operand: Option<u16>,
}

/// Frame-local requirements checked before dispatch touches the value stack.
/// `outputs` includes retained operands, so outputs-inputs is net growth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StackEffect {
    pub inputs: usize,
    pub outputs: usize,
}

impl Instruction {
    pub fn operand(self) -> Result<u16, DecodeError> {
        self.operand.ok_or(DecodeError::MissingOperand { offset: self.offset })
    }

    pub fn stack_effect(self) -> Option<StackEffect> {
        use OpCode as O;
        let byte = self.opcode as u8;
        let operand = usize::from(self.operand.unwrap_or(0));
        let effect = match self.opcode {
            O::StackRef0
            | O::StackRef1
            | O::StackRef2
            | O::StackRef3
            | O::StackRef4
            | O::StackRef5 => {
                let inputs = usize::from(byte) + 1;
                (inputs, inputs + 1)
            }
            O::StackRefN | O::StackRefN2 => (operand + 1, operand + 2),
            O::VarRef0
            | O::VarRef1
            | O::VarRef2
            | O::VarRef3
            | O::VarRef4
            | O::VarRef5
            | O::VarRefN
            | O::VarRefN2 => (0, 1),
            // Variable writes validate their constant/type before stack access.
            // Their existing checked helpers preserve that error precedence.
            O::VarSet0
            | O::VarSet1
            | O::VarSet2
            | O::VarSet3
            | O::VarSet4
            | O::VarSet5
            | O::VarSetN
            | O::VarSetN2
            | O::VarBind0
            | O::VarBind1
            | O::VarBind2
            | O::VarBind3
            | O::VarBind4
            | O::VarBind5
            | O::VarBindN
            | O::VarBindN2 => return None,
            O::Call0 | O::Call1 | O::Call2 | O::Call3 | O::Call4 | O::Call5 => {
                (usize::from(byte - O::Call0 as u8) + 1, 1)
            }
            O::CallN | O::CallN2 => (operand + 1, 1),
            O::PushCondtionCase => (1, 0),
            O::Nth
            | O::Eq
            | O::Memq
            | O::Cons
            | O::List2
            | O::Aref
            | O::Set
            | O::Fset
            | O::Get
            | O::EqlSign
            | O::GreaterThan
            | O::LessThan
            | O::LessThanOrEqual
            | O::GreaterThanOrEqual
            | O::Diff
            | O::Plus
            | O::Max
            | O::Min
            | O::Multiply
            | O::Equal
            | O::Nthcdr
            | O::Elt
            | O::Member
            | O::Assq
            | O::Setcar
            | O::Setcdr
            | O::Nconc
            | O::Quo
            | O::Rem
            | O::Concat2 => (2, 1),
            O::Symbolp
            | O::Consp
            | O::Stringp
            | O::Listp
            | O::Not
            | O::Car
            | O::Cdr
            | O::List1
            | O::Length
            | O::SymbolValue
            | O::SymbolFunction
            | O::Sub1
            | O::Add1
            | O::Negate
            | O::Nreverse
            | O::CarSafe
            | O::CdrSafe
            | O::Numberp
            | O::Integerp => (1, 1),
            O::List3 | O::Aset | O::Concat3 => (3, 1),
            O::List4 | O::Concat4 => (4, 1),
            O::GotoIfNil | O::GotoIfNonNil | O::Discard | O::Return | O::PushCatch => (1, 0),
            O::GotoIfNilElsePop | O::GotoIfNonNilElsePop => (1, 1),
            O::Duplicate => (1, 2),
            O::StackSetN | O::StackSetN2 => (operand + 1, operand),
            O::DiscardN => {
                let count = operand & 0x7f;
                let keep = usize::from(operand & 0x80 != 0);
                (count + keep, keep)
            }
            O::ListN | O::ConcatN => (operand, 1),
            O::Switch => (2, 0),
            _ if byte >= O::Constant0 as u8 || self.opcode == O::ConstantN2 => (0, 1),
            // Unsupported operations return a Lisp error without touching the
            // value stack; no speculative effect is assigned to them yet.
            _ => return None,
        };
        Some(StackEffect { inputs: effect.0, outputs: effect.1 })
    }

    pub fn jump_target(self) -> Option<usize> {
        match self.opcode {
            OpCode::Goto
            | OpCode::GotoIfNil
            | OpCode::GotoIfNonNil
            | OpCode::GotoIfNilElsePop
            | OpCode::GotoIfNonNilElsePop
            | OpCode::PushCondtionCase
            | OpCode::PushCatch => self.operand.map(usize::from),
            _ => None,
        }
    }
}

/// GNU bytecode.c BYTE_CODES: low-six families use inline/8/16-bit
/// operands; branch addresses and constant2 are little-endian 16-bit values.
fn operand_bytes(opcode: OpCode) -> usize {
    let byte = opcode as u8;
    if byte < 48 {
        return match byte & 7 {
            6 => 1,
            7 => 2,
            _ => 0,
        };
    }
    match opcode {
        OpCode::PushCondtionCase
        | OpCode::PushCatch
        | OpCode::ConstantN2
        | OpCode::Goto
        | OpCode::GotoIfNil
        | OpCode::GotoIfNonNil
        | OpCode::GotoIfNilElsePop
        | OpCode::GotoIfNonNilElsePop
        | OpCode::StackSetN2 => 2,
        OpCode::ListN
        | OpCode::ConcatN
        | OpCode::InsertN
        | OpCode::StackSetN
        | OpCode::DiscardN => 1,
        _ => 0,
    }
}

#[derive(Debug)]
pub(super) struct Program {
    len: usize,
    // Zero denotes an operand byte; other values are instruction index + 1.
    boundaries: Box<[usize]>,
    entries: Box<[Result<Instruction, DecodeError>]>,
}

impl Program {
    pub fn decode(bytes: &[u8]) -> Self {
        let mut boundaries = vec![0; bytes.len()];
        let mut entries = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            boundaries[offset] = entries.len() + 1;
            let opcode = match OpCode::try_from(bytes[offset]) {
                Ok(opcode) => opcode,
                Err(_) => {
                    entries.push(Err(DecodeError::UnknownOpcode { offset, byte: bytes[offset] }));
                    offset += 1;
                    continue;
                }
            };
            let width = operand_bytes(opcode);
            let Some(operands) = bytes.get(offset + 1..).and_then(|tail| tail.get(..width)) else {
                entries.push(Err(DecodeError::Truncated { offset }));
                break;
            };
            let operand = match operands {
                [] => None,
                [byte] => Some(u16::from(*byte)),
                [low, high] => Some(u16::from_le_bytes([*low, *high])),
                _ => unreachable!("operand width metadata exceeds two bytes"),
            };
            let next = offset + 1 + width;
            entries.push(Ok(Instruction { opcode, offset, next, operand }));
            offset = next;
        }
        Self {
            len: bytes.len(),
            boundaries: boundaries.into_boxed_slice(),
            entries: entries.into_boxed_slice(),
        }
    }

    pub fn instruction(&self, offset: usize) -> Result<Instruction, DecodeError> {
        if offset >= self.len {
            return Err(DecodeError::Truncated { offset });
        }
        let index = self.boundaries[offset];
        if index == 0 {
            return Err(DecodeError::NotInstruction { offset });
        }
        self.entries[index - 1].clone()
    }

    pub fn validate_target(&self, offset: usize) -> Result<(), DecodeError> {
        if offset >= self.len {
            return Err(DecodeError::InvalidTarget { offset });
        }
        if self.boundaries[offset] == 0 {
            return Err(DecodeError::NotInstruction { offset });
        }
        Ok(())
    }

    /// Optional eager structural verification for loaders/tooling. Execution
    /// deliberately checks lazily, preserving catchable decoder failures.
    pub fn validate(&self) -> Result<(), DecodeError> {
        if self.entries.is_empty() {
            return Err(DecodeError::Truncated { offset: 0 });
        }
        for entry in &self.entries {
            let instruction = entry.as_ref().map_err(Clone::clone)?;
            if let Some(target) = instruction.jump_target() {
                self.validate_target(target)?;
            }
        }
        Ok(())
    }
}

/// Integer offsets and Arc-owned decoded code survive collections and frame
/// switches without retaining any untraced pointers into ByteFn storage.
#[derive(Clone, Debug)]
pub(super) struct ProgramCounter {
    program: Arc<Program>,
    offset: usize,
}

impl ProgramCounter {
    #[cfg(test)]
    pub fn new(bytes: &[u8]) -> Self {
        Self { program: Arc::new(Program::decode(bytes)), offset: 0 }
    }

    pub fn as_offset(&self) -> usize {
        self.offset
    }

    pub fn fetch(&mut self) -> Result<Instruction, DecodeError> {
        let instruction = self.program.instruction(self.offset)?;
        self.offset = instruction.next;
        Ok(instruction)
    }

    pub fn validate_target(&self, offset: usize) -> Result<(), DecodeError> {
        self.program.validate_target(offset)
    }

    pub fn goto(&mut self, offset: usize) -> Result<(), DecodeError> {
        self.validate_target(offset)?;
        self.offset = offset;
        Ok(())
    }
}

/// Per-VM cache keyed by immutable code bytes, not movable function addresses.
/// Re-entering a compiled function needs no code copy or decoding on a hit.
#[derive(Default, Debug)]
pub(super) struct ProgramCache(HashMap<Box<[u8]>, Arc<Program>>);

impl ProgramCache {
    pub fn cursor(&mut self, bytes: &[u8], offset: usize) -> Result<ProgramCounter, DecodeError> {
        let program = if let Some(program) = self.0.get(bytes) {
            program.clone()
        } else {
            let program = Arc::new(Program::decode(bytes));
            self.0.insert(bytes.into(), program.clone());
            program
        };
        // A saved return offset may be exactly EOF. The subsequent fetch then
        // signals a catchable truncation error instead of fabricating an opcode.
        if offset != program.len {
            program.validate_target(offset)?;
        }
        Ok(ProgramCounter { program, offset })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OpCode as O;

    #[test]
    fn every_opcode_family_has_checked_operand_widths() {
        for byte in 0..=u8::MAX {
            let Ok(opcode) = O::try_from(byte) else {
                continue;
            };
            let width = operand_bytes(opcode);
            let mut bytes = vec![byte];
            bytes.extend([0x34, 0x12].into_iter().take(width));
            let instruction = Program::decode(&bytes).instruction(0).unwrap();
            assert_eq!(instruction.next, 1 + width);
            assert_eq!(
                instruction.operand,
                match width {
                    0 => None,
                    1 => Some(0x34),
                    _ => Some(0x1234),
                }
            );
            for short in 1..=width {
                assert!(matches!(
                    Program::decode(&bytes[..short]).instruction(0),
                    Err(DecodeError::Truncated { offset: 0 })
                ));
            }
        }
    }

    #[test]
    fn branches_must_target_instruction_starts_not_operands_or_eof() {
        for target in [1_u16, 2, 4, u16::MAX] {
            let [low, high] = target.to_le_bytes();
            let program = Program::decode(&[O::Goto as u8, low, high, O::Return as u8]);
            assert!(program.validate().is_err(), "target {target}");
        }
        let program = Program::decode(&[O::Goto as u8, 3, 0, O::Return as u8]);
        program.validate().unwrap();
        assert!(program.instruction(1).is_err());
    }

    #[test]
    fn failed_fetch_and_jump_leave_cursor_unchanged() {
        let mut cursor = ProgramCounter::new(&[O::ConstantN2 as u8, 0]);
        assert!(cursor.fetch().is_err());
        assert_eq!(cursor.as_offset(), 0);
        assert!(cursor.goto(1).is_err());
        assert_eq!(cursor.as_offset(), 0);
        let mut empty = ProgramCounter::new(&[]);
        assert!(empty.fetch().is_err());
        assert!(empty.goto(0).is_err());
    }

    #[test]
    fn decoded_image_owns_metadata_and_cache_is_content_addressed() {
        let mut bytes = vec![O::Constant0 as u8, O::Return as u8];
        let mut cache = ProgramCache::default();
        let mut first = cache.cursor(&bytes, 0).unwrap();
        let second = cache.cursor(&bytes.clone(), 0).unwrap();
        assert!(Arc::ptr_eq(&first.program, &second.program));
        bytes.fill(51);
        drop(bytes);
        assert_eq!(first.fetch().unwrap().opcode, O::Constant0);
        let mut resumed = cache.cursor(&[O::Constant0 as u8, O::Return as u8], 2).unwrap();
        assert!(resumed.fetch().is_err());
    }

    #[test]
    fn invalid_instructions_remain_lazy_error_boundaries() {
        let mut cursor = ProgramCounter::new(&[O::Constant0 as u8, 51, O::Return as u8]);
        assert_eq!(cursor.fetch().unwrap().opcode, O::Constant0);
        assert!(matches!(
            cursor.fetch(),
            Err(DecodeError::UnknownOpcode { offset: 1, byte: 51 })
        ));
        cursor.goto(2).unwrap();
        assert_eq!(cursor.fetch().unwrap().opcode, O::Return);
        assert!(Program::decode(&[O::PushCondtionCase as u8, 1, 0]).validate().is_err());
    }
}
