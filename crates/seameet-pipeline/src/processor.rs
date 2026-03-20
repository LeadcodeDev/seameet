use seameet_core::frame::{PcmFrame, VideoFrame};
use seameet_core::traits::{Processor, ProcessorHint};

// Re-export core types for convenience.
pub use seameet_core::traits::Passthrough;

/// A chain of processors applied in sequence.
///
/// Each frame is passed through the processors in order. If any processor
/// returns [`ProcessorHint::Drop`] or [`ProcessorHint::Emit`], the chain
/// short-circuits and returns that hint immediately.
#[derive(Default)]
pub struct ProcessorChain {
    processors: Vec<Box<dyn Processor>>,
}

impl std::fmt::Debug for ProcessorChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessorChain")
            .field("len", &self.processors.len())
            .finish()
    }
}

impl ProcessorChain {
    /// Creates an empty processor chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a processor to the end of the chain.
    pub fn push(&mut self, processor: Box<dyn Processor>) {
        self.processors.push(processor);
    }

    /// Returns the number of processors in the chain.
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// Returns `true` if the chain has no processors.
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }

    /// Processes an audio frame through the chain.
    pub fn process_audio(&mut self, frame: &mut PcmFrame) -> ProcessorHint {
        for proc in &mut self.processors {
            match proc.process_audio(frame) {
                ProcessorHint::Forward => continue,
                hint => return hint,
            }
        }
        ProcessorHint::Forward
    }

    /// Processes a video frame through the chain.
    pub fn process_video(&mut self, frame: &mut VideoFrame) -> ProcessorHint {
        for proc in &mut self.processors {
            match proc.process_video(frame) {
                ProcessorHint::Forward => continue,
                hint => return hint,
            }
        }
        ProcessorHint::Forward
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seameet_core::events::RoomEvent;
    use seameet_core::ParticipantId;
    use std::any::Any;

    struct DropProcessor;

    impl Processor for DropProcessor {
        fn process_audio(&mut self, _frame: &mut PcmFrame) -> ProcessorHint {
            ProcessorHint::Drop
        }
        fn process_video(&mut self, _frame: &mut VideoFrame) -> ProcessorHint {
            ProcessorHint::Drop
        }
    }

    struct EmitProcessor {
        participant: ParticipantId,
    }

    impl Processor for EmitProcessor {
        fn process_audio(&mut self, _frame: &mut PcmFrame) -> ProcessorHint {
            ProcessorHint::Emit(RoomEvent::Custom {
                participant: self.participant,
                payload: Box::new(42u32) as Box<dyn Any + Send>,
            })
        }
        fn process_video(&mut self, _frame: &mut VideoFrame) -> ProcessorHint {
            ProcessorHint::Forward
        }
    }

    #[test]
    fn test_processor_chain_forward() {
        let mut chain = ProcessorChain::new();
        chain.push(Box::new(Passthrough));
        chain.push(Box::new(Passthrough));

        let mut frame = PcmFrame {
            samples: vec![1.0],
            sample_rate: 48000,
            channels: 1,
        };
        assert!(matches!(chain.process_audio(&mut frame), ProcessorHint::Forward));
    }

    #[test]
    fn test_processor_chain_drop() {
        let mut chain = ProcessorChain::new();
        chain.push(Box::new(Passthrough));
        chain.push(Box::new(DropProcessor));
        chain.push(Box::new(Passthrough)); // should not be reached

        let mut frame = PcmFrame {
            samples: vec![1.0],
            sample_rate: 48000,
            channels: 1,
        };
        assert!(matches!(chain.process_audio(&mut frame), ProcessorHint::Drop));
    }

    #[test]
    fn test_processor_chain_emit() {
        let pid = ParticipantId::random();
        let mut chain = ProcessorChain::new();
        chain.push(Box::new(EmitProcessor { participant: pid }));

        let mut frame = PcmFrame {
            samples: vec![1.0],
            sample_rate: 48000,
            channels: 1,
        };
        match chain.process_audio(&mut frame) {
            ProcessorHint::Emit(RoomEvent::Custom { participant, payload }) => {
                assert_eq!(participant, pid);
                assert!(payload.downcast_ref::<u32>().is_some());
            }
            other => panic!("expected Emit, got {other:?}"),
        }
    }
}
