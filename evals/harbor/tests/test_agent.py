import asyncio
import shlex
import subprocess
from types import SimpleNamespace
from unittest.mock import AsyncMock

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
