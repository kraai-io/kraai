import json
import shlex
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from fixtures import Fixture
from harbor.models.agent.context import AgentContext
from kraai_harbor.agent import ProfileAgent


class ConfigurationTests(Fixture):
    def test_model_identity_mismatch_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "model.*match"):
            ProfileAgent(
                logs_dir=self.root / "logs",
                spec_path=str(self.spec_path),
                model_name="different",
            )

    def test_no_prompt_reframing_is_allowed(self):
        with self.assertRaisesRegex(ValueError, "original task instruction"):
            ProfileAgent(
                logs_dir=self.root / "logs",
                spec_path=str(self.spec_path),
                prompt_template_path="template.md",
            )


class AgentTests(Fixture, unittest.IsolatedAsyncioTestCase):
    async def test_install_run_and_finalize_use_the_pinned_sdk(self):
        commands = []
        uploads = {}
        usage = {
            "total_tokens": 150,
            "input_tokens": 70,
            "cache_read_tokens": 50,
            "output_tokens": 20,
            "reasoning_tokens": 10,
        }
        accounting = {
            "model_requests": 1,
            "unpriced_requests": 0,
            "unrecorded_requests": 0,
            "context": {"samples": 1},
            "cost_overflow": False,
            "known_estimated_cost": 1_420_000,
        }
        controller = self.root / "controller.py"
        controller.write_text(
            "import json,pathlib,sys\n"
            "state=pathlib.Path(sys.argv[sys.argv.index('--state-dir')+1])\n"
            "state.mkdir(mode=0o700)\n"
            "config=state/'providers.toml'\n"
            "config.write_text('proxy_token_env = \\\"TOKEN\\\"\\n')\n"
            "profiles=state/'agents.toml'\n"
            "profiles.write_text('[[profiles]]\\nid = \\\"eval-coding\\\"\\n')\n"
            "(state/'ready.json').write_text(json.dumps({'schema_version':1,'base_url':'http://proxy','environment':{'TOKEN':'ephemeral'},'provider_id':'provider','provider_config':str(config),'agent_profiles':str(profiles)}))\n"
            "sys.stdin.read()\n"
            f"(state/'metrics.json').write_text(json.dumps({{'requests':1,'usage':{usage!r},'accounting':{accounting!r}}}))\n"
            f"(state/'request-accounting.json').write_text(json.dumps({accounting!r}))\n"
            "(state/'identity.json').write_text('{}')\n"
        )
        self.spec_data["proxy_command"].append(str(controller))
        self.write_spec()

        class Environment:
            default_user = "task-user"

            async def exec(self, **kwargs):
                commands.append(kwargs)
                return SimpleNamespace(
                    return_code=0,
                    stdout="/task workspace\n"
                    if kwargs["command"].endswith("pwd -P")
                    else "",
                    stderr="",
                )

            async def upload_file(self, source, target):
                uploads[target] = Path(source).read_bytes()

        agent = ProfileAgent(
            logs_dir=self.root / "logs",
            spec_path=str(self.spec_path),
            model_name=self.spec_data["model"],
        )
        environment = Environment()
        await agent.setup(environment)
        await agent.run("the unchanged task instruction", environment, AgentContext())
        context = AgentContext()
        agent.populate_context_post_run(context)
        self.assertEqual(
            uploads["/installed-agent/kraai-runner.tar"], self.bundle.read_bytes()
        )
        self.assertEqual(
            uploads["/installed-agent/providers.toml"], b'proxy_token_env = "TOKEN"\n'
        )
        self.assertEqual(
            uploads["/installed-agent/agents.toml"],
            b'[[profiles]]\nid = "eval-coding"\n',
        )
        self.assertTrue(
            any(
                "tar -xf" in command["command"] and command["user"] == "root"
                for command in commands
            )
        )
        self.assertTrue(
            any("chown task-user" in command["command"] for command in commands)
        )
        runtime = commands[-1]
        self.assertIsNone(runtime["user"])
        self.assertIsNone(runtime["cwd"])
        self.assertEqual(
            runtime["env"],
            {
                "TOKEN": "ephemeral",
                "KRAAI_AGENT_PROFILES": "/installed-agent/agents.toml",
                "KRAAI_EVAL_METRICS_PATH": "/logs/agent/kraai-metrics.json",
            },
        )
        self.assertFalse((agent.logs_dir / "proxy-metrics.json").exists())
        self.assertTrue(
            (
                agent.logs_dir.parent / "kraai-controller" / "proxy-metrics.json"
            ).is_file()
        )
        self.assertEqual(context.n_input_tokens, 120)
        self.assertEqual(context.n_cache_tokens, 50)
        self.assertEqual(context.cost_usd, 0.00142)
        self.assertEqual(context.model_dump()["cost_usd"], 0.00142)
        self.assertEqual(
            context.metadata["kraai_eval"]["cost_source"], "estimated_api_equivalent"
        )
        self.assertEqual(
            json.loads(
                (agent.logs_dir.parent / "kraai-controller/request-accounting.json").read_text()
            ),
            accounting,
        )
        self.assertFalse((agent.logs_dir / "request-accounting.json").exists())
        self.assertEqual(context.metadata["kraai_eval"]["usage_source"], "proxy")
        self.assertNotIn(
            "ephemeral",
            "".join(path.read_text() for path in agent.logs_dir.glob("*.json")),
        )

    async def test_native_instruction_and_working_directory_are_preserved_on_failure(
        self,
    ):
        commands = []

        class Environment:
            default_user = None

            async def exec(self, **kwargs):
                commands.append(kwargs)
                if kwargs["command"].endswith("pwd -P"):
                    return SimpleNamespace(
                        return_code=0, stdout="/task workspace\n", stderr=""
                    )
                return SimpleNamespace(return_code=7, stdout="", stderr="")

        class Proxy:
            stopped = False

            def __init__(self, *_args):
                pass

            async def start(self):
                return {
                    "base_url": "http://proxy",
                    "environment": {"TOKEN": "ephemeral"},
                }

            async def stop(self):
                Proxy.stopped = True

        agent = ProfileAgent(
            logs_dir=self.root / "logs",
            spec_path=str(self.spec_path),
            model_name=self.spec_data["model"],
        )
        instruction = "literal {model}; $(exit 90) 'quoted'"
        with (
            patch("kraai_harbor.agent.TrialProxy", Proxy),
            self.assertRaises(RuntimeError),
        ):
            await agent.run(instruction, Environment(), AgentContext())
        self.assertTrue(Proxy.stopped)
        runner = commands[-1]
        argv = shlex.split(runner["command"])
        self.assertEqual(argv[argv.index("--message") + 1], instruction)
        self.assertIsNone(runner["cwd"])
        self.assertEqual(runner["env"]["TOKEN"], "ephemeral")
        self.assertNotIn("ephemeral", runner["command"])
        metrics = json.loads(
            (
                agent.logs_dir.parent / "kraai-controller" / "runner-metrics.json"
            ).read_text()
        )
        self.assertIsNotNone(metrics["execution_ms"])


if __name__ == "__main__":
    unittest.main()
