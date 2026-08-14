"""
reachpy.topic — typed pub/sub topics for ReachPy nodes.

    class Perception:
        detections: Topic[Detection] = Topic[Detection]("/detections")

        def on_start(self):
            self.detections.subscribe(self.on_detection)

        def on_detection(self, msg: Detection):
            ...

        @timer(0.5)
        def announce(self):
            self.detections.publish(Detection(x=1.0, y=2.0, label="cone"))

Backed by reachpy_runtime.PyNode's create_publisher()/create_subscription(),
which move raw bytes over Zenoh -- Topic just handles the Message<->bytes
conversion on either side. No ROS2, no rclpy, anywhere in this stack.
"""

from __future__ import annotations

from typing import Callable, Generic, Optional, TypeVar

from .message import Message

T = TypeVar("T", bound=Message)


class Topic(Generic[T]):
    """Declares a topic on a node.

    Bind the message type with subscript syntax: `Topic[Waypoint]("/wp")`.
    Each node instance gets its own live copy of the Topic (bound to that
    node) -- the class-level declaration is just a template.
    """

    _msg_type: Optional[type] = None

    def __init__(self, name: str):
        self.name = name
        self.msg_type: Optional[type] = type(self)._msg_type

        self._node = None  # the underlying reachpy_runtime.PyNode
        self._publisher = None
        self._subscribed = False

    def __class_getitem__(cls, item):
        return type(f"Topic[{getattr(item, '__name__', item)}]", (cls,), {"_msg_type": item})

    def __repr__(self) -> str:
        type_name = self.msg_type.__name__ if self.msg_type else "?"
        return f"Topic[{type_name}]({self.name!r})"

    # --- wiring, called by the @node decorator -----------------------------

    def _bind(self, raw_node) -> None:
        self._node = raw_node

    def _require_ready(self) -> type[Message]:
        if self.msg_type is None:
            raise TypeError(
                f"Topic('{self.name}') has no message type. Declare it as "
                f"Topic[YourMessage]('{self.name}')."
            )
        if self._node is None:
            raise RuntimeError(
                f"Topic('{self.name}') isn't bound to a node yet. Topics are "
                f"only usable from on_start()/on_stop() or a method called "
                f"after node construction -- not in __init__ before ReachPy "
                f"has finished wiring the node."
            )
        return self.msg_type

    # --- publish/subscribe --------------------------------------------------

    def publish(self, message: T) -> None:
        msg_type = self._require_ready()
        if not isinstance(message, msg_type):
            raise TypeError(
                f"Topic('{self.name}') expects {msg_type.__name__}, "
                f"got {type(message).__name__}"
            )
        if self._publisher is None:
            self._publisher = self._node.create_publisher(self.name)
        self._publisher.publish(message.to_bytes())

    def subscribe(self, callback: Callable[[T], None]) -> None:
        msg_type = self._require_ready()
        if self._subscribed:
            raise RuntimeError(f"Topic('{self.name}') already has a subscriber.")

        def _on_raw(raw_bytes: bytes) -> None:
            callback(msg_type.from_bytes(raw_bytes))

        self._node.create_subscription(self.name, _on_raw)
        self._subscribed = True