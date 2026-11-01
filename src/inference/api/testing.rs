//! A [`Completion`] built from an assistant turn, for tests of the code that reads one
//! (docs/transport.md). Production builds completions only in the stream assemblers,
//! from what a response said.

use super::*;

impl Completion {
    /// A finalized completion whose turn is `output`, and which says nothing else: no
    /// response id or finish reason, the model `test`, zero usage, no cache-miss
    /// attribution or receipt, and the transient failure class, which means nothing
    /// without a failure. Each `with_*` method sets the field it names to the value given.
    pub(crate) fn new(output: AssistantOutput) -> Self {
        Self {
            assistant: Payload::assistant(output),
            id: None,
            model: "test".into(),
            finish: None,
            usage: Usage::default(),
            cache_miss: None,
            receipt: Receipt::default(),
            failure_class: FailureClass::Transient,
        }
    }

    pub(crate) fn with_finish(mut self, finish: Option<Finish>) -> Self {
        self.finish = finish;
        self
    }
}
