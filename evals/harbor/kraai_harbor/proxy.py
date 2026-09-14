import asyncio
import json
import shutil
import tempfile
from pathlib import Path


class TrialProxy:
    def __init__(self, command: tuple[str, ...], logs_dir: Path):
        self.command = command
        self.logs_dir = logs_dir
        self._directory = tempfile.TemporaryDirectory(prefix="kraai-harbor-proxy-")
        self.directory = Path(self._directory.name) / "proxy"
        self.process = None
        self._output = None
        self.ready = None

    async def start(self) -> dict:
        self._output = (Path(self._directory.name) / "controller.log").open("wb")
        self.process = await asyncio.create_subprocess_exec(
            *self.command,
            "--state-dir",
            str(self.directory),
            stdin=asyncio.subprocess.PIPE,
            stdout=self._output,
            stderr=self._output,
        )
        try:
            async with asyncio.timeout(60):
                while True:
                    if self.process.returncode is not None:
                        raise RuntimeError(
                            f"Evaluation proxy exited before readiness: {self.process.returncode}"
                        )
                    path = self.directory / "ready.json"
                    if path.is_file():
                        try:
                            ready = json.loads(path.read_text())
                        except json.JSONDecodeError:
                            await asyncio.sleep(0.05)
                            continue
                        if ready.get("schema_version") != 1:
                            raise ValueError(
                                "Unsupported evaluation proxy readiness version"
                            )
                        if not isinstance(ready.get("base_url"), str) or not isinstance(
                            ready.get("environment"), dict
                        ):
                            raise ValueError(
                                "Invalid evaluation proxy readiness record"
                            )
                        if any(
                            not isinstance(k, str) or not isinstance(v, str)
                            for k, v in ready["environment"].items()
                        ):
                            raise ValueError("Invalid evaluation proxy environment")
                        self.ready = ready
                        return ready
                    await asyncio.sleep(0.05)
        except BaseException:
            await self.stop()
            raise

    async def stop(self) -> None:
        try:
            if self.process is not None:
                if self.process.stdin is not None:
                    self.process.stdin.close()
                try:
                    await asyncio.wait_for(self.process.wait(), timeout=30)
                except TimeoutError:
                    self.process.terminate()
                    try:
                        await asyncio.wait_for(self.process.wait(), timeout=5)
                    except TimeoutError:
                        self.process.kill()
                        await self.process.wait()
                self.logs_dir.mkdir(parents=True, exist_ok=True)
                for source, target in (
                    ("metrics.json", "proxy-metrics.json"),
                    ("request-accounting.json", "request-accounting.json"),
                    ("identity.json", "proxy-identity.json"),
                    ("proxy.events.jsonl", "proxy.events.jsonl"),
                ):
                    path = self.directory / source
                    if path.is_file():
                        shutil.copyfile(path, self.logs_dir / target)
        finally:
            if self._output is not None:
                self._output.close()
                self._output = None
            self._directory.cleanup()
