import argparse
import json
import shlex
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from fixtures import Fixture
from kraai_harbor.archive import validate_bundle
from kraai_harbor.metrics import codex_usage, populate_context
from kraai_harbor.proxy import TrialProxy
from kraai_harbor.run import build_command, validate_requested_tasks
from kraai_harbor.spec import RunnerSpec


class SpecTests(Fixture):
    def test_prompt_replacement_preserves_literal_placeholders_and_shell_characters(
        self,
    ):
        spec = RunnerSpec.load(self.spec_path)
        instruction = "Keep {model} and {proxy_url} literally; $(touch /bad)\n'quoted'"
        argv = spec.arguments(
            instruction, "/workspace", "http://proxy", "provider", "/config"
        )
        self.assertEqual(argv[1], instruction)
        self.assertEqual(shlex.split(shlex.join(argv)), argv)

    def test_bundle_digest_is_verified(self):
        self.bundle.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            RunnerSpec.load(self.spec_path)

    def test_bundle_cannot_overwrite_task_files(self):
        with tarfile.open(self.bundle, "w") as bundle:
            bundle.addfile(tarfile.TarInfo("app/solution.py"))
        with self.assertRaisesRegex(ValueError, "Unexpected"):
            validate_bundle(self.bundle)

    def test_bundle_symbolic_links_cannot_escape_the_installation(self):
        with tarfile.open(self.bundle, "w") as bundle:
            item = tarfile.TarInfo("nix/store/example/root")
            item.type = tarfile.SYMTYPE
            item.linkname = "/"
            bundle.addfile(item)
        with self.assertRaisesRegex(ValueError, "symbolic link"):
            validate_bundle(self.bundle)


class MetricsTests(Fixture):
    def test_proxy_usage_is_authoritative_and_harbor_input_includes_cached_tokens(self):
        usage = {
            "total_tokens": 150,
            "input_tokens": 70,
            "cache_read_tokens": 50,
            "output_tokens": 20,
            "reasoning_tokens": 10,
        }
        (self.root / "proxy-metrics.json").write_text(
            json.dumps({"requests": 2, "usage": usage})
        )
        (self.root / "kraai-metrics.json").write_text(
            json.dumps({"schema_version": 1, "usage": dict(usage, input_tokens=700)})
        )
        context = SimpleNamespace(
            n_input_tokens=None,
            n_cache_tokens=None,
            n_output_tokens=None,
            metadata=None,
        )
        populate_context(self.root, context, self.root)
        self.assertEqual(
            (context.n_input_tokens, context.n_cache_tokens, context.n_output_tokens),
            (120, 50, 30),
        )
        self.assertEqual(
            context.metadata["kraai_eval"]["usage"], dict(usage, cache_write_tokens=0)
        )
        self.assertEqual(context.metadata["kraai_eval"]["usage_source"], "proxy")

    def test_only_fully_priced_accounting_populates_estimated_api_cost(self):
        accounting = {
            "model_requests": 1,
            "unpriced_requests": 0,
            "unrecorded_requests": 0,
            "cost_overflow": False,
            "known_estimated_cost": 1_420_000,
        }
        for changes, expected in (
            ({}, 0.00142),
            ({"known_estimated_cost": 0}, 0.0),
            ({"unpriced_requests": 1}, None),
            ({"unrecorded_requests": 1}, None),
            ({"cost_overflow": True}, None),
            ({"model_requests": 0}, None),
        ):
            with self.subTest(changes=changes):
                (self.root / "proxy-metrics.json").write_text(
                    json.dumps({"accounting": accounting | changes})
                )
                context = SimpleNamespace(cost_usd=999)
                populate_context(self.root, context, self.root)
                self.assertEqual(context.cost_usd, expected)
                if expected is not None:
                    self.assertEqual(
                        context.metadata["kraai_eval"]["cost_source"],
                        "estimated_api_equivalent",
                    )
        for failure in ({"unrecorded_requests": 1}, {"accounting_error": "failed"}):
            (self.root / "proxy-metrics.json").write_text(
                json.dumps(failure | {"accounting": accounting})
            )
            context = SimpleNamespace()
            populate_context(self.root, context, self.root)
            self.assertIsNone(context.cost_usd)

    def test_partial_proxy_usage_stays_available_without_becoming_a_complete_total(self):
        usage = {
            "total_tokens": 150,
            "input_tokens": 70,
            "cache_read_tokens": 40,
            "cache_write_tokens": 10,
            "output_tokens": 20,
            "reasoning_tokens": 10,
        }
        for samples, unrecorded, error in ((1, 0, None), (2, 1, None), (2, 0, "failed")):
            with self.subTest(samples=samples, unrecorded=unrecorded, error=error):
                (self.root / "proxy-metrics.json").write_text(
                    json.dumps(
                        {
                            "requests": 2,
                            "usage": usage,
                            "accounting_error": error,
                            "accounting": {
                                "model_requests": 2,
                                "unrecorded_requests": unrecorded,
                                "context": {"samples": samples},
                            },
                        }
                    )
                )
                (self.root / "kraai-metrics.json").write_text(
                    json.dumps({"usage": usage})
                )
                context = SimpleNamespace(n_input_tokens=999, cost_usd=999)
                populate_context(self.root, context, self.root)
                self.assertIsNone(context.n_input_tokens)
                self.assertIsNone(context.n_cache_tokens)
                self.assertIsNone(context.n_output_tokens)
                self.assertIsNone(context.cost_usd)
                self.assertEqual(context.metadata["kraai_eval"]["usage"], usage)
                self.assertFalse(context.metadata["kraai_eval"]["usage_complete"])
                self.assertEqual(context.metadata["kraai_eval"]["usage_source"], "proxy")

    def test_cache_writes_are_input_tokens_and_missing_proxy_usage_ignores_agent_logs(self):
        usage = {
            "total_tokens": 150,
            "input_tokens": 70,
            "cache_read_tokens": 40,
            "cache_write_tokens": 10,
            "output_tokens": 20,
            "reasoning_tokens": 10,
        }
        for proxy_usage, expected in ((usage, 120), (None, None)):
            with self.subTest(proxy_usage=proxy_usage):
                (self.root / "proxy-metrics.json").write_text(
                    json.dumps({"requests": 1, "usage": proxy_usage})
                )
                (self.root / "kraai-metrics.json").write_text(
                    json.dumps({"usage": usage})
                )
                context = SimpleNamespace()
                populate_context(self.root, context, self.root)
                self.assertEqual(context.n_input_tokens, expected)
                self.assertEqual(context.metadata["kraai_eval"]["usage_source"], "proxy")

    def test_codex_usage_is_normalized_without_counting_intermediate_events(self):
        path = self.root / "runner.stdout.jsonl"
        event = {
            "type": "turn.completed",
            "usage": {
                "input_tokens": 120,
                "cached_input_tokens": 50,
                "output_tokens": 30,
                "reasoning_output_tokens": 10,
            },
        }
        path.write_text(
            json.dumps(dict(event, type="item.completed"))
            + "\n"
            + json.dumps(event)
            + "\n"
        )
        self.assertEqual(
            codex_usage(path),
            {
                "total_tokens": 150,
                "input_tokens": 70,
                "cache_read_tokens": 50,
                "output_tokens": 20,
                "reasoning_tokens": 10,
            },
        )

    def test_missing_usage_remains_unknown(self):
        context = SimpleNamespace(
            n_input_tokens=None,
            n_cache_tokens=None,
            n_output_tokens=None,
            metadata=None,
        )
        populate_context(self.root, context, self.root)
        self.assertIsNone(context.n_input_tokens)
        self.assertIsNone(context.n_output_tokens)
        self.assertIsNone(context.cost_usd)


