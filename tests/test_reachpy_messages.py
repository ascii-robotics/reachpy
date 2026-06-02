import unittest

from reachpy import reachpy_messages as rm


class ReachpyMessagesTests(unittest.TestCase):
    def setUp(self) -> None:
        for schema in [
            "Pose2D",
            "Waypoint",
            "Telemetry",
            "Timeline",
        ]:
            if rm.schema_exists(schema):
                rm.unregister_schema(schema)

    def test_nested_struct_roundtrip(self) -> None:
        rm.register_schema(
            "Pose2D",
            [
                ("x", "float64"),
                ("y", "float64"),
            ],
        )
        rm.register_schema(
            "Waypoint",
            [
                ("pose", "struct:Pose2D"),
                ("active", "bool"),
                ("label", "string"),
            ],
        )

        payload = {
            "pose": {"x": 1.25, "y": -9.5},
            "active": True,
            "label": "dock",
        }
        encoded = rm.serialize("Waypoint", payload)
        decoded = rm.deserialize("Waypoint", encoded)
        self.assertEqual(decoded, payload)

    def test_array_roundtrip(self) -> None:
        rm.register_schema(
            "Telemetry",
            [
                ("samples", "array<float64>"),
                ("blob", "bytes"),
            ],
        )

        payload = {
            "samples": [0.5, 1.5, 2.5],
            "blob": b"\x01\x02\x03",
        }
        encoded = rm.serialize("Telemetry", payload)
        decoded = rm.deserialize("Telemetry", encoded)
        self.assertEqual(decoded["samples"], payload["samples"])
        self.assertEqual(decoded["blob"], payload["blob"])

    def test_time_and_duration_roundtrip(self) -> None:
        rm.register_schema(
            "Timeline",
            [
                ("stamp", "time"),
                ("timeout", "duration"),
            ],
        )

        payload = {
            "stamp": {"sec": 42, "nanosec": 123456789},
            "timeout": {"sec": -1, "nanosec": 9000},
        }
        encoded = rm.serialize("Timeline", payload)
        decoded = rm.deserialize("Timeline", encoded)
        self.assertEqual(decoded, payload)


if __name__ == "__main__":
    unittest.main()
