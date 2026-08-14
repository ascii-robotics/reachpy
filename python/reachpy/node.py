"""
reachpy.node — the @node decorator.

Turns a plain Python class into a live ReachPy node, backed by
reachpy_runtime.PyNode (Rust-owned Zenoh session + event loop). No rclpy,
no ROS2, no manual spin()/shutdown() boilerplate beyond calling
node.spin() to block and node.destroy_node() to clean up.

    from reachpy import node, timer, Topic, Message

    class Waypoint(Message):
        x: float
        y: float

    @node(name="detector")
    class Detector:
        waypoints: Topic[Waypoint] = Topic[Waypoint]("/waypoints")

        def on_start(self):
            print("detector up")

        @timer(0.5)
        def tick(self):
            self.waypoints.publish(Waypoint(x=1.0, y=2.0))

    d = Detector()
    d.spin()            # blocks until Ctrl+C
    d.destroy_node()     # call explicitly before the process exits --
                          # see reachpy_runtime.PyNode.destroy_node's own
                          # docstring for why this can't just be left to GC

Your own __init__ (if you define one) runs exactly as written -- it
should just set up plain attributes. Topic wiring, timers, and on_start()
all happen after it, once the underlying node is alive.
"""

from __future__ import annotations

import functools
from typing import Callable, Optional

from . import reachpy_runtime
from .topic import Topic

__all__ = ["node", "timer"]

_TIMER_ATTR = "__reachpy_timer_period__"


def timer(period_seconds: float):
    """Mark a method to be called on a repeating timer.

        @timer(0.5)
        def tick(self):
            ...
    """

    def decorator(fn: Callable) -> Callable:
        setattr(fn, _TIMER_ATTR, period_seconds)
        return fn

    return decorator


def node(cls: Optional[type] = None, *, name: Optional[str] = None):
    """Class decorator that turns `cls` into a ReachPy node.

    Instantiating the decorated class opens a live reachpy_runtime.PyNode
    (bare in `__reachpy_node__`) and wires up Topics/timers/on_start().
    Usable bare (`@node`) or with a name (`@node(name=...)`); defaults to
    the snake_cased class name.
    """

    def wrap(user_cls: type) -> type:
        node_name = name or _to_snake(user_cls.__name__)
        user_init = user_cls.__dict__.get("__init__")

        class _ReachPyNode(user_cls):
            def __init__(self, *args, **kwargs):
                # The real, Rust-owned node -- everything below is just
                # wiring Python-level conveniences on top of it.
                self.__reachpy_node__ = reachpy_runtime.PyNode(node_name)

                if user_init is not None:
                    user_init(self, *args, **kwargs)

                self._reachpy_topics = _bind_topics(self, type(self), self.__reachpy_node__)
                _wire_timers(self, user_cls, self.__reachpy_node__)

                on_start = getattr(self, "on_start", None)
                if callable(on_start):
                    on_start()

            def spin(self) -> None:
                """Blocks until interrupted (Ctrl+C). All the real work
                happens on Rust-owned background threads; this just keeps
                the process alive."""
                self.__reachpy_node__.spin()

            def destroy_node(self) -> None:
                """Stops all background workers/timers and blocks until
                they've exited. Call this explicitly before your script
                ends -- don't rely on garbage collection. See
                reachpy_runtime.PyNode.destroy_node for why."""
                on_stop = getattr(self, "on_stop", None)
                if callable(on_stop):
                    on_stop()
                self.__reachpy_node__.destroy_node()

        _ReachPyNode.__name__ = user_cls.__name__
        _ReachPyNode.__qualname__ = user_cls.__qualname__
        _ReachPyNode.__module__ = user_cls.__module__
        _ReachPyNode.__doc__ = user_cls.__doc__
        _ReachPyNode.__reachpy_node_class__ = True
        return _ReachPyNode

    return wrap if cls is None else wrap(cls)


def _bind_topics(instance, klass: type, raw_node) -> list:
    """Find Topic[...] class attributes across the MRO, give each node
    instance its own bound copy (so multiple instances of the same node
    class never share publisher/subscriber state), and shadow the class
    attribute with that per-instance copy."""
    import copy

    bound = []
    seen: set[str] = set()
    for klass_in_mro in klass.__mro__:
        for attr_name, value in list(vars(klass_in_mro).items()):
            if attr_name in seen or not isinstance(value, Topic):
                continue
            seen.add(attr_name)
            instance_topic = copy.copy(value)
            instance_topic._bind(raw_node)
            setattr(instance, attr_name, instance_topic)
            bound.append(instance_topic)
    return bound


def _wire_timers(instance, user_cls: type, raw_node) -> None:
    seen: set[str] = set()
    for klass in user_cls.__mro__:
        for attr_name, fn in list(vars(klass).items()):
            if attr_name in seen:
                continue
            period = getattr(fn, _TIMER_ATTR, None)
            if period is None:
                continue
            seen.add(attr_name)
            bound_fn = functools.partial(fn, instance)
            raw_node.create_timer(period, bound_fn)


def _to_snake(name: str) -> str:
    out = []
    for i, ch in enumerate(name):
        if ch.isupper() and i != 0:
            out.append("_")
        out.append(ch.lower())
    return "".join(out)