use pyo3::prelude::*;
use thiserror::Error;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::sync::{Mutex, OnceLock};
use std::collections::HashMap;


// This file contains the code for supporting custom messages in ReachPy using the native CDR type that ROS2 uses. 
#[derive(Clone)]
struct FieldSchema { 
    name: String, 
    field_type: FieldType,
}

#[derive(Clone)]
enum FieldType {
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    RosString,
    RosBytes,
    Array(Box<FieldType>),     
    Struct(String),            
    Time,                      
    Duration,                  
}

#[derive(Clone)]
struct MessageSchema {
    name: String,
    fields: Vec<FieldSchema>,
}

#[derive(Debug, Error)]
pub enum MessageError {
    #[error("Unknown message type: {0}")]
    UnknownSchema(String),
    #[error("Field type mismatch on field '{field}': expected {expected}, got {got}")]
    TypeMismatch { field: String, expected: String, got: String },
    #[error("Serialization failed: {0}")]
    SerializationError(String),
    #[error("Deserialization failed: {0}")]
    DeserializationError(String),
    #[error("Schema already registered: {0}")]
    DuplicateSchema(String),
}

static SCHEMA_REGISTRY: OnceLock<Mutex<HashMap<String, MessageSchema>>> = OnceLock::new(); 

fn get_schema_registry() -> &'static Mutex<HashMap<String, MessageSchema>> {
    SCHEMA_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_schema(schema: MessageSchema) -> Result<(), MessageError> {
    let mut registry = get_schema_registry().lock().unwrap();
    if registry.contains_key(&schema.name) {
        return Err(MessageError::DuplicateSchema(schema.name.clone()));
    }
    registry.insert(schema.name.clone(), schema.clone());
    Ok(())
}

fn get_schema(name: &str) -> Result<MessageSchema, MessageError> {
    let registry = get_schema_registry().lock().unwrap();
    registry.get(name).cloned().ok_or(MessageError::UnknownSchema(name.to_string()))
}

fn schema_exists(name: &str) -> bool {
    let registry = get_schema_registry().lock().unwrap();
    registry.contains_key(name)
}

fn unregister_schema(name: &str) -> Result<(), MessageError> {
    let mut registry = get_schema_registry().lock().unwrap();
    registry.remove(name)
        .map(|_| ())
        .ok_or(MessageError::UnknownSchema(name.to_string()))
}

fn message_to_cdr(message: &impl Message) -> Result<Vec<u8>, MessageError> {
    let mut cdr = Cdr::new();
    message.serialize(&mut cdr)?;
    Ok(cdr.to_vec())
}

fn cdr_to_message(data: &[u8]) -> Result<impl Message, MessageError> {
    let mut cdr = Cdr::new();
    cdr.from_slice(data)?;
    let message = Message::deserialize(&mut cdr)?;
    Ok(message)
} 

fn serialize(schema_name: &str, values: Vec<PyObject>) -> Result<Vec<u8>, MessageError>{
    let cdr = message_to_cdr(message)?;
    Ok(cdr.to_vec())
}

fn deserialize(schema_name: &str, data: &[u8]) -> Result<Vec<PyObject>, MessageError>{
    let mut cdr = Cdr::new();
    cdr.from_slice(data)?;
    let message = Message::deserialize(&mut cdr)?;
    Ok(message)
} 

fn register_message(name: &str, schema: MessageSchema) -> Result<(), MessageError> {
    register_schema(schema)?;
    Ok(())
} 

#[pymodule]
fn reachpy_messages(py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(register_message, m)?)?;
    m.add_function(wrap_pyfunction!(get_schema, m)?)?;
    m.add_function(wrap_pyfunction!(schema_exists, m)?)?;
    m.add_function(wrap_pyfunction!(unregister_schema, m)?)?;
    m.add_function(wrap_pyfunction!(serialize_message, m)?)?;
    m.add_function(wrap_pyfunction!(deserialize_message, m)?)?;
    Ok(())
} 