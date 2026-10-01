import asyncio
import shlex
import subprocess
import shutil
from pathlib import PurePosixPath
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from fixtures import Fixture
from kraai_harbor.agent import KraaiAgent, ProfileAgent


class AgentSkillsTests(Fixture):
    def agent(self, kind, skills):
        return kind(
            logs_dir=self.root / "logs",
            spec_path=str(self.spec_path),
            skills_dir=str(skills),
        )

    def test_kraai_registers_task_skills_and_supporting_files(self):
        source = self.root / "task skills '$(not-a-command)"
        skill = source / "review"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text("task instructions")
        (skill / ".support").write_text("supporting file")
        destination = self.root / "installed skills"
        agent = self.agent(KraaiAgent, source)
        agent.exec_as_root = AsyncMock()
        environment = SimpleNamespace(upload_file=AsyncMock())

        async def execute(environment, command):
            if command.startswith("test -x "):
                return
            command = command.replace(
                '"$HOME/.agents/skills"', shlex.quote(str(destination))
            )
            subprocess.run(["bash", "-c", command], check=True, capture_output=True)

        agent.exec_as_agent = execute
        asyncio.run(agent.install(environment))
        self.assertEqual((destination / "review/SKILL.md").read_text(), "task instructions")
        self.assertEqual((destination / "review/.support").read_text(), "supporting file")
        agent.skills_dir = str(destination)
        asyncio.run(agent.install(environment))
        self.assertEqual((destination / "review/.support").read_text(), "supporting file")
        agent.skills_dir = str(self.root / "missing")
        with self.assertRaises(subprocess.CalledProcessError):
            asyncio.run(agent.install(environment))

    def test_generic_profiles_do_not_silently_ignore_task_skills(self):
        with self.assertRaisesRegex(ValueError, "task skills"):
            self.agent(ProfileAgent, self.root / "skills")


class ExecutionRecordsTests(Fixture):
    def setUp(self):
        super().setUp()
        self.home = self.root / "container home"
        self.data = self.home / ".kraai/data"
        self.logs = self.root / "agent logs '$(not-a-command)"
        self.agent = KraaiAgent(
            logs_dir=self.logs,
            environment_logs_dir=PurePosixPath(str(self.logs)),
            spec_path=str(self.spec_path),
        )

        async def execute(environment, command):
            for suffix in ("/executions", ""):
                command = command.replace(
                    f'"$HOME/.kraai/data{suffix}"',
                    shlex.quote(str(self.data) + suffix),
                )
            subprocess.run(["sh", "-c", command], check=True, capture_output=True)

        self.agent.exec_as_agent = execute

    def test_records_survive_interruption_and_container_home_removal(self):
        async def interrupted(instruction, environment, context):
            execution = self.data / "executions/first"
            execution.mkdir()
            (execution / "source.nu").write_text("print hello")
            (execution / "stdout.bin").write_bytes(b"hello\n")
            (execution / "stderr.bin").write_bytes(b"diagnostic\n")
            (execution / "record.json").write_text('{"phase":"running"}')
            raise asyncio.CancelledError()

        with patch.object(ProfileAgent, "run", side_effect=interrupted):
            with self.assertRaises(asyncio.CancelledError):
                asyncio.run(self.agent.run("task", object(), object()))
        with patch.object(ProfileAgent, "run", new_callable=AsyncMock) as run:
            asyncio.run(self.agent.run("task", object(), object()))
            run.assert_awaited_once()
        shutil.rmtree(self.home)
        execution = self.logs / "script-executions/first"
        self.assertEqual((execution / "source.nu").read_text(), "print hello")
        self.assertEqual((execution / "stdout.bin").read_bytes(), b"hello\n")
        self.assertEqual((execution / "stderr.bin").read_bytes(), b"diagnostic\n")
        self.assertEqual((execution / "record.json").read_text(), '{"phase":"running"}')

    def test_existing_execution_directory_is_not_replaced_or_silently_ignored(self):
        existing = self.data / "executions"
        existing.mkdir(parents=True)
        (existing / "saved").write_text("keep")
        with patch.object(ProfileAgent, "run", new_callable=AsyncMock) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                asyncio.run(self.agent.run("task", object(), object()))
            run.assert_not_awaited()
        self.assertEqual((existing / "saved").read_text(), "keep")
