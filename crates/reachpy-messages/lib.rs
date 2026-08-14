use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use pyo3::exceptions::{PyKeyError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::{Mutex, OnceLock};
use thiserror::Error;

/// ReachPy Binary v1 — a single-byte format tag, no ROS2/CDR alignment
/// padding anywhere. Replaces the CDR encapsulation header this crate used
/// to write. Bump this if the wire format ever changes shape.
const FORMAT_TAG: u8 = 0x01;

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

impl FieldType {
    fn from_name(name: &str) -> Result<Self, MessageError> {
        if let Some(inner) = name
            .strip_prefix("array<")
            .and_then(|value| value.strip_suffix('>'))
        {
            return Ok(Self::Array(Box::new(Self::from_name(inner)?)));
        }
        if let Some(struct_name) = name.strip_prefix("struct:") {
            if struct_name.trim().is_empty() {
                return Err(MessageError::SchemaDefinition(
                    "Struct type must include a schema name".into(),
                ));
            }
            return Ok(Self::Struct(struct_name.trim().to_string()));
        }
        match name {
            "bool" => Ok(Self::Bool),
            "int8" => Ok(Self::Int8),
            "int16" => Ok(Self::Int16),
            "int32" => Ok(Self::Int32),
            "int64" => Ok(Self::Int64),
            "uint8" => Ok(Self::UInt8),
            "uint16" => Ok(Self::UInt16),
            "uint32" => Ok(Self::UInt32),
            "uint64" => Ok(Self::UInt64),
            "float32" => Ok(Self::Float32),
            "float64" => Ok(Self::Float64),
            "string" | "ros_string" => Ok(Self::RosString),
            "bytes" | "ros_bytes" => Ok(Self::RosBytes),
            "time" => Ok(Self::Time),
            "duration" => Ok(Self::Duration),
            other => Err(MessageError::SerializationError(format!(
                "Unsupported field type '{other}'"
            ))),
        }
    }

    fn as_name(&self) -> String {
        match self {
            Self::Bool => "bool".to_string(),
            Self::Int8 => "int8".to_string(),
            Self::Int16 => "int16".to_string(),
            Self::Int32 => "int32".to_string(),
            Self::Int64 => "int64".to_string(),
            Self::UInt8 => "uint8".to_string(),
            Self::UInt16 => "uint16".to_string(),
            Self::UInt32 => "uint32".to_string(),
            Self::UInt64 => "uint64".to_string(),
            Self::Float32 => "float32".to_string(),
            Self::Float64 => "float64".to_string(),
            Self::RosString => "string".to_string(),
            Self::RosBytes => "bytes".to_string(),
            Self::Array(inner) => format!("array<{}>", inner.as_name()),
            Self::Struct(name) => format!("struct:{name}"),
            Self::Time => "time".to_string(),
            Self::Duration => "duration".to_string(),
        }
    }
}

#[derive(Clone)]
struct MessageSchema {
    fields: Vec<FieldSchema>,
}

#[derive(Debug, Error)]
pub enum MessageError {
    #[error("Unknown custom message schema: {0}")]
    UnknownSchema(String),
    #[error("Field type mismatch on '{field}': expected {expected}, got {got}")]
    TypeMismatch {
        field: String,
        expected: String,
        got: String,
    },
    #[error("Serialization failed: {0}")]
    SerializationError(String),
    #[error("Deserialization failed: {0}")]
    DeserializationError(String),
    #[error("Schema already registered: {0}")]
    DuplicateSchema(String),
    #[error("Schema definition error: {0}")]
    SchemaDefinition(String),
}

impl From<MessageError> for PyErr {
    fn from(value: MessageError) -> Self {
        match value {
            MessageError::UnknownSchema(name) => PyKeyError::new_err(name),
            MessageError::TypeMismatch { .. }
            | MessageError::SerializationError(_)
            | MessageError::DeserializationError(_) => PyTypeError::new_err(value.to_string()),
            MessageError::DuplicateSchema(_) | MessageError::SchemaDefinition(_) => {
                PyValueError::new_err(value.to_string())
            }
        }
    }
}

static SCHEMA_REGISTRY: OnceLock<Mutex<HashMap<String, MessageSchema>>> = OnceLock::new();

fn get_schema_registry() -> &'static Mutex<HashMap<String, MessageSchema>> {
    SCHEMA_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_schema_internal(name: &str, schema: MessageSchema) -> Result<(), MessageError> {
    let mut registry = get_schema_registry()
        .lock()
        .map_err(|_| MessageError::SerializationError("Schema registry lock poisoned".into()))?;
    if registry.contains_key(name) {
        return Err(MessageError::DuplicateSchema(name.to_string()));
    }
    registry.insert(name.to_string(), schema);
    Ok(())
}

fn get_schema_internal(name: &str) -> Result<MessageSchema, MessageError> {
    let registry = get_schema_registry()
        .lock()
        .map_err(|_| MessageError::DeserializationError("Schema registry lock poisoned".into()))?;
    registry
        .get(name)
        .cloned()
        .ok_or_else(|| MessageError::UnknownSchema(name.to_string()))
}

fn parse_schema(fields: &Bound<'_, PyList>) -> Result<MessageSchema, MessageError> {
    let mut parsed_fields = Vec::with_capacity(fields.len());
    for item in fields.iter() {
        let pair = item.cast::<PyList>().ok();
        let tuple = item.cast::<PyTuple>().ok();

        let (name, field_type_name): (String, String) = if let Some(p) = pair {
            if p.len() != 2 {
                return Err(MessageError::SchemaDefinition(
                    "Each field entry must contain exactly 2 items".into(),
                ));
            }
            (
                p.get_item(0)
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?
                    .extract::<String>()
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?,
                p.get_item(1)
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?
                    .extract::<String>()
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?,
            )
        } else if let Some(t) = tuple {
            if t.len() != 2 {
                return Err(MessageError::SchemaDefinition(
                    "Each field entry must contain exactly 2 items".into(),
                ));
            }
            (
                t.get_item(0)
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?
                    .extract::<String>()
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?,
                t.get_item(1)
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?
                    .extract::<String>()
                    .map_err(|e| MessageError::SchemaDefinition(e.to_string()))?,
            )
        } else {
            return Err(MessageError::SchemaDefinition(
                "Fields must be provided as list[tuple[str, str]]".into(),
            ));
        };

        let field_type = FieldType::from_name(&field_type_name)?;
        parsed_fields.push(FieldSchema { name, field_type });
    }

    Ok(MessageSchema {
        fields: parsed_fields,
    })
}

fn type_name_for_value(value: &Bound<'_, PyAny>) -> &'static str {
    if value.extract::<bool>().is_ok() {
        "bool"
    } else if value.extract::<i64>().is_ok() {
        "int"
    } else if value.extract::<u64>().is_ok() {
        "uint"
    } else if value.extract::<f64>().is_ok() {
        "float"
    } else if value.extract::<String>().is_ok() {
        "string"
    } else if value.cast::<PyBytes>().is_ok() {
        "bytes"
    } else if value.cast::<PyList>().is_ok() || value.extract::<Vec<Py<PyAny>>>().is_ok() {
        "list"
    } else if value.cast::<PyDict>().is_ok() {
        "dict"
    } else {
        "unknown"
    }
}

// ── encode ──────────────────────────────────────────────────────────────
// No alignment padding anywhere below (that was purely a ROS2/CDR wire
// requirement) — every field is packed back-to-back at its natural width.

fn write_field_value(
    buf: &mut Vec<u8>,
    py: Python<'_>,
    field: &FieldSchema,
    value: &Bound<'_, PyAny>,
) -> Result<(), MessageError> {
    let mismatch = |expected: String| MessageError::TypeMismatch {
        field: field.name.clone(),
        expected,
        got: type_name_for_value(value).to_string(),
    };
    write_by_type(buf, py, &field.field_type, value, &field.name, &mismatch)
}

fn write_by_type(
    buf: &mut Vec<u8>,
    py: Python<'_>,
    field_type: &FieldType,
    value: &Bound<'_, PyAny>,
    field_name: &str,
    mismatch: &dyn Fn(String) -> MessageError,
) -> Result<(), MessageError> {
    match field_type {
        FieldType::Bool => {
            buf.write_u8(if value.extract::<bool>().map_err(|_| mismatch("bool".to_string()))? {
                1
            } else {
                0
            })
            .map_err(|e| MessageError::SerializationError(e.to_string()))?
        }
        FieldType::Int8 => buf
            .write_i8(value.extract::<i8>().map_err(|_| mismatch("int8".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::Int16 => buf
            .write_i16::<LittleEndian>(value.extract::<i16>().map_err(|_| mismatch("int16".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::Int32 => buf
            .write_i32::<LittleEndian>(value.extract::<i32>().map_err(|_| mismatch("int32".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::Int64 => buf
            .write_i64::<LittleEndian>(value.extract::<i64>().map_err(|_| mismatch("int64".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::UInt8 => buf
            .write_u8(value.extract::<u8>().map_err(|_| mismatch("uint8".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::UInt16 => buf
            .write_u16::<LittleEndian>(value.extract::<u16>().map_err(|_| mismatch("uint16".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::UInt32 => buf
            .write_u32::<LittleEndian>(value.extract::<u32>().map_err(|_| mismatch("uint32".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::UInt64 => buf
            .write_u64::<LittleEndian>(value.extract::<u64>().map_err(|_| mismatch("uint64".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::Float32 => buf
            .write_f32::<LittleEndian>(value.extract::<f32>().map_err(|_| mismatch("float32".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::Float64 => buf
            .write_f64::<LittleEndian>(value.extract::<f64>().map_err(|_| mismatch("float64".to_string()))?)
            .map_err(|e| MessageError::SerializationError(e.to_string()))?,
        FieldType::RosString => {
            let data = value.extract::<String>().map_err(|_| mismatch("string".to_string()))?;
            let bytes = data.as_bytes();
            buf.write_u32::<LittleEndian>(bytes.len() as u32)
                .map_err(|e| MessageError::SerializationError(e.to_string()))?;
            buf.extend_from_slice(bytes);
        }
        FieldType::RosBytes => {
            let data = value.cast::<PyBytes>().map_err(|_| mismatch("bytes".to_string()))?.as_bytes();
            buf.write_u32::<LittleEndian>(data.len() as u32)
                .map_err(|e| MessageError::SerializationError(e.to_string()))?;
            buf.extend_from_slice(data);
        }
        FieldType::Array(inner) => {
            let items = value
                .cast::<PyList>()
                .map_err(|_| mismatch(format!("array<{}>", inner.as_name())))?;
            buf.write_u32::<LittleEndian>(items.len() as u32)
                .map_err(|e| MessageError::SerializationError(e.to_string()))?;
            for item in items.iter() {
                let item_mismatch = |expected: String| MessageError::TypeMismatch {
                    field: field_name.to_string(),
                    expected,
                    got: type_name_for_value(&item).to_string(),
                };
                write_by_type(buf, py, inner, &item, field_name, &item_mismatch)?;
            }
        }
        FieldType::Struct(schema_name) => {
            let nested = value.cast::<PyDict>().map_err(|_| mismatch(format!("struct:{schema_name}")))?;
            let nested_schema = get_schema_internal(schema_name)?;
            for nested_field in &nested_schema.fields {
                let nested_value = nested
                    .get_item(&nested_field.name)
                    .map_err(|e| MessageError::SerializationError(e.to_string()))?
                    .ok_or_else(|| {
                        MessageError::SerializationError(format!(
                            "Missing nested field '{}.{}'",
                            field_name, nested_field.name
                        ))
                    })?;
                write_by_type(
                    buf,
                    py,
                    &nested_field.field_type,
                    &nested_value,
                    &format!("{field_name}.{}", nested_field.name),
                    mismatch,
                )?;
            }
        }
        FieldType::Time | FieldType::Duration => {
            let dict = value.cast::<PyDict>().map_err(|_| mismatch(field_type.as_name()))?;
            let sec = dict
                .get_item("sec")
                .map_err(|e| MessageError::SerializationError(e.to_string()))?
                .ok_or_else(|| MessageError::SerializationError(format!("Missing field '{field_name}.sec'")))?
                .extract::<i32>()
                .map_err(|_| mismatch(format!("{} with int32 sec", field_type.as_name())))?;
            buf.write_i32::<LittleEndian>(sec)
                .map_err(|e| MessageError::SerializationError(e.to_string()))?;
            let nanosec = dict
                .get_item("nanosec")
                .map_err(|e| MessageError::SerializationError(e.to_string()))?
                .ok_or_else(|| MessageError::SerializationError(format!("Missing field '{field_name}.nanosec'")))?
                .extract::<u32>()
                .map_err(|_| mismatch(format!("{} with uint32 nanosec", field_type.as_name())))?;
            buf.write_u32::<LittleEndian>(nanosec)
                .map_err(|e| MessageError::SerializationError(e.to_string()))?;
        }
    }

    Ok(())
}

// ── decode ──────────────────────────────────────────────────────────────

fn read_field_value<'py>(
    py: Python<'py>,
    reader: &mut Cursor<&[u8]>,
    field: &FieldSchema,
) -> Result<Py<PyAny>, MessageError> {
    read_by_type(py, reader, &field.field_type, &field.name)
}

fn read_by_type<'py>(
    py: Python<'py>,
    reader: &mut Cursor<&[u8]>,
    field_type: &FieldType,
    field_name: &str,
) -> Result<Py<PyAny>, MessageError> {
    let io_err = |e: std::io::Error| MessageError::DeserializationError(e.to_string());
    let value = match field_type {
        FieldType::Bool => (reader.read_u8().map_err(io_err)? != 0).into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .to_owned()
            .into_any()
            .unbind(),
        FieldType::Int8 => reader.read_i8().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::Int16 => reader.read_i16::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::Int32 => reader.read_i32::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::Int64 => reader.read_i64::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::UInt8 => reader.read_u8().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::UInt16 => reader.read_u16::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::UInt32 => reader.read_u32::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::UInt64 => reader.read_u64::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::Float32 => reader.read_f32::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::Float64 => reader.read_f64::<LittleEndian>().map_err(io_err)?.into_pyobject(py)
            .map_err(|e| MessageError::DeserializationError(e.to_string()))?
            .into_any().unbind(),
        FieldType::RosString => {
            let len = reader.read_u32::<LittleEndian>().map_err(io_err)? as usize;
            let mut bytes = vec![0u8; len];
            reader.read_exact(&mut bytes).map_err(io_err)?;
            let s = String::from_utf8(bytes).map_err(|e| MessageError::DeserializationError(e.to_string()))?;
            s.into_pyobject(py)
                .map_err(|e| MessageError::DeserializationError(e.to_string()))?
                .into_any().unbind()
        }
        FieldType::RosBytes => {
            let len = reader.read_u32::<LittleEndian>().map_err(io_err)? as usize;
            let mut bytes = vec![0u8; len];
            reader.read_exact(&mut bytes).map_err(io_err)?;
            PyBytes::new(py, &bytes).into_any().unbind()
        }
        FieldType::Array(inner) => {
            let len = reader.read_u32::<LittleEndian>().map_err(io_err)? as usize;
            let out = PyList::empty(py);
            for _ in 0..len {
                out.append(read_by_type(py, reader, inner, field_name)?)
                    .map_err(|e| MessageError::DeserializationError(e.to_string()))?;
            }
            out.into_any().unbind()
        }
        FieldType::Struct(schema_name) => {
            let schema = get_schema_internal(schema_name)?;
            let out = PyDict::new(py);
            for nested_field in &schema.fields {
                let nested_value = read_by_type(
                    py,
                    reader,
                    &nested_field.field_type,
                    &format!("{field_name}.{}", nested_field.name),
                )?;
                out.set_item(&nested_field.name, nested_value)
                    .map_err(|e| MessageError::DeserializationError(e.to_string()))?;
            }
            out.into_any().unbind()
        }
        FieldType::Time | FieldType::Duration => {
            let sec = reader.read_i32::<LittleEndian>().map_err(io_err)?;
            let nanosec = reader.read_u32::<LittleEndian>().map_err(io_err)?;
            let out = PyDict::new(py);
            out.set_item("sec", sec).map_err(|e| MessageError::DeserializationError(e.to_string()))?;
            out.set_item("nanosec", nanosec).map_err(|e| MessageError::DeserializationError(e.to_string()))?;
            out.into_any().unbind()
        }
    };

    Ok(value)
}

// ── PyO3 surface — unchanged from the CDR version except serialize()/
// deserialize()'s header handling ──────────────────────────────────────

#[pyfunction]
fn register_schema(name: &str, fields: &Bound<'_, PyList>) -> PyResult<()> {
    let schema = parse_schema(fields)?;
    register_schema_internal(name, schema)?;
    Ok(())
}

#[pyfunction]
fn get_schema(py: Python<'_>, name: &str) -> PyResult<Py<PyList>> {
    let schema = get_schema_internal(name)?;
    let out = PyList::empty(py);
    for field in schema.fields {
        out.append((field.name, field.field_type.as_name()))?;
    }
    Ok(out.unbind())
}

#[pyfunction]
fn schema_exists(name: &str) -> bool {
    if let Ok(registry) = get_schema_registry().lock() {
        registry.contains_key(name)
    } else {
        false
    }
}

#[pyfunction]
fn unregister_schema(name: &str) -> PyResult<()> {
    let mut registry = get_schema_registry()
        .lock()
        .map_err(|_| PyValueError::new_err("Schema registry lock poisoned"))?;
    registry
        .remove(name)
        .ok_or_else(|| MessageError::UnknownSchema(name.to_string()))?;
    Ok(())
}

#[pyfunction]
fn serialize(py: Python<'_>, schema_name: &str, values: &Bound<'_, PyDict>) -> PyResult<Py<PyBytes>> {
    let schema = get_schema_internal(schema_name)?;
    let mut out = Vec::with_capacity(128);
    out.push(FORMAT_TAG);
    for field in &schema.fields {
        let value = values
            .get_item(&field.name)?
            .ok_or_else(|| MessageError::SerializationError(format!("Missing field '{}'", field.name)))?;
        write_field_value(&mut out, py, field, &value)?;
    }
    Ok(PyBytes::new(py, &out).unbind())
}

#[pyfunction]
fn deserialize(py: Python<'_>, schema_name: &str, data: &[u8]) -> PyResult<Py<PyDict>> {
    if data.is_empty() {
        return Err(PyValueError::new_err("Empty payload (missing format tag)"));
    }
    if data[0] != FORMAT_TAG {
        return Err(PyValueError::new_err(format!(
            "Unsupported format tag {:#04x}. Expected {:#04x} (ReachPy Binary v1). \
             This payload may be old CDR-encoded data from before the encoder swap.",
            data[0], FORMAT_TAG
        )));
    }

    let schema = get_schema_internal(schema_name)?;
    let out = PyDict::new(py);
    let mut cursor = Cursor::new(data);
    cursor.set_position(1);

    for field in &schema.fields {
        let value = read_field_value(py, &mut cursor, field)?;
        out.set_item(&field.name, value)?;
    }

    Ok(out.unbind())
}

#[pymodule]
pub fn _reachpy_messages(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(register_schema, m)?)?;
    m.add_function(wrap_pyfunction!(get_schema, m)?)?;
    m.add_function(wrap_pyfunction!(schema_exists, m)?)?;
    m.add_function(wrap_pyfunction!(unregister_schema, m)?)?;
    m.add_function(wrap_pyfunction!(serialize, m)?)?;
    m.add_function(wrap_pyfunction!(deserialize, m)?)?;
    Ok(())
}