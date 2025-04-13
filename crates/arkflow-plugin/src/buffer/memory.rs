/*
 *    Licensed under the Apache License, Version 2.0 (the "License");
 *    you may not use this file except in compliance with the License.
 *    You may obtain a copy of the License at
 *
 *        http://www.apache.org/licenses/LICENSE-2.0
 *
 *    Unless required by applicable law or agreed to in writing, software
 *    distributed under the License is distributed on an "AS IS" BASIS,
 *    WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *    See the License for the specific language governing permissions and
 *    limitations under the License.
 */

use crate::time::deserialize_duration;
use arkflow_core::buffer::{register_buffer_builder, Buffer, BufferBuilder};
use arkflow_core::input::Ack;
use arkflow_core::{Error, MessageBatch};
use async_trait::async_trait;
use datafusion::arrow;
use datafusion::arrow::array::RecordBatch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time;
use tokio::sync::{Notify, RwLock};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBufferConfig {
    capacity: u32,
    #[serde(deserialize_with = "deserialize_duration")]
    timeout: time::Duration,
}

pub struct MemoryBuffer {
    config: MemoryBufferConfig,
    queue: Arc<RwLock<VecDeque<(MessageBatch, Arc<dyn Ack>)>>>,
    notify: Arc<Notify>,
    close: CancellationToken,
}

impl MemoryBuffer {
    fn new(config: MemoryBufferConfig) -> Result<Self, Error> {
        let notify = Arc::new(Notify::new());
        let notify_clone = Arc::clone(&notify);
        let duration = config.timeout.clone();
        let close = CancellationToken::new();
        let close_clone = close.clone();

        tokio::spawn(async move {
            loop {
                let timer = sleep(duration);
                tokio::select! {
                    _ = timer => {
                        // notify read
                        notify_clone.notify_waiters();
                    }
                    _ = close_clone.cancelled() => {
                         // notify read
                        notify_clone.notify_waiters();
                        break;
                    }
                    _ = notify_clone.notified() => {
                    }
                }
            }
        });
        Ok(Self {
            close,
            notify,
            config,
            queue: Arc::new(Default::default()),
        })
    }

    async fn process_messages(&self) -> Result<Option<(MessageBatch, Arc<dyn Ack>)>, Error> {
        let queue_arc = Arc::clone(&self.queue);
        let mut queue_lock = queue_arc.write().await;

        if queue_lock.is_empty() {
            return Ok(None);
        }

        let mut messages = Vec::new();
        let mut acks = Vec::new();

        while let Some((msg, ack)) = queue_lock.pop_back() {
            messages.push(msg);
            acks.push(ack);
        }

        // 如果没有消息，返回 None
        if messages.is_empty() {
            return Ok(None);
        }
        let schema = messages[0].schema();
        let x: Vec<RecordBatch> = messages.into_iter().map(|batch| batch.into()).collect();
        let new_batch = arrow::compute::concat_batches(&schema, &x)
            .map_err(|e| Error::Process(format!("Merge batches failed: {}", e)))?;

        let new_ack = Arc::new(ArrayAck(acks));
        Ok(Some((MessageBatch::new_arrow(new_batch), new_ack)))
    }
}

#[async_trait]
impl Buffer for MemoryBuffer {
    async fn write(&self, msg: MessageBatch, arc: Arc<dyn Ack>) -> Result<(), Error> {
        let queue_arc = Arc::clone(&self.queue);

        let mut queue_lock = queue_arc.write().await;
        queue_lock.push_front((msg, arc));
        let cnt = queue_lock.iter().map(|x| x.0.len()).reduce(|acc, x| {
            return acc + x;
        });
        let cnt = cnt.unwrap_or(0);
        if cnt >= self.config.capacity as usize {
            let notify = self.notify.clone();
            notify.notify_waiters();
        }
        Ok(())
    }

    async fn read(&self) -> Result<Option<(MessageBatch, Arc<dyn Ack>)>, Error> {
        let notify = Arc::clone(&self.notify);
        notify.notified().await;

        self.process_messages().await
    }

    async fn flush(&self) -> Result<(), Error> {
        let notify = Arc::clone(&self.notify);
        notify.notify_waiters();
        Ok(())
    }

    async fn close(&self) -> Result<(), Error> {
        self.close.cancel();

        Ok(())
    }
}
struct ArrayAck(Vec<Arc<dyn Ack>>);
#[async_trait]
impl Ack for ArrayAck {
    async fn ack(&self) {
        for ack in self.0.iter() {
            ack.ack().await;
        }
    }
}

struct MemoryBufferBuilder;

impl BufferBuilder for MemoryBufferBuilder {
    fn build(&self, config: &Option<Value>) -> Result<Arc<dyn Buffer>, Error> {
        if config.is_none() {
            return Err(Error::Config(
                "Memory buffer configuration is missing".to_string(),
            ));
        }

        let config: MemoryBufferConfig = serde_json::from_value(config.clone().unwrap())?;
        Ok(Arc::new(MemoryBuffer::new(config)?))
    }
}

pub fn init() {
    register_buffer_builder("memory", Arc::new(MemoryBufferBuilder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkflow_core::input::NoopAck;

    #[tokio::test]
    async fn test_memory_buffer_new() {
        let p = MemoryBuffer::new(MemoryBufferConfig {
            capacity: 10,
            timeout: time::Duration::from_secs(10),
        })
        .unwrap();

        let x = p
            .write(
                MessageBatch::new_binary(vec!["test".as_bytes().to_vec()]).unwrap(),
                Arc::new(NoopAck),
            )
            .await;
    }
}
