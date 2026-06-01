# ReachPy 🚀

**ReachPy** is a modern Python framework built to dramatically improve the Developer Experience (DX) for robotics engineers using ROS 2. 

Building robotics applications in ROS 2 shouldn't mean wrestling with boilerplate, complex build pipelines, or rigid message compilation. ReachPy aims to provide a more Pythonic, intuitive, and dynamic interface to the ROS 2 ecosystem, letting you focus on writing robot logic rather than fighting the middleware.

## ✨ Why ReachPy?

While `rclpy` provides the standard bindings for ROS 2, the workflow often requires dropping down to C++ build tools (`colcon`, `CMake`) just to define custom data structures. ReachPy is being built to bridge this gap, offering dynamic runtime features powered by a high-performance Rust backend.

## 📦 Core Components

### Dynamic Messaging (`reachpy_messages`)
At the core of ReachPy is our custom message serialization engine, written in Rust (via PyO3). 

Instead of writing `.msg` files, recompiling your workspace, and sourcing environments just to add a new field to a message, `reachpy_messages` allows you to define, serialize, and deserialize native ROS 2 Common Data Representation (CDR) bytes **on the fly**.

- **Dynamic Schema Registry:** Register complex nested types, arrays, and primitives entirely at runtime.
- **Native CDR Support:** Outputs perfectly aligned Little-Endian bytes that are 100% compatible with standard ROS 2 nodes.
- **Blazing Fast:** Offloads the heavy lifting of byte-packing to Rust, ensuring your Python nodes stay performant.

## 🚀 Quick Start (Preview)

*Note: ReachPy is currently in active development.*

```python
import reachpy
from reachpy.messages import MessageSchema, FieldSchema, FieldType

# 1. Define a ROS 2 compatible message entirely in Python
schema = MessageSchema(
    name="RobotWaypoint",
    fields=[
        FieldSchema("x", FieldType.Float64),
        FieldSchema("y", FieldType.Float64),
        FieldSchema("is_active", FieldType.Bool)
    ]
)

# 2. Register it dynamically (no CMake or colcon build required!)
reachpy.messages.register_schema("RobotWaypoint", schema)

# 3. Serialize directly to ROS 2 CDR bytes to send over the wire
cdr_payload = reachpy.messages.serialize("RobotWaypoint", [15.2, -7.8, True])
```

## 🛠️ Building from Source

ReachPy uses Rust for its performance-critical extensions. You will need Rust and Python 3.8+ installed.

```bash
# Build and install the Rust extension locally
maturin develop --release
```