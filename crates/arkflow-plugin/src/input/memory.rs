//! Memory input component
//!
//! Read data from an in-memory message queue

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use arkflow_core::input::{register_input_builder, Ack, Input, InputBuilder, NoopAck};
use arkflow_core::{Error, MessageBatch};

/// Memory input configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryInputConfig {
    /// Initial message for the memory queue
    pub messages: Option<Vec<String>>,
}

/// Memory input component
pub struct MemoryInput {
    queue: Arc<Mutex<VecDeque<MessageBatch>>>,
    connected: AtomicBool,
}

impl MemoryInput {
    /// Create a new memory input component
    pub fn new(config: MemoryInputConfig) -> Result<Self, Error> {
        let mut queue = VecDeque::new();

        // If there is an initial message in the configuration, it is added to the queue
        if let Some(messages) = &config.messages {
            for msg_str in messages {
                queue.push_back(MessageBatch::from_string(msg_str)?);
            }
        }

        Ok(Self {
            queue: Arc::new(Mutex::new(queue)),
            connected: AtomicBool::new(false),
        })
    }

    /// Add a message to the memory input
    pub async fn push(&self, msg: MessageBatch) -> Result<(), Error> {
        let mut queue = self.queue.lock().await;
        queue.push_back(msg);
        Ok(())
    }
}

#[async_trait]
impl Input for MemoryInput {
    async fn connect(&self) -> Result<(), Error> {
        self.connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn read(&self) -> Result<(MessageBatch, Arc<dyn Ack>), Error> {
        if !self.connected.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::Connection("The input is not connected".to_string()));
        }

        // Try to get a message from the queue
        let msg_option;
        {
            let mut queue = self.queue.lock().await;
            msg_option = queue.pop_front();
        }

        if let Some(msg) = msg_option {
            Ok((msg, Arc::new(NoopAck)))
        } else {
            Err(Error::EOF)
        }
    }

    async fn close(&self) -> Result<(), Error> {
        self.connected
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

pub(crate) struct MemoryInputBuilder;
impl InputBuilder for MemoryInputBuilder {
    fn build(&self, config: &Option<serde_json::Value>) -> Result<Arc<dyn Input>, Error> {
        if config.is_none() {
            return Err(Error::Config(
                "Memory input configuration is missing".to_string(),
            ));
        }
        let config: MemoryInputConfig = serde_json::from_value(config.clone().unwrap())?;
        Ok(Arc::new(MemoryInput::new(config)?))
    }
}

pub fn init() {
    register_input_builder("memory", Arc::new(MemoryInputBuilder));
}
