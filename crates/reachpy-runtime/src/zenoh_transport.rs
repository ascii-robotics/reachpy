//! Zenoh implementation of `Transport`.

use crate::transport::{RawCallback, Transport, TransportError};
use std::any::Any;
use std::sync::Arc;
use thiserror::Error;
use zenoh::{Session, Wait};

#[derive(Debug, Error)]
pub enum ZenohTransportError {
    #[error("failed to open zenoh session: {0}")]
    SessionOpen(String),
    #[error("failed to publish on '{topic}': {reason}")]
    Publish { topic: String, reason: String },
    #[error("failed to subscribe on '{topic}': {reason}")]
    Subscribe { topic: String, reason: String },
}

pub struct ZenohTransport {
    #[allow(dead_code)]
    node_name: String,
    session: Session,
}

impl ZenohTransport {
    pub fn open(node_name: &str) -> Result<Arc<Self>, ZenohTransportError> {
        let config = zenoh::Config::default();
        let session = zenoh::open(config)
            .wait()
            .map_err(|e| ZenohTransportError::SessionOpen(e.to_string()))?;

        Ok(Arc::new(Self {
            node_name: node_name.to_string(),
            session,
        }))
    }
}

impl Transport for ZenohTransport {
    fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), TransportError> {
        // Declaring a publisher per-call is wasteful for a hot path --
        // Topic in the Python layer should hold onto a Publisher/handle
        // rather than calling this per message in the long run. Kept
        // simple here since NodeHandle::publish() doesn't currently cache
        // per-topic publishers; worth revisiting once Topic wiring lands.
        self.session.put(topic, payload).wait().map_err(|e| {
            Box::new(ZenohTransportError::Publish {
                topic: topic.to_string(),
                reason: e.to_string(),
            }) as TransportError
        })
    }

    fn raw_subscribe(&self, topic: &str, on_raw: RawCallback) -> Result<Box<dyn Any + Send>, TransportError> {
        let subscriber = self
            .session
            .declare_subscriber(topic)
            .callback(move |sample| {
                let bytes = sample.payload().to_bytes().to_vec();
                on_raw(bytes);
            })
            .wait()
            .map_err(|e| {
                Box::new(ZenohTransportError::Subscribe {
                    topic: topic.to_string(),
                    reason: e.to_string(),
                }) as TransportError
            })?;

        Ok(Box::new(subscriber))
    }
}