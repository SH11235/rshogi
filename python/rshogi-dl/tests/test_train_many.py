from contextlib import redirect_stderr
import io
import json
from pathlib import Path
import unittest
from unittest import mock

from rshogi_dl import train_many

from .support import artifact_tempdir


class TrainManyTest(unittest.TestCase):
    def run_jobs(self, jobs, **trainer):
        with artifact_tempdir() as temporary:
            path = Path(temporary) / "jobs.json"
            path.write_text(json.dumps(jobs), encoding="utf-8")
            errors = io.StringIO()
            with mock.patch.object(train_many, "train", **trainer) as train, redirect_stderr(errors):
                status = train_many.main([str(path)])
            return status, train.call_args_list, errors.getvalue()

    def test_runs_jobs_in_manifest_order(self):
        jobs = [["--seed", "11"], ["--seed", "23"]]
        status, calls, errors = self.run_jobs(jobs, return_value=0)
        self.assertEqual((status, calls, errors), (0, [mock.call(job) for job in jobs], ""))

    def test_rejects_malformed_job_before_training(self):
        status, calls, errors = self.run_jobs([["--seed", "11"], [23]], return_value=0)
        self.assertEqual((status, calls), (2, []))
        self.assertIn("job 2", errors)

    def test_stops_after_a_failed_trainer_job(self):
        jobs = [["--seed", "11"], ["--seed", "23"], ["--seed", "37"]]
        for failure in (2, SystemExit(2)):
            with self.subTest(failure=failure):
                status, calls, errors = self.run_jobs(jobs, side_effect=[0, failure, 0])
                self.assertEqual((status, calls), (2, [mock.call(job) for job in jobs[:2]]))
                self.assertIn("job 2 of 3", errors)