class DriverTests(Fixture):
    def test_every_requested_task_must_exist_even_when_another_task_matches(self):
        validate_requested_tasks(["real-task"], {"real-task", "other-task"})
        with self.assertRaisesRegex(ValueError, "misspelled-task"):
            validate_requested_tasks(
                ["real-task", "misspelled-task"], {"real-task", "other-task"}
            )

    def arguments(self, **changes):
        args = {
            "spec": self.spec_path,
            "oracle": False,
            "dataset": "terminal-bench@2.0",
            "job_dir": self.root / "job",
            "attempts": 2,
            "task_name": ["hello-world"],
            "full_dataset": False,
            "model": None,
            "docker_compose": [],
            "allow_agent_host": [],
        }
        return argparse.Namespace(**(args | changes))

    def test_exact_tasks_and_attempts_use_serial_native_harbor_trials(self):
        command = build_command(self.arguments())
        self.assertIn("kraai_harbor.agent:KraaiAgent", command)
        self.assertEqual(command[command.index("--n-concurrent") + 1], "1")
        self.assertEqual(command[command.index("--max-retries") + 1], "0")
        self.assertEqual(command[command.index("--n-attempts") + 1], "2")
        self.assertEqual(
            command[command.index("--include-task-name") + 1], "hello-world"
        )

    def test_task_globs_and_unpinned_datasets_are_rejected(self):
        for changes in (
            {"task_name": ["*"]},
            {"dataset": "terminal-bench"},
            {"task_name": []},
        ):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                build_command(self.arguments(**changes))

    def test_oracle_does_not_need_spec_or_proxy(self):
        command = build_command(self.arguments(spec=None, oracle=True))
        self.assertEqual(command[command.index("--agent") + 1], "oracle")
        self.assertNotIn("--agent-kwarg", command)


class ProxyTests(unittest.IsolatedAsyncioTestCase):
    async def test_per_trial_proxy_keeps_credentials_out_of_artifacts_and_flushes_usage(
        self,
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / "controller.py"
            script.write_text(
                "import json,pathlib,sys\n"
                "state=pathlib.Path(sys.argv[sys.argv.index('--state-dir')+1])\n"
                "state.mkdir(mode=0o700)\n"
                "(state/'ready.json').write_text(json.dumps({'schema_version':1,'base_url':'http://proxy','environment':{'TOKEN':'private-proxy-token'}}))\n"
                "sys.stdin.read()\n"
                "(state/'metrics.json').write_text(json.dumps({'requests':3}))\n"
                "(state/'request-accounting.json').write_text(json.dumps({'known_estimated_cost':1420000}))\n"
                "(state/'identity.json').write_text(json.dumps({'kind':'codex_subscription'}))\n"
                "(state/'proxy.events.jsonl').write_text('{}\\n')\n"
            )
            logs = root / "logs"
            proxy = TrialProxy((sys.executable, str(script)), logs)
            ready = await proxy.start()
            self.assertEqual(ready["environment"]["TOKEN"], "private-proxy-token")
            await proxy.stop()
            self.assertEqual(
                json.loads((logs / "proxy-metrics.json").read_text())["requests"], 3
            )
            self.assertTrue((logs / "proxy-identity.json").is_file())
            self.assertEqual(
                json.loads((logs / "request-accounting.json").read_text()),
                {"known_estimated_cost": 1_420_000},
            )
            self.assertFalse(proxy.directory.exists())
            self.assertNotIn(
                "private-proxy-token",
                "".join(path.read_text() for path in logs.iterdir()),
            )


if __name__ == "__main__":
    unittest.main()
