import json
import unittest

from backend.main import parse_response


class ParseResponseTests(unittest.TestCase):
    def test_accepts_valid_control_fields(self):
        parsed = parse_response(json.dumps({
            "reply": "Hello!",
            "emotion": "happy",
            "motion": "nod",
            "music": {"action": "search", "query": "Renai Circulation"},
        }))

        self.assertEqual(parsed["emotion"], "happy")
        self.assertEqual(parsed["motion"], "nod")
        self.assertEqual(parsed["music"]["action"], "search")

    def test_neutralizes_unrecognized_controls(self):
        parsed = parse_response(json.dumps({
            "reply": "No unsafe controls.",
            "emotion": "malicious",
            "motion": "launch",
            "music": {"action": "run_program", "query": "calc"},
        }))

        self.assertEqual(parsed["emotion"], "neutral")
        self.assertIsNone(parsed["motion"])
        self.assertIsNone(parsed["music"])

    def test_extracts_json_from_model_fences(self):
        parsed = parse_response('```json\n{"reply":"Hi","emotion":"shy"}\n```')

        self.assertEqual(parsed["reply"], "Hi")
        self.assertEqual(parsed["emotion"], "shy")

    def test_requires_reply(self):
        with self.assertRaises(ValueError):
            parse_response('{"emotion":"happy"}')


if __name__ == "__main__":
    unittest.main()
