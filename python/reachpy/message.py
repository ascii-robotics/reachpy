"""
reachpy.message — declarative message definitions for ReachPy.

Define custom, ROS2-CDR-compatible messages as plain Python classes with
type hints. ReachPy derives a schema at class-definition time and registers
it with the Rust-backed reachpy_messages engine — no .msg files, no colcon
build, no codegen step.

    class Waypoint(Message):
        x: float
        y: float
        is_active: bool

    wp = Waypoint(x=1.0, y=2.0, is_active=True)
    payload = wp.to_bytes()          # -> ROS2 CDR bytes
    wp2 = Waypoint.from_bytes(payload)

Nesting works — a Message can contain another Message:

    class Path(Message):
        waypoints: list[Waypoint]
        name: str

Field widths default to sensible types (int -> int32, float -> float64).
Use Annotated to be explicit when the wire format matters:

    from typing import Annotated

    class Reading(Message):
        value: Annotated[int, "uint8"]
"""

from __future__ import annotations

import itertools
import typing
from dataclasses import dataclass
from typing import Any, ClassVar, Optional

from . import _reachpy_messages as _msgs

__all__ = ["Message"]

_schema_name_counter = itertools.count()

# Python type -> reachpy_messages field type name.
# Override per-field with Annotated[T, "reachpy_type_name"].
_DEFAULT_TYPE_MAP: dict[type, str] = {
    bool: "bool",
    int: "int32",
    float: "float64",
    str: "string",
    bytes: "bytes",
}


def _resolve_field_type(py_type: Any) -> str:
    """Map a Python annotation to a reachpy_messages field-type name."""
    origin = typing.get_origin(py_type)

    # Annotated[int, "uint8"] — explicit override wins
    if getattr(py_type, "__metadata__", None):
        base = py_type.__args__[0]
        for meta in py_type.__metadata__:
            if isinstance(meta, str):
                return meta
        return _resolve_field_type(base)

    # list[T] / List[T] -> array (matches reachpy_messages' "array<T>", not "list<T>")
    if origin in (list, typing.List):
        (inner,) = typing.get_args(py_type)
        return f"array<{_resolve_field_type(inner)}>"

    # Nested Message type -> struct
    if isinstance(py_type, type) and issubclass(py_type, Message):
        return f"struct:{py_type.__schema_name__}"

    if py_type in _DEFAULT_TYPE_MAP:
        return _DEFAULT_TYPE_MAP[py_type]

    raise TypeError(
        f"ReachPy Message: unsupported field type {py_type!r}. "
        f"Use bool, int, float, str, bytes, list[...], a nested Message "
        f"subclass, or Annotated[T, 'reachpy_type_name'] to be explicit "
        f"(e.g. Annotated[int, 'uint8'])."
    )


class Message:
    """Base class for user-defined ReachPy messages.

    Subclass and declare fields as type-hinted class attributes. Each
    subclass gets its own CDR schema, derived and registered once with
    the Rust message engine at class-creation time. Subclasses become
    dataclasses automatically — you get __init__/__repr__/__eq__ for free.
    """

    __schema_name__: ClassVar[str]
    __field_names__: ClassVar[tuple[str, ...]]

    #: Optional ROS2 type string (e.g. "sensor_msgs/msg/Image") for
    #: interop with topics that standard ROS2 nodes expect a specific
    #: type on. Leave unset for ReachPy-only dynamic topics.
    ros_type: ClassVar[Optional[str]] = None

    def __init_subclass__(cls, schema_name: str | None = None, **kwargs):
        super().__init_subclass__(**kwargs)

        hints = typing.get_type_hints(cls, include_extras=True)
        own_annotations = cls.__dict__.get("__annotations__", {})
        field_names = tuple(name for name in own_annotations if name in hints)

        name = schema_name or f"{cls.__module__}.{cls.__qualname__}#{next(_schema_name_counter)}"
        schema_fields = [(fname, _resolve_field_type(hints[fname])) for fname in field_names]

        if not _msgs.schema_exists(name):
            _msgs.register_schema(name, schema_fields)

        cls.__schema_name__ = name
        cls.__field_names__ = field_names

        # Make the class a dataclass so __init__/__repr__/__eq__ come for
        # free without fighting any methods the user defines themselves.
        dataclass(eq=True, repr=True)(cls)

    def to_bytes(self) -> bytes:
        """Serialize this message to ROS2 CDR bytes."""
        values = {}
        for name in self.__field_names__:
            value = getattr(self, name)
            values[name] = _to_wire(value)
        return _msgs.serialize(self.__schema_name__, values)

    @classmethod
    def from_bytes(cls, data: bytes) -> "Message":
        """Deserialize ROS2 CDR bytes into an instance of this message."""
        raw = _msgs.deserialize(cls.__schema_name__, data)
        hints = typing.get_type_hints(cls, include_extras=True)
        kwargs = {name: _from_wire(raw[name], hints[name]) for name in cls.__field_names__}
        return cls(**kwargs)

    def to_dict(self) -> dict:
        return {name: getattr(self, name) for name in self.__field_names__}


def _to_wire(value: Any) -> Any:
    if isinstance(value, Message):
        return value.to_dict()
    if isinstance(value, list):
        return [_to_wire(v) for v in value]
    return value


def _from_wire(value: Any, py_type: Any) -> Any:
    origin = typing.get_origin(py_type)
    if getattr(py_type, "__metadata__", None):
        py_type = py_type.__args__[0]
        origin = typing.get_origin(py_type)

    if isinstance(py_type, type) and issubclass(py_type, Message) and isinstance(value, dict):
        nested_hints = typing.get_type_hints(py_type, include_extras=True)
        kwargs = {k: _from_wire(v, nested_hints[k]) for k, v in value.items()}
        return py_type(**kwargs)

    if origin in (list, typing.List) and isinstance(value, list):
        (inner_type,) = typing.get_args(py_type)
        return [_from_wire(v, inner_type) for v in value]

    return value