"""Probe control flow with a fake clock and API; never touches real input."""
import unittest

import latency_pointer_probe as probe


class Clock:
    def __init__(self):
        self.now = 0.0
        self.overshoot = 0.0
        self.after_sleep = lambda: None

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds + self.overshoot
        self.overshoot = 0.0
        self.after_sleep()


class Api:
    def __init__(self, clock):
        self.clock = clock
        self.position = (10, 20)
        self.reason = None
        self.moves = []
        self.after_move = lambda: None

    def stop_reason(self):
        return self.reason

    def cursor(self):
        return self.position

    def client_rect(self):
        return (100, 200, 800, 600)

    def target_at(self, point):
        return True

    def move(self, point):
        self.moves.append((self.clock(), point))
        self.position = point
        self.after_move()


class PointerProbeTests(unittest.TestCase):
    def test_focus_or_controls_stop_all_further_injection(self):
        for reason in ("focus_lost", "target_lost", "controls_held"):
            with self.subTest(reason=reason):
                clock = Clock()
                api = Api(clock)

                def take_over():
                    api.reason = reason

                api.after_move = take_over
                report = probe.run_probe(api, 1, 120, clock, clock.sleep)
                self.assertEqual(len(api.moves), 1)
                self.assertEqual(report["moves"], 1)
                self.assertEqual(report["stop_reason"], reason)
                self.assertFalse(report["cursor_restored"])

    def test_late_wake_skips_missed_ticks_and_stops_at_the_deadline(self):
        clock = Clock()
        clock.overshoot = .36
        api = Api(clock)
        report = probe.run_probe(api, .5, 10, clock, clock.sleep)
        self.assertEqual([round(t, 3) for t, _ in api.moves[:-1]], [0, .37, .4])
        self.assertEqual(report["moves"], 3)
        self.assertEqual(report["skipped_ticks"], 2)
        self.assertTrue(report["cursor_restored"])
        self.assertEqual(api.moves[-1][1], (10, 20))
        self.assertTrue(all(t < .5 for t, _ in api.moves[:-1]))
        self.assertEqual(report["intervals"]["count"], 2)

    def test_cursor_feedback_mismatch_is_not_a_takeover_proof(self):
        clock = Clock()
        api = Api(clock)
        api.after_move = lambda: setattr(api, "position", (0, 0))
        report = probe.run_probe(api, .1, 30, clock, clock.sleep)
        self.assertEqual(report["stop_reason"], "completed")
        self.assertGreater(report["moves"], 1)
        self.assertFalse(report["cursor_restored"])

    def test_obscured_or_confined_path_never_injects(self):
        clock = Clock()
        api = Api(clock)
        api.target_at = lambda point: False
        report = probe.run_probe(api, 1, 120, clock, clock.sleep)
        self.assertEqual(api.moves, [])
        self.assertEqual(report["stop_reason"], "target_obscured_or_constrained")

    def test_control_held_at_completion_prevents_cursor_restoration(self):
        clock = Clock()
        api = Api(clock)

        def take_over_at_deadline():
            if clock() >= .1:
                api.reason = "controls_held"

        clock.after_sleep = take_over_at_deadline
        report = probe.run_probe(api, .1, 10, clock, clock.sleep)
        self.assertEqual(report["stop_reason"], "controls_held")
        self.assertEqual(len(api.moves), 1)
        self.assertFalse(report["cursor_restored"])


if __name__ == "__main__":
    unittest.main()
