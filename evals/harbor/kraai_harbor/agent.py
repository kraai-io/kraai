import json
import shlex
import time
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

from kraai_harbor.archive import validate_bundle
from kraai_harbor.metrics import populate_context
from kraai_harbor.proxy import TrialProxy
from kraai_harbor.spec import RunnerSpec


class ProfileAgent(BaseInstalledAgent):
    def __init__(self, *args, spec_path: str, **kwargs):
        if kwargs.get("prompt_template_path") is not None:
            raise ValueError("Public benchmarks must use the original task instruction")
        self.spec = RunnerSpec.load(spec_path)
        super().__init__(*args, version=self.spec.bundle_sha256, **kwargs)
        if self.model_name is not None and self.model_name != self.spec.model:
            raise ValueError("Harbor model and runner spec model must match")
        if self.mcp_servers or self.skills_dir:
            raise ValueError(
                "Profile adapter does not support task MCP servers or skills"
            )

    @staticmethod
    def name() -> str:
        return "kraai-profile"

    async def install(self, environment: BaseEnvironment) -> None:
        validate_bundle(self.spec.runner_bundle)
        bundle_path = "/installed-agent/kraai-runner.tar"
        await environment.upload_file(self.spec.runner_bundle, bundle_path)
        await self.exec_as_root(
            environment,
            command=f"tar -xf {shlex.quote(bundle_path)} -C / --no-same-owner && rm {shlex.quote(bundle_path)}",
        )
        await self.exec_as_agent(
            environment, command=f"test -x {shlex.quote(self.spec.runner_path)}"
        )

    async def run(
        self, instruction: str, environment: BaseEnvironment, context: AgentContext
    ) -> None:
        controller_dir = self.logs_dir.parent / "kraai-controller"
        proxy = TrialProxy(self.spec.proxy_command, controller_dir)
        started = None
        try:
            ready = await proxy.start()
            remote_config = ""
            if ready.get("provider_config"):
                remote_config = "/installed-agent/providers.toml"
                await self._upload_config_text(
                    environment,
                    content=Path(ready["provider_config"]).read_text(),
                    remote_path=remote_config,
                    filename="providers.toml",
                )
            remote_profiles = None
            if ready.get("agent_profiles"):
                remote_profiles = "/installed-agent/agents.toml"
                await self._upload_config_text(
                    environment,
                    content=Path(ready["agent_profiles"]).read_text(),
                    remote_path=remote_profiles,
                    filename="agents.toml",
                )
            cwd_result = await self.exec_as_agent(environment, command="pwd -P")
            workspace = cwd_result.stdout.rstrip("\n")
            argv = [
                self.spec.runner_path,
                *self.spec.arguments(
                    instruction,
                    workspace,
                    ready["base_url"],
                    ready.get("provider_id", ""),
                    remote_config,
                ),
            ]
            logs = self.environment_logs_dir
            environment_vars = dict(ready["environment"])
            if remote_profiles is not None:
                environment_vars["KRAAI_AGENT_PROFILES"] = remote_profiles
            environment_vars["KRAAI_EVAL_METRICS_PATH"] = str(
                logs / "kraai-metrics.json"
            )
            command = (
                f"{shlex.join(argv)} > {shlex.quote(str(logs / 'runner.stdout.jsonl'))} "
                f"2> {shlex.quote(str(logs / 'runner.stderr.log'))}"
            )
            started = time.monotonic_ns()
            await self.exec_as_agent(environment, command=command, env=environment_vars)
        finally:
            finished = time.monotonic_ns()
            try:
                await proxy.stop()
            finally:
                controller_dir.mkdir(parents=True, exist_ok=True)
                (controller_dir / "runner-metrics.json").write_text(
                    json.dumps(
                        {
                            "schema_version": 1,
                            "harness": self.spec.harness,
                            "model": self.spec.model,
                            "bundle_sha256": self.spec.bundle_sha256,
                            "execution_ms": (finished - started) / 1_000_000
                            if started is not None
                            else None,
                            "wall_clock_reliable": False,
                        },
                        indent=2,
                    )
                    + "\n"
                )

    def populate_context_post_run(self, context: AgentContext) -> None:
        populate_context(
            self.logs_dir, context, self.logs_dir.parent / "kraai-controller"
        )


class KraaiAgent(ProfileAgent):
    @staticmethod
    def name() -> str:
        return "kraai"
